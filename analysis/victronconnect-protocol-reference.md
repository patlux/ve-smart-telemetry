# VictronConnect protocol reference

Scope: static reverse engineering of VictronConnect Android `com.victronenergy.victronconnect` version `6.33` / version code `7000715` for lawful interoperability research.

Trust note: the analyzed artifact was acquired from an APKPure XAPK mirror via `apkeep`; treat exact hashes/version behavior as lower-trust until re-verified against a user-owned device or Play-authenticated split APK set.

Primary evidence:

- APK/XAPK provenance: `raw/current/PROVENANCE.md`
- JADX output: `decompiled/jadx/`
- apktool output: `decompiled/apktool/base/`
- native libraries: `decompiled/native/lib/armeabi-v7a/`
- generated symbols/strings/disassembly: `analysis/generated/`
- earlier notes: `analysis/bluetooth-protocol-findings.md`

## 1. High-level architecture

VictronConnect is a Qt 6 native app. Android Java code mainly starts Qt, manages permissions, handles background scan/readout triggers, and bridges data to widgets / Android Auto / notifications.

Important native libraries:

| Path | Role |
|---|---|
| `decompiled/native/lib/armeabi-v7a/libVictronConnect_armeabi-v7a.so` | Main VictronConnect app and protocol logic |
| `decompiled/native/lib/armeabi-v7a/libQtBluetoothLocal_armeabi-v7a.so` | Qt Android Bluetooth support |
| `decompiled/native/lib/armeabi-v7a/libcrypto_3.so` | OpenSSL crypto support, relevant to protected advertisement analysis |
| `decompiled/native/lib/armeabi-v7a/libssl_3.so` | TLS support |

Important Java bridge files:

| Path | Role |
|---|---|
| `decompiled/jadx/com.victronenergy.victronconnect/sources/com/victronenergy/victronconnect/PlatformHelper.java` | Main `QtActivity`, permissions, intents, USB, Bluetooth/location state |
| `decompiled/jadx/com.victronenergy.victronconnect/sources/com/victronenergy/victronconnect/PlatformHelperService.java` | Background `QtService`, native data/readout bridge |
| `decompiled/jadx/com.victronenergy.victronconnect/sources/com/victronenergy/victronconnect/BleScanManager.java` | PendingIntent BLE scanning and WorkManager readout trigger |
| `decompiled/jadx/com.victronenergy.victronconnect/sources/com/victronenergy/victronconnect/PairingHelper.java` | PIN pairing helper, `PAIRING_REQUEST` and bond-state receiver |
| `decompiled/jadx/com.victronenergy.victronconnect/sources/com/victronenergy/victronconnect/StartPlatformServiceWorker.java` | Starts/binds the background Qt service for readout |
| `decompiled/jadx/com.victronenergy.victronconnect/sources/org/qtproject/p005qt/android/bluetooth/QtBluetoothLE.java` | Qt BLE scan/connect/GATT read/write callback bridge |

Native symbol clusters:

| Symbol family | Observed role |
|---|---|
| `VeBleInterface::*` | BLE scan/discovery/storage/connection orchestration |
| `VeSmartDevice::*` | Smart-device advertisement matching, connection, VE.Smart routing |
| `VeService::*` | Base Victron pairing/device-info/PIN/PUK/DFU-start service |
| `VeSmartService::*` | Main VE.Smart control/data/CBOR protocol |
| `DfuService::*` | Modern Victron DFU protocol |
| `Legacy::DfuService::*` | Legacy Nordic-style DFU protocol |
| `BleServiceBase::*` | Shared GATT state and queued write machinery |
| `Updater::*`, `Legacy::Updater::*` | Firmware update state machines using DFU services |

## 2. Android manifest surface

Package/version:

| Field | Value |
|---|---|
| Package | `com.victronenergy.victronconnect` |
| Version | `6.33` |
| Version code | `7000715` |
| Min SDK | `28` |
| Target SDK | `36` |
| Split configs | `config.armeabi_v7a`, `config.mdpi` |

Key permissions:

| Permission | Notes |
|---|---|
| `android.permission.BLUETOOTH` | Legacy Bluetooth |
| `android.permission.BLUETOOTH_ADMIN` | Legacy Bluetooth admin |
| `android.permission.ACCESS_FINE_LOCATION` | Max SDK 30; legacy BLE scan location gate |
| `android.permission.BLUETOOTH_SCAN` | Android 12+, `neverForLocation` flag |
| `android.permission.BLUETOOTH_CONNECT` | Android 12+ connect/bond/GATT permission |
| `android.permission.INTERNET` | VRM/API/update/MQTT/Qt network capability |
| `android.permission.ACCESS_NETWORK_STATE` | Network-state bridge |
| `android.permission.CAMERA` | ML Kit QR/OCR flows |
| `android.permission.POST_NOTIFICATIONS` | Alarm/readout notification surface |
| `android.permission.RECEIVE_BOOT_COMPLETED` | Background/service/readout startup surface |

