# VictronConnect BLE protocol findings

Scope: static analysis of VictronConnect `com.victronenergy.victronconnect` version `6.33` / version code `7000715`, acquired as an APKPure XAPK mirror via `apkeep`.

Trust note: acquisition source is lower-trust mirror provenance. Re-verify against a user-owned device or Play Store-authenticated artifact before relying on exact version claims.

## App architecture

VictronConnect's Android Java code mostly wraps platform BLE lifecycle and pairing. The Bluetooth protocol itself is implemented in Qt/C++ inside:

- `decompiled/native/lib/armeabi-v7a/libVictronConnect_armeabi-v7a.so`
- `decompiled/native/lib/armeabi-v7a/libQtBluetoothLocal_armeabi-v7a.so`

Important Java wrappers:

- `decompiled/jadx/com.victronenergy.victronconnect/sources/com/victronenergy/victronconnect/BleScanManager.java`
- `decompiled/jadx/com.victronenergy.victronconnect/sources/com/victronenergy/victronconnect/PairingHelper.java`
- `decompiled/jadx/com.victronenergy.victronconnect/sources/com/victronenergy/victronconnect/PlatformHelperService.java`

Android permissions include BLE scan/connect permissions plus legacy Bluetooth/location permissions:

- `BLUETOOTH`, `BLUETOOTH_ADMIN`
- `ACCESS_FINE_LOCATION` with max SDK 30
- `BLUETOOTH_SCAN` with `neverForLocation`
- `BLUETOOTH_CONNECT`

## Native entry points

Key exported/dynamic symbols from `libVictronConnect_armeabi-v7a.so`:

- `VeBleInterface::*` — scan, discovery, storage, connection orchestration
- `VeSmartDevice::*` — smart-device advertisement matching, connection, VE.Smart response routing
- `VeService::*` — pairing/device-info/PIN/PUK/DFU service
- `VeSmartService::*` — main VE.Smart CBOR control/data protocol
- `DfuService::*` and `Legacy::DfuService::*` — firmware update paths

Thumb-mode disassembly was required. Use `llvm-objdump --triple=thumbv7-unknown-linux-android`; generic `objdump -d` misdecodes many functions.

## Advertisement matching

`VeSmartDevice::validAdvertisement(QBluetoothDeviceInfo const&)` checks either:

1. manufacturer data with company id `0x02e1` and first payload byte `0x10`, or
2. advertised service UUID equal to either of `VeSmartService::ServiceUuid0` / `ServiceUuid1`.

The VE.Smart service UUIDs are initialized from UTF-16 strings:

| Symbol | UUID |
|---|---|
| `VeSmartService::ServiceUuid0` | `306b0001-b081-4037-83dc-e59fcc3cdfd0` |
| `VeSmartService::ServiceUuid1` | `306b0001-b081-4037-83dc-e59fcc3cdfd1` |

## BLE services and characteristics

### VE service / pairing service

`VeService::*` uses a service and four characteristics initialized from UTF-16 UUID strings:

| Symbol | UUID | Observed role |
|---|---|---|
| `VeService::ServiceUuid` | `97580001-ddf1-48be-b73e-182664615d8e` | service UUID |
| `VeService::DeviceInfoUuid` | `97580002-ddf1-48be-b73e-182664615d8e` | device info characteristic |
| `VeService::PinCodeUuid` | `97580003-ddf1-48be-b73e-182664615d8e` | PIN-code characteristic |
| `VeService::PukCode24Uuid` | `97580004-ddf1-48be-b73e-182664615d8e` | PUK-code characteristic |
| `VeService::StartDfuUuid` | `97580006-ddf1-48be-b73e-182664615d8e` | start-DFU characteristic |

`VeService::getCharacteristics()` stores them in object fields:

- offset `+0x28`: device info characteristic
- offset `+0x34`: PIN-code characteristic
- offset `+0x40`: PIN CCC descriptor `0x2902`
- offset `+0x4c`: PUK-code characteristic
- offset `+0x58`: PUK CCC descriptor `0x2902`
- offset `+0x64`: start-DFU characteristic

The code validates characteristic properties and writes CCC descriptors. Existing descriptor enable/disable flow uses hex string `0000`; exact notify-enable value still needs live trace confirmation.

### VE.Smart main service

`VeSmartService::getUuid(unsigned char index)` derives characteristic UUIDs from the connected service UUID by converting the service UUID to RFC4122 bytes and incrementing byte `3` by `index`.

Given service `306b0001-b081-4037-83dc-e59fcc3cdfd0`, this yields:

| index | UUID | Object field | Observed role |
|---:|---|---|---|
| 1 | `306b0002-b081-4037-83dc-e59fcc3cdfd0` | `+0x2c` | control characteristic, written by `writeControl()` |
| 2 | `306b0003-b081-4037-83dc-e59fcc3cdfd0` | `+0x38` | data/CBOR chunk characteristic |
| 3 | `306b0004-b081-4037-83dc-e59fcc3cdfd0` | `+0x44` | data/CBOR chunk characteristic |

The same derivation likely applies to service `...dfd1`.

`VeSmartService::getCharacteristics()` looks up all three derived characteristics and requires valid notify/indicate-capable properties. It writes CCC descriptors on all three characteristics.

### DFU services

Two DFU families are present.

Modern `DfuService::*`:

| Symbol | UUID |
|---|---|
| `DfuService::ServiceUuid` | `68c10001-b17f-4d3a-a290-34ad6499937c` |
| `DfuService::ControlUuid` | `68c10002-b17f-4d3a-a290-34ad6499937c` |
| `DfuService::DataUuid` | `68c10003-b17f-4d3a-a290-34ad6499937c` |

Legacy `Legacy::DfuService::*`:

| Symbol | UUID |
|---|---|
| `Legacy::DfuService::ServiceUuid` | `00001530-1212-efde-1523-785feabcd123` |
| `Legacy::DfuService::ControlUuid` | `00001531-1212-efde-1523-785feabcd123` |
| `Legacy::DfuService::PacketUuid` | `00001532-1212-efde-1523-785feabcd123` |

## VE.Smart control/data framing

### Control characteristic writes

`VeSmartService::writeControl(QByteArray const&)` writes to field `+0x2c`, so derived UUID index `1` is the control characteristic.

Observed control opcodes:

| Bytes | Meaning |
|---|---|
| `f9 <n>` | ready-to-receive credit, emitted by `writeReadyToReceive(unsigned char)` after accumulated credit reaches `0x41` |
| `fa <max-att?> <chunk-size?>` | CBOR/chunk-size control, emitted by `writeCborChunkSize(unsigned char,unsigned char)` |

`processControlData()` parses incoming control data as big-endian `QDataStream`:

1. `uint8` at object offset `+0x74`
2. `uint16` at `+0x76`
3. `uint8` at `+0x79`
4. `uint8` at `+0x78`
5. optional `uint16` at `+0x7c`; if missing or `<= 0x14`, set to `0x14`

Static names are not available, but logs around this code refer to max ATT length, chunk size, and CBOR chunks. The code sends `fa 80 ff` and then ready credit `f9 80` when negotiating/adjusting chunk parameters.

### CBOR command stream

Application requests are CBOR values concatenated into a byte stream and sent with `writeCbor()`. Helpers build commands by encoding an opcode as a CBOR unsigned integer, followed by CBOR-encoded parameters.

Confirmed outgoing opcodes from helper functions:

| Opcode | Function | Parameters |
|---:|---|---|
| `1` | `getDevices()` | none |
| `3` | `subscribe(instance)` | `uint16 instance` |
| `4` | `unsubscribe(instance)` | `uint16 instance` |
| `5` | `getValues(instance, vreg-list)` | `uint16 instance`, list of `uint16` vregs |
| `6` | `setValues(instance, pairs)` | `uint16 instance`, list of `(uint16 vreg, QByteArray value)` pairs |
| `10` | `getPathList(instance)` | `uint16 instance` |
| `11` | `getPathValues(instance, path-index-list)` | `uint16 instance`, list of `int` path indexes |
| `12` | `setPathValues(instance, pairs)` | `uint16 instance`, list of `(int pathIndex, QVariant value)` pairs |

Single-value helpers delegate to list variants:

- `getValue(instance, vreg)` -> `getValues(instance, [vreg])`
- `setValue(instance, vreg, bytes)` -> `setValues(instance, [(vreg, bytes)])`
- `getPathValue(instance, pathIndex)` -> `getPathValues(instance, [pathIndex])`
- `setPathValue(instance, pathIndex, value)` -> `setPathValues(instance, [(pathIndex, value)])`

Keep-alive uses `setValue(0, 0x0093, QByteArray{0x10, 0x27})`, i.e. writes vreg `0x0093` on instance `0` with bytes `10 27`.

### Chunking

`writeCbor()` queues CBOR payloads and combines adjacent payloads up to the negotiated byte limit at object offset `+0x78`.

`writeChunkToStack()` splits outbound queued data by the negotiated chunk size at `+0x7c` and alternates/chooses between the two data characteristics (`+0x38` and `+0x44`, derived UUID indexes `2` and `3`) when writing chunks. Live captures are needed to confirm the exact alternation rule.

When input control data advertises sufficient size, the app builds an initial queued CBOR message:

- bytes: `06 00 82 00 58 <len> <zero padding...>`
- `<len>` is `(incoming chunk length - 6)`

This appears to be a prebuilt CBOR-ish set-values/byte-string message used to prime or adjust chunking. Needs deeper validation.

## Crypto / protected advertisements

The binary references encrypted manufacturer-data handling and BLE advertisement key storage:

- `ExtraManufData::BaseTypes::EncryptedRecord<...>` symbols
- `VeBleInterface::storeAdvertisementKey(...)`
- `VeBleInterface::getStoredAdvertisementKey(...)`
- `Networking::aes_ccm_encrypt`
- strings mentioning PUK/PIN/dynamic key handling

So some advertisement records or instant-readout data are likely AES-CCM protected and require product-specific keys/PIN flows. No keys were extracted or recorded.

## Generated analysis artifacts

Generated files are under `analysis/generated/` and ignored by git. Useful artifacts:

- `libVictronConnect.dynamic-symbols.demangled.txt`
- `vesmartservice-symbols.txt`
- `veservice-bleservicebase-symbols.txt`
- `thumb-protocol-disassembly.txt`
- `vesmartservice-write-methods.txt`
- `veservice-actions.txt`

## Next work

1. Capture live BLE traffic for pairing plus normal readout to confirm CCC values, characteristic directions, and chunk alternation.
2. Build a small CBOR encoder/decoder test harness for the confirmed opcodes.
3. Re-acquire the APK from a higher-trust source and compare hashes/symbol findings.