Exported or important components:

| Component | Exported | Notes |
|---|---:|---|
| `com.victronenergy.victronconnect.PlatformHelper` | yes | Main Qt activity; launcher, file/content import, `victron-connect://*`, `ve3.nl` links |
| `com.victronenergy.victronconnect.PlatformHelperService` | yes | Background Qt service in `:victronconnect_background`; native data/scan methods |
| `com.victronenergy.victronconnect.CarService` | yes | Android Auto CarAppService; code uses `HostValidator.ALLOW_ALL_HOSTS_VALIDATOR` |
| `com.victronenergy.victronconnect.BleScanManager$BleScanReceiver` | yes | Handles BLE scan/readout/restart actions |
| `com.victronenergy.victronconnect.VictronConnectWidgetProvider` | yes | Widget update/readout trigger surface |
| `com.victronenergy.victronconnect.AlarmNotificationCenter` | yes | Alarm/update notification receiver |
| `com.victronenergy.victronconnect.WidgetConfigActivity` | yes | App-widget configuration activity |
| `androidx.core.content.FileProvider` | no | Authority `com.victronenergy.victronconnect.share`; grants URIs from app-controlled paths |

No explicit manifest attributes found in the decoded manifest for:

- `android:debuggable`
- `usesCleartextTraffic`
- `networkSecurityConfig`
- `requestLegacyExternalStorage`

`allowBackup` is not explicitly disabled in the decoded manifest; confirm effective installed state with `dumpsys package` if needed.

## 3. BLE discovery and advertisement matching

Java-side background scan:

- `BleScanManager.ensureBleScanRunning()` starts `BluetoothLeScanner.startScan(...)` with a `PendingIntent`.
- Filter includes manufacturer data company id `737` (`0x02e1`), data byte `0x10`, mask `0xfe`.
- Scan settings use low-power mode and first-match behavior.
- Matching scan events are delivered to `BleScanManager$BleScanReceiver` and can enqueue a one-time WorkManager readout.

Qt-side BLE scan bridge:

- `QtBluetoothLE.java` also builds scan filters.
- It includes manufacturer id `737` / data `0x10` / mask `0xfe`.
- It also filters Victron-like service UUID masks.
- Results are forwarded to native code via callbacks such as `leScanResult(...)`.

Native advertisement matching:

`VeSmartDevice::validAdvertisement(QBluetoothDeviceInfo const&)` accepts a device when either condition is true:

1. manufacturer data company id `0x02e1` and first payload byte `0x10`, or
2. advertised service UUID is one of the VE.Smart service UUIDs below.

VE.Smart service UUIDs:

| Symbol | UUID |
|---|---|
| `VeSmartService::ServiceUuid0` | `306b0001-b081-4037-83dc-e59fcc3cdfd0` |
| `VeSmartService::ServiceUuid1` | `306b0001-b081-4037-83dc-e59fcc3cdfd1` |

## 4. GATT services and characteristics

### 4.1 Base Victron service: `VeService`

| Symbol | UUID | Observed role | Object field |
|---|---|---|---|
| `VeService::ServiceUuid` | `97580001-ddf1-48be-b73e-182664615d8e` | service UUID | n/a |
| `VeService::DeviceInfoUuid` | `97580002-ddf1-48be-b73e-182664615d8e` | device-info characteristic | `this+0x28` |
| `VeService::PinCodeUuid` | `97580003-ddf1-48be-b73e-182664615d8e` | PIN-code characteristic | `this+0x34` |
| `VeService::StartDfuUuid` | `97580004-ddf1-48be-b73e-182664615d8e` | start-DFU characteristic | `this+0x64` |
| `VeService::PukCode24Uuid` | `97580006-ddf1-48be-b73e-182664615d8e` | PUK-code characteristic | `this+0x4c` |

Additional fields observed:

| Field | Observed role |
|---|---|
| `this+0x40` | PIN CCC descriptor `0x2902` |
| `this+0x58` | PUK CCC descriptor `0x2902` |

Write behavior from `VeService::characteristicWritten(...)`:

| Written characteristic | Observed behavior |
|---|---|
| PIN code | Recognized as PIN-code flow |
| PUK code | Recognized as PUK-code flow |
| Start DFU | Compares written value with `QByteArray(1, 0)` and emits/calls `VeService::startDfuWritten(bool)` |
| Unknown characteristic | Error/state path through `BleServiceBase::setState(...)` |

### 4.2 VE.Smart service: `VeSmartService`

Service UUIDs:

| Symbol | UUID |
|---|---|
| `VeSmartService::ServiceUuid0` | `306b0001-b081-4037-83dc-e59fcc3cdfd0` |
| `VeSmartService::ServiceUuid1` | `306b0001-b081-4037-83dc-e59fcc3cdfd1` |

Characteristic UUID derivation:

`VeSmartService::getUuid(unsigned char index)` obtains the connected service UUID, converts it to RFC4122 bytes, increments byte offset `3` by `index`, then reconstructs a `QUuid`.

For service `306b0001-b081-4037-83dc-e59fcc3cdfd0`:

| Index | Derived UUID | Observed role | Object field |
|---:|---|---|---|
| `1` | `306b0002-b081-4037-83dc-e59fcc3cdfd0` | Control characteristic | `this+0x2c` |
| `2` | `306b0003-b081-4037-83dc-e59fcc3cdfd0` | LastData characteristic | `this+0x38` |
| `3` | `306b0004-b081-4037-83dc-e59fcc3cdfd0` | Data characteristic | `this+0x44` |

For service `306b0001-b081-4037-83dc-e59fcc3cdfd1`:

| Index | Derived UUID | Observed role | Object field |
|---:|---|---|---|
| `1` | `306b0002-b081-4037-83dc-e59fcc3cdfd1` | Control characteristic | `this+0x2c` |
| `2` | `306b0003-b081-4037-83dc-e59fcc3cdfd1` | LastData characteristic | `this+0x38` |
| `3` | `306b0004-b081-4037-83dc-e59fcc3cdfd1` | Data characteristic | `this+0x44` |

Descriptor fields:

| Field | Observed role |
|---|---|
| `this+0x50` | Control CCCD descriptor |
| `this+0x5c` | LastData CCCD descriptor |
| `this+0x68` | Data CCCD descriptor |

Observed setup sequence:

1. discover/validate Control, LastData, Data characteristics
2. require notify/indicate-capable properties
3. write Control CCCD value `0100`
4. write LastData CCCD value `0100`
5. write Data CCCD value `0100`
6. read Control characteristic

### 4.3 Modern DFU service: `DfuService`

| Symbol | UUID | Observed role | Object field |
|---|---|---|---|
| `DfuService::ServiceUuid` | `68c10001-b17f-4d3a-a290-34ad6499937c` | service UUID | n/a |
| `DfuService::ControlUuid` | `68c10002-b17f-4d3a-a290-34ad6499937c` | control characteristic | `this+0x28` |
| `DfuService::DataUuid` | `68c10003-b17f-4d3a-a290-34ad6499937c` | data characteristic | `this+0x38` |

Setup:

1. validate Control and Data characteristics
2. verify Control has notify/indicate support
3. get CCCD descriptor `0x2902`
4. connect descriptor/characteristic handlers
5. write CCCD value `0100`

### 4.4 Legacy DFU service: `Legacy::DfuService`

| Symbol | UUID | Observed role | Object field |
|---|---|---|---|
| `Legacy::DfuService::ServiceUuid` | `00001530-1212-efde-1523-785feabcd123` | service UUID | n/a |
| `Legacy::DfuService::ControlUuid` | `00001531-1212-efde-1523-785feabcd123` | control characteristic | `this+0x28` |
| `Legacy::DfuService::PacketUuid` | `00001532-1212-efde-1523-785feabcd123` | packet/data characteristic | `this+0x38` |

Setup mirrors modern DFU: validate control/packet, require notify/indicate support, obtain CCCD `0x2902`, then write `0100`.

## 5. VE.Smart control characteristic protocol

All control parsing/writing observed in native code uses `QDataStream` with byte order value `1`, consistent with little-endian in the recovered write paths.

### 5.1 Control characteristic read / negotiation

`VeSmartService::characteristicRead(...)` calls `VeSmartService::processControlData(QByteArray const&)` for Control characteristic reads.

Observed state fields in `processControlData()`:

| Field | Inferred role |
|---|---|
| `this+0x74` | negotiated/protocol byte from control data |
| `this+0x76` | two-byte field from control data |
| `this+0x78` | write/CBOR payload size limit used by `writeCbor()` |
| `this+0x79` | one-byte field from control data |
| `this+0x7a` | free chunk count / window from `0xf9` notifications |
| `this+0x7b` | accumulated ready-to-receive credit counter |
| `this+0x7c` | chunk size / max ATT related value; clamped to at least `0x14` / 20 |
| `this+0x80` | free-chunk timer |
| `this+0x88` | keep-alive/timer-related field |
| `this+0x90` | outstanding/keep-alive counter; `-1` after keep-alive response |
| `this+0x94` | queued outbound chunks, `QList<QByteArray>` |
| `this+0xac` | accumulated incoming CBOR buffer |

Initial negotiation behavior:

- rejects too-short control data through diagnostics around `control characteristic data length for Opcode::ReadyToReceive < 2`
- reads the fields above
- clamps `this+0x7c` to at least 20
- sends `writeCborChunkSize(0x80, 0xff)`, which writes control bytes `fa 80 ff`
- resets ready credit state and calls `writeReadyToReceive(0x80)`

### 5.2 Control opcodes

| Opcode / bytes | Direction | Observed behavior | Confidence |
|---|---|---|---|
| `f7 ...` | peripheral -> app | Error path; reads one or two error bytes and emits `VeSmartService::Error(...)` | high |
| `f8` | peripheral -> app | Clear/reset accumulated CBOR buffer at `this+0xac` | high |
| `f9 <n>` | both | Peripheral notification updates free-chunk count; app write reports ready-to-receive credit | high |
| `fa <a> <b>` | app -> peripheral | Chunk-size / CBOR-size negotiation via `writeCborChunkSize(a,b)` | high |

`writeReadyToReceive(unsigned char)` details:

- accumulates received chunk count in `this+0x7b`
- once the counter reaches `0x41` / 65, writes `[0xf9, accumulated]` to Control
- resets `this+0x7b` after writing

`characteristicChanged(...)` handling of incoming `0xf9`:

- reads the byte after the opcode
- updates `this+0x7a` free-chunk count
- stops the free-chunk timer at `this+0x80` when chunks are available
- drains queued chunks from `this+0x94` via `writeChunkToStack()`

## 6. VE.Smart CBOR/data protocol

Data and LastData notifications are combined into a CBOR byte stream.

Observed receive behavior:

1. Data/LastData notifications append payload bytes to accumulated buffer `this+0xac`.
2. LastData indicates the accumulated stream should be parsed.
3. The parser reads concatenated CBOR values.
4. First CBOR value is an unsigned opcode.
5. Subsequent CBOR values are opcode-specific parameters.

### 6.1 Outgoing CBOR opcodes

| Opcode | Function | Parameters | Notes |
|---:|---|---|---|
| `0x01` | `getDevices()` | none | Requests device list |
| `0x03` | `subscribe(instance)` | `instance:uint16` | If `instance == 0`, also starts keep-alive flow |
| `0x04` | `unsubscribe(instance)` | `instance:uint16` | Unsubscribe |
| `0x05` | `getValues(instance, vregs)` | `instance:uint16`, list of `uint16` VREG ids | `getValue()` wraps this with one VREG |
| `0x06` | `setValues(instance, pairs)` | `instance:uint16`, list of `(uint16 vreg, byteString value)` pairs | `setValue()` wraps this with one pair |
| `0x0a` | `getPathList(instance)` | `instance:uint16` | Requests compressed path list |
| `0x0b` | `getPathValues(instance, pathIndexes)` | `instance:uint16`, list of `int` path indexes | `getPathValue()` wraps this |
| `0x0c` | `setPathValues(instance, pairs)` | `instance:uint16`, list of `(int pathIndex, QVariant value)` pairs | `setPathValue()` wraps this |

Keep-alive:

| Method | Encoded action |
|---|---|
| `sendKeepAlive()` | calls `setValue(0, 0x0093, QByteArray{0x10, 0x27})` |

`10 27` is little-endian `0x2710` / decimal `10000`.

### 6.2 Incoming CBOR opcodes

| Opcode | Name / signal | Parameters | Observed behavior |
|---:|---|---|---|
| `0x02` | DeviceList | array of unsigned pairs | Builds `VeSmartService::DeviceListItem` list and emits `deviceListChanged()` |
| `0x07` | Response | `instance:uint`, `opcode:uint`, `response:int` | Emits `Response(unsigned short, Opcodes, Responses)` |
| `0x08` | Value | `instance:uint`, `vreg:uint`, `data:byteString` | Emits `itemValue(instance, vreg, data)`; special keep-alive handling for instance `0`, vreg `0x93` |
| `0x09` | ValueResponse | `instance:uint`, `vreg:uint`, `response:int` | Emits `valueResponse(instance, vreg, response)`; keep-alive response stops timer and sets counter to `-1` |
| `0x0d` | PathList | `instance:uint`, `compressedPathList:byteString` | `qUncompress()`, convert to `QString`, split, emit `PathList(instance, paths)` |
| `0x0e` | NewPath | `instance:uint`, `pathIndex:int`, `path:text/string` | Emits `NewPath(instance, pathIndex, path)` |
| `0x0f` | PathValue | `instance:uint`, `pathIndex:int`, `value:QVariant` | Emits `PathValue(instance, pathIndex, value)` |
| `0x10` | PathValueResponse | `instance:uint`, `pathIndex:int`, `response:int` | Emits `PathResponse(instance, pathIndex, response)` |

Unknown/default ranges observed:

- `0x03`-`0x06` on receive enter unknown/default handling.
- `0x0a`-`0x0c` on receive enter unknown/default handling.

### 6.3 Chunking and write queue

`writeCbor(QByteArray const&)`:

- rejects zero-length data
- rejects data longer than negotiated payload limit at `this+0x78`
- queues when no free chunks are available
- combines adjacent buffered payloads when possible
- calls `writeChunkToStack()` when free chunk window permits

`writeChunkToStack(QByteArray const&)`:

- splits queued data according to the negotiated chunk size at `this+0x7c`
- writes through the VE.Smart Data/LastData characteristics
- exact characteristic alternation / final-chunk rule still needs live BLE confirmation

Important strings/diagnostics observed in native output:

- `No more free cbor chunks for data`
- `Added to existing buffered chunk`
- `Appending new cbor chunk to buffered data`
- `Writing Cbor string`
- `Writing to data:`
- `Writing to lastData:`
- `skipping remainder`
- `Received unknown data opcode`

## 7. Modern DFU protocol

Native class: `DfuService`

Byte order: little-endian in recovered write/parse paths.

### 7.1 Commands written by the app

| Method | Control/Data target | Encoded payload | Notes |
|---|---|---|---|
| `startUpdate(unsigned char, unsigned int)` | Control | `01 <u8> <u32le>` | Starts update with mode/type and size/flags field |
| `sendData(QByteArray const&)` | Data | raw payload | Firmware chunk data |
| `validate(unsigned short)` | Control | `02 <u16le>` | Validate image/checksum context |
| `activate(unsigned int)` | Control | `03 <u32le>` | Activate image/context |
| `run()` | Control | `04` | Run firmware/app |
| `reset()` | Control | `05` | Reset target |

### 7.2 Notifications from peripheral

First byte is notification opcode, second byte is status. Too-short payloads trigger `DfuService::Error::LengthError`.

| Opcode | Signal / meaning | Extra fields |
|---:|---|---|
| `0x00` | `general(Status)` | none observed |
| `0x10` | `startNotify(Status, unsigned char, unsigned char)` | one or two extra bytes; second defaults to `0x14` if missing |
| `0x11` | `dataNotify(Status, unsigned int)` | `uint32le` value |
| `0x12` | `validateNotify(Status)` | none observed |
| `0x13` | `activateNotify(Status)` | none observed |
| `0x14` | `runNotify(Status)` | none observed |
| `0x15` | `resetNotify(Status)` | none observed |

### 7.3 Status and error enums

Recovered from Qt meta-object data.

`DfuService::Status`:

| Value | Name |
|---:|---|
| `0` | `Succes` |
| `1` | `OpcodeNotSupported` |
| `2` | `RequestedEncryptionNotSupported` |
| `3` | `DataSizeExceedsLimit` |
| `4` | `FlashError` |
| `5` | `CrcError` |
| `6` | `DataLengthError` |
| `7` | `NotAllowed` |

`DfuService::Error`:

| Value | Name |
|---:|---|
| `0` | `Unknown` |
| `1` | `CharacteristicInvalid` |
| `2` | `NoNotify` |
| `3` | `ServiceError` |
| `4` | `LengthError` |
| `5` | `Unexpected` |

Note: `Succes` is the recovered spelling from the binary metadata.

## 8. Legacy DFU protocol

Native class: `Legacy::DfuService`

This resembles Nordic legacy DFU over control + packet characteristics.

Byte order: little-endian in recovered write/parse paths.

### 8.1 Commands written by the app

| Method | Target | Encoded payload | Notes |
|---|---|---|---|
| `startDfu(softdeviceSize, bootloaderSize, applicationSize)` | Control then Packet | Control: `01 <imageType>`; Packet: `<u32le softdeviceSize><u32le bootloaderSize><u32le applicationSize>` | `imageType` bitfield: `0x01`, `0x02`, `0x04` for nonzero sizes |
| `sendInitParams(unsigned short)` | Control / Packet / Control | `02 00`, then init packet, then `02 01` | Init packet includes `-1`, `-1`, `1`, `0x5a`, and argument |
| `sendPacketRequestNotify(unsigned short)` | Control | `08 <u16le>` | Packet receipt notification interval |
| `sendImageHeader()` | Control | `03` | Begins image send |
| `sendData(QByteArray const&)` | Packet | raw payload | Length must be `< 0x15`; practical max 20 bytes |
| `validate()` | Control | `04` | Validate image |
| `activate()` | Control | `05` | Activate/run image |

### 8.2 Notifications from peripheral

| First byte | Meaning | Extra fields / behavior |
|---:|---|---|
| `0x11` | Packet receipt notification | reads `uint32le`, emits `packetNotify(unsigned int)` |
| `0x10` | Response Code | next byte is request opcode, next byte is status |

Response Code request-opcode dispatch:

| Request opcode | Signal |
|---:|---|
| `0x01` | `startDfuNotify(Status)` |
| `0x02` | `initPacketNotify(Status)` |
| `0x03` | `imageSendNotify(Status)` |
| `0x04` | `validateNotify(Status)` |

Incoming length `<= 2` triggers `Legacy::DfuService::Error::LengthError`.

### 8.3 Status and error enums

`Legacy::DfuService::Status`:

| Value | Name |
|---:|---|
| `0` | `Reserved` |
| `1` | `Succes` |
| `2` | `InvalidState` |
| `3` | `NotSupported` |
| `4` | `SizeExceeds` |
| `5` | `CrcError` |
| `6` | `OperationFailed` |

`Legacy::DfuService::Error`:

| Value | Name |
|---:|---|
| `0` | `Unknown` |
| `1` | `CharacteristicInvalid` |
| `2` | `NoNotify` |
| `3` | `ServiceError` |
| `4` | `LengthError` |
| `5` | `Unexpected` |

## 9. Pairing, PIN/PUK, and protected advertisements

Java pairing flow:

- `PairingHelper.register()` installs high-priority receivers for `PAIRING_REQUEST` and `BOND_STATE_CHANGED`.
- `PairingHelper.setPinForDevice(mac, pin)` stores PINs by MAC.
- On `PAIRING_REQUEST`, if variant is PIN-code and MAC is known, it calls `BluetoothDevice.setPin(...)` and aborts the broadcast on success.
- On bond success/failure, it removes the stored PIN.

Native/JNI evidence:

| Symbol/string | Meaning |
|---|---|
| `PlatformHelper::assignBlePinToMac(QString, QString)` | Native flow assigns PIN to Java pairing helper |
| `PlatformHelperAndroid::assignBlePinToMac(QString, QString)` | Android platform wrapper for the above |
| `setPinForDevice` | Java helper method name referenced from native strings |
| `setPukCode` | PUK flow exists in native/app code |
| `PUK CODE NOT VALID` | PUK validation diagnostic |
| `******** GENERATE DYNAMIC KEY` | Dynamic key generation diagnostic |

Protected advertisement/key storage evidence:

| Symbol/string | Meaning |
|---|---|
| `ProductDb::getStoredAdvertisementKey(...)` | Retrieves stored advertisement key |
| `ProductDb::storeAdvertisementKey(...)` | Stores advertisement key |
| `ProductDb::deleteAdvertisementKey(...)` | Deletes advertisement key |
| `VeBleInterface::getStoredAdvertisementKey(...)` | BLE-interface wrapper |
| `VeBleInterface::storeAdvertisementKey(...)` | BLE-interface wrapper |
| `VeBleInterface::deleteAdvertisementKey(...)` | BLE-interface wrapper |
| `BleAdvertisementKey` | Qt/meta-type string |
| `ExtraManufData::BaseTypes::EncryptedRecord<...>::Reader` | Encrypted manufacturer-data reader templates |

Observed shapes:

- advertisement keys appear to be 16-byte values
- stored against 6-byte addresses / MAC-like identifiers
- encrypted advertisement reader callbacks operate on 16-byte arrays

No keys, PINs, PUKs, or private credentials were extracted or recorded.

## 10. Java/native background readout flow

Observed static flow:

```text
BleScanManager.ensureBleScanRunning()
  -> BluetoothLeScanner.startScan(... PendingIntent ...)
  -> Android delivers ACTION_BLE_SCAN to BleScanReceiver
  -> BleScanManager.onScanMatch()
  -> BleScanManager.triggerReadout()
  -> WorkManager unique work "vc_instantreadout"
  -> StartPlatformServiceWorker.doWork()
  -> bind PlatformHelperService
  -> PlatformHelperService starts Qt native service
  -> native service performs readout
  -> PlatformHelper.instantReadoutDeviceChanged(deviceId, deviceJson)
  -> package-scoped ACTION_DEVICE_UPDATED broadcast
  -> SharedStorageReceiver / widget / notifications / Android Auto consume JSON
```

Relevant Java/native bridge methods:

| Method / symbol | Direction | Role |
|---|---|---|
| `PlatformHelperService.startScan()` | Java -> native | Restart scan/readout from service/Auto UI |
| `PlatformHelperService.sendAllData()` | Java -> native | Request all device JSON |
| `PlatformHelperService.sendDeviceData()` | Java -> native | Request one device JSON |
| `PlatformHelperService.getBitmap()` | Java -> native | Request product/device icon bitmap |
| `PlatformHelper.instantReadoutDeviceChanged(...)` | native -> Java | Broadcast device update JSON |
| `UrlHandler.openCustomUrl(...)` | Java -> native | Handle `victron-connect://`, `http`, `https` app links |
| `UrlHandler.openUri(...)` | Java -> native | Handle file/content imports after Java-side URI processing |

## 11. Internet/backend endpoints observed statically

This section separates internet backends from Bluetooth/local/LAN protocols.

Victron-owned static strings:

| Domain/path | Probable purpose | Evidence area |
|---|---|---|
| `https://vrmapi.victronenergy.com/v2` | VRM API base | native strings, `MqttController::*` symbols |
| `/accesstokens/create` | VRM access-token creation | native strings/symbols |
| `/accesstokens/` | VRM token management | native strings/symbols |
| `/installations/` | VRM installation data | native strings/symbols |
| `https://vrm.victronenergy.com/victron-connect-login` | VRM browser sign-in | native strings, `MqttController::vrmSignIn()` |
| `mqtt-rpc.victronenergy.com` | MQTT RPC broker | native strings, `VenusGateway::*`, `MqttRpcClientQt::*` |
| `/remoteFirmwares.json` | firmware catalog | native strings, `FirmwareManager::*` |
| `/firmwareDownload` | firmware download | native strings, `FirmwareRequest::*` |
| `/firmwares/changelog` | release notes/changelog | native strings, `FirmwareRequest::*` |
| `/other-firmware/evcs` | EVCS/other firmware | native strings |
| `https://ve3.nl/qvc`, `/vcrp`, `/vccp`, `/ps/` | shortlinks/deeplinks/help | manifest and native strings |

Shared/cross-platform update strings present in Android native library:

| URL | Notes |
|---|---|
| `https://updates.victronenergy.com/feeds/VictronConnect/windows/w10/version.txt` | Windows self-update string; Android runtime use not proven |
| `https://updates.victronenergy.com/feeds/VictronConnect/windows/w10/VictronConnectInstaller.exe` | Windows installer string; Android runtime use not proven |

Third-party SDK endpoints:

| Endpoint | Source | Notes |
|---|---|---|
| `firebaselogging.googleapis.com` | Google datatransport code | Google logging endpoint |
| `firebaselogging-pa.googleapis.com` | Google datatransport code | Google legacy logging endpoint |
| `visionkit-pa.googleapis.com` | Google ML Kit / VisionKit code | OCR/vision service host |

No Victron-owned crash-reporting, telemetry, or license-validation endpoint was identified by static search. Runtime capture is still needed to prove actual network use.

## 12. Screenshot UI coverage and live-capture plan

A screenshot parity pass is documented in `analysis/screenshot-ui-coverage.md`. It covers the visible original-app fields:

- Status tab: solar voltage/current/power, battery voltage/current/state, charger-off help, load-output state/current/power, energy/yield values.
- Trends/history view: `Last 30 days`, `Detailed`, date/month axis labels, `Lifetime total`, `Since reset`.
- Static QML anchors: `PageSolarCharger`, `VBusItemsSolarCharger`, `DeviceOffReasons`, `PageSolarChargerHistory`, `PageGraphsSolarCharger`, `TrendsSolarCharger`, `BroadcastDataMppt`.
- Candidate VBus paths: `/Pv/V`, `/Pv/I`, `/Yield/Power`, `/Dc/0/Voltage`, `/Dc/0/Current`, `/State`, `/DeviceOffReason`, `/Load/State`, `/Load/I`, `/Load/V`, `/Yield/System`, `/Yield/User`, `/History/Daily/`.

The concrete UI field -> path/VREG -> BLE frame mapping is still candidate-only until confirmed by static `vregs.json` extraction and runtime BLE capture.

High-value runtime checks:

1. Re-acquire the app from a user-owned device or Play-authenticated source.
2. Verify base/split signatures with `apksigner verify --verbose --print-certs`.
3. Install the same split set on a test device.
4. Capture logcat during scan, pairing, readout, and DFU setup.
5. Capture BLE traffic where lawful and technically possible.
6. Record actual services, characteristics, descriptors, properties, and CCCD writes.
7. Confirm ATT MTU negotiation and effective chunk sizes.
8. Confirm VE.Smart Data vs LastData chunk alternation/finalization rule.
9. Trigger `getDevices`, `subscribe`, `getValue`, `getPathList`, and keep-alive flows.
10. Compare observed CBOR payloads against the opcode tables above.
11. Test PIN/PUK flow with an owned device only; never record secrets in the repository.
12. Exercise firmware catalog/download without flashing unless explicitly intended and safe.

Suggested capture table:

| Step | Expected observation | Status |
|---|---|---|
| Scan | manufacturer id `0x02e1`, payload first byte `0x10`, or service UUID `306b0001-...` | observed live for `Solar Charger` |
| Connect | GATT services include VE.Smart service suffix `dfd0` | observed live |
| VE.Smart setup | CCCD notifications on Control, LastData, Data | observed via Bleak start-notify; exact CCCD ATT write not captured |
| Control read | control characteristic read before CBOR exchange | observed: `00040001de4a00` |
| Negotiation | write `fa80ff`, then `f980`; device sends control `f901` | observed live |
| Device list | app sends CBOR opcode `0x01`; peripheral responds with opcode `0x02` | observed: `029f000001000301ff` |
| Subscribe | app sends opcode `0x03` with instance | observed: `0303`, response-like `07000300` |
| Value read | app sends opcode `0x05`; peripheral responds with `0x08` value records | observed live |
| Path API | app sends `0x0a`/`0x0b` path requests | rejected/unavailable on tested device; no `PathList` received |
| History fallback | device pushes/returns history/trend VREG blocks | observed: `0x104f`, `0x1050`, `0xec20`, `0x2001`, `0x2007`, `0x2008`, `0x200b`, `0x2013`, `0x2027` |
| Keep-alive | app writes VREG `0x0093` value `10 27` on instance `0` | todo |
| DFU start | base service StartDfu write behavior confirmed | todo |
| Modern DFU | service `68c10001-...`, CCCD `0100`, control opcodes | todo |
| Legacy DFU | service `00001530-...`, packet max 20 bytes | todo |
| Screenshot Status fields | confirm PV/battery/load paths and VREGs for visible values | partly observed via VREG fallback/live reader |
| Screenshot Trends fields | confirm `/Yield/System`, `/Yield/User`, `/History/Daily/` and trend VREGs | VREG history blocks observed; exact field layout pending |
| Off reasons | map `/DeviceOffReason` numeric values to `DeviceOffReasons` QML strings | todo |
| Device name | confirm `/CustomName`, `/Description2`, `DeviceInfoUuid`, or `deviceJson.customName` source | todo |

Live retry notes (`2026-06-03`): `getPathList(instance=3)` (`0a03`) and `getPathValues(instance=3, ...)` did not yield `0x0d`/`0x0f`; the device emitted response-like frames with response code `2`. The practical history path for this charger is therefore VREG fallback for now. `scripts/read-victron-history.py` now falls back to observed history/trend VREGs and writes `mode: "vreg-fallback"` JSON under `analysis/generated/runtime/`.

## 13. Remaining unknowns

| Area | Unknown |
|---|---|
| Acquisition trust | Need device/Play-source artifact comparison |
| Signature | `apksigner` verification not yet run |
| ABI parity | Only `armeabi-v7a` split analyzed |
| VE.Smart enum names | `Responses` and `Errors` symbolic names are incomplete |
| Control field names | Several `processControlData()` object fields are behavior-inferred only |
| Chunking | Exact Data/LastData alternation and final-chunk rule require live capture |
| Protected advertisements | Encrypted manufacturer-data format and decrypt callbacks need deeper tracing |
| PIN/PUK flow | Exact native call chain for dynamic key generation needs disassembly/runtime trace |
| Firmware URLs | Some firmware path fragments need call-site/base URL reconstruction |
| Runtime networking | Static endpoint presence does not prove runtime use |

## 14. Useful generated artifacts

| File | Use |
|---|---|
| `analysis/generated/libVictronConnect.dynamic-symbols.demangled.txt` | Demangled dynamic symbols and object addresses |
| `analysis/generated/libVictronConnect.strings.txt` | Native string anchors, URLs, diagnostics |
| `analysis/generated/protocol-string-offsets.txt` | UUID/string offsets |
| `analysis/generated/thumb-protocol-disassembly.txt` | Thumb disassembly extracts |
| `analysis/generated/vesmartservice-symbols.txt` | VE.Smart symbol list |
| `analysis/generated/vesmartservice-write-methods.txt` | VE.Smart write helper disassembly/anchors |
| `analysis/generated/veservice-actions.txt` | Base service write/action anchors |
| `analysis/generated/veservice-bleservicebase-symbols.txt` | `VeService` / `BleServiceBase` symbols |
| `analysis/generated/vebleinterface-symbols.txt` | BLE interface symbols |
| `analysis/generated/vesmartdevice-symbols.txt` | Smart-device symbols |

## 15. Safety notes

- Do not commit APKs, native dumps, credentials, PINs, PUKs, advertisement keys, or captured private device data.
- Keep captured traffic sanitized and scoped to owned devices.
- Treat firmware update paths as destructive until proven safe; prefer read-only capture first.
- Use this document for interoperability/security research, not unauthorized device access.
