# Screenshot UI coverage — VictronConnect solar charger

Scope: compare the original VictronConnect screenshot at `a private reference screenshot` against the current BLE/protocol documentation and decompiled bundle.

Note: the screenshot was processed locally. The UI text inventory comes from local OCR plus static bundle search; exact numeric values are runtime BLE/device data and are not expected to exist in the APK bundle.

## Visible screenshot inventory

| Area | Visible fields / values | Notes |
|---|---|---|
| Header | `VictronConnect`, device `Solar Charger` | Device name is runtime/user/device data, not a bundle string. |
| Navigation | `Status`, `Trends` | Static QML also contains `History`. |
| Solar | `Voltage 0.81 V`, `Current 0.0 A`, `Power 0 W` | OCR read `Power` value as `Ow`/`OW`; interpreted as `0 W`. |
| Battery / charger | `Voltage 25.58 V`, `Current 0.00 A`, `3.49 kWh`, `State Off`, `Why is the charger off?` | The `3.49 kWh` label was not visible in OCR; likely yield/history-derived. |
| Load output | `1.75 kWh`, `State Off`, `Current 0.0 A`, `Power 0 W` | The `1.75 kWh` label was not visible in OCR; likely load/yield/history-derived. |
| Trends/history chart | `Last 30 days`, `Detailed`, date buckets `2, 1, 31..13`, `June` | Date/month labels are runtime chart-axis labels. |
| Totals | `Lifetime total 1220 kWh`, `Since reset 1220 kWh` | Static QML indicates likely paths `/Yield/System` and `/Yield/User`. |

## Coverage verdict

Current protocol docs cover:

- VictronConnect app/package/version context.
- BLE discovery and VE.Smart service/characteristic layout.
- Generic VE.Smart CBOR transports: `getValues`, `getPathList`, `getPathValues`, `Value`, `PathValue`.
- Java/native background readout bridge via `PlatformHelperService` and `instantReadoutDeviceChanged(deviceId, deviceJson)`.

Missing before this note:

- concrete screenshot field -> QML item -> VBus path -> VREG/path-index mapping
- Status tab field table
- Trends/History field table
- charger/load `Off` enum and `/DeviceOffReason` values
- reset semantics for lifetime vs since-reset totals
- runtime fixture tying actual screenshot values to BLE frames

## Static bundle anchors found

Main UI labels are not Android `res/values/strings.xml`; they are compiled Qt/QML strings inside `libVictronConnect_armeabi-v7a.so`.

| Screenshot feature | Static anchor | Candidate mapping |
|---|---|---|
| Main solar charger page | `QmlCacheGeneratedCode::_qml_PageSolarCharger_qml::qmlData` | Contains `Status`, `History`, `Trends`, `Solar`, `Battery`, `Voltage`, `Current`, `Power`, `State`, `Why is the charger off?`, `Load output`, `Last 30 days`, `Detailed`. |
| Solar voltage | `VBusItemsSolarCharger` qmlData | `pvVoltage` -> `/Pv/V` |
| Solar current | `VBusItemsSolarCharger` qmlData | `pvCurrent` -> `/Pv/I` |
| Solar power | `VBusItemsSolarCharger` qmlData | `pvPower` -> `/Yield/Power` |
| Battery voltage | `VBusItemsSolarCharger` qmlData | `batteryVoltage` -> `/Dc/0/Voltage` |
| Battery current | `VBusItemsSolarCharger` qmlData | `batteryCurrent` -> `/Dc/0/Current` |
| Charger state | `VBusItemsSolarCharger` qmlData | `state` -> `/State` |
| Charger off reason | `VBusItemsSolarCharger` + `DeviceOffReasons` qmlData | `deviceOffReason` -> `/DeviceOffReason` |
| Load output state | `VBusItemsSolarCharger` qmlData | `loadOutputState` -> `/Load/State` |
| Load output current | `VBusItemsSolarCharger` qmlData | `loadOutputCurrent` -> `/Load/I` |
| Load output voltage | `VBusItemsSolarCharger` qmlData | `loadOutputVoltage` -> `/Load/V` |
| Load output power | `VBusItemsSolarCharger` qmlData | `loadOutputPower`; likely calculated from `/Load/V` and `/Load/I` or a nearby product item. |
| Lifetime total | `PageSolarChargerHistory` / `SolarChargerHistoryGraphFullSceen` qmlData | likely `/Yield/System` |
| Since reset | `PageSolarChargerHistory` / `SolarChargerHistoryGraphFullSceen` qmlData | likely `/Yield/User` |
| Daily history | history QML data | `/History/Daily/`, `/History/Overall/DaysAvailable`, `/0/Yield`, `/0/MaxPower`, `/0/MaxPvVoltage`, `/0/Consumption`, `/0/MinBatteryVoltage`, `/0/MaxBatteryVoltage` |
| Trend chart VREGs | `PageGraphsSolarCharger` / `TrendsSolarCharger` qmlData | `inputPowerVreg`, `batteryVoltageVreg`, `batteryCurrentVreg`, `inputVoltageVreg`, `loadCurrentVreg`, `totalYieldToday`, `totalLoadToday` |
| Background/widget MPPT readout | `BroadcastDataMppt` qmlData | includes `Load current`, `Yield today`, `State`, `Power`, `Battery voltage`, `/History/Daily/0/Yield`, `/Yield/Power`, `/Dc/0/Voltage` |
| Device name | Java JSON consumers + likely VBus paths | Java uses `customName`; static path candidates include `/CustomName` and `/Description2`, but screenshot name `Solar Charger` is runtime data. |

## Generated static extraction outputs

Implemented static QML extraction scripts:

- `scripts/extract-qmlcache-strings.py`
- `scripts/map-qml-fields.py`

Generated local artifacts:

- `analysis/generated/qmlcache-strings.json`
- `analysis/generated/qmlcache-strings.tsv`
- `analysis/generated/ui-field-candidates.json`
- `analysis/generated/ui-field-candidates.md`

Current static result summary:

| UI field | Candidate path(s) | Static confidence |
|---|---|---|
| Device name | `/CustomName`, `/Description2` | candidate |
| Solar voltage | `/Pv/V` | medium-high |
| Solar current | `/Pv/I` | medium-high |
| Solar power | `/Yield/Power` | medium-high |
| Battery voltage | `/Dc/0/Voltage` | medium-high |
| Battery current | `/Dc/0/Current` | medium-high |
| Charger state | `/State` | medium-high |
| Charger off reason | `/DeviceOffReason` | medium-high |
| Load output state | `/Load/State` | medium-high |
| Load output current | `/Load/I` | medium-high |
| Load output power | `/Load/V`, `/Load/I` | candidate |
| Trend chart kWh scale | `/History/Daily/`, `/0/Yield`, `/0/Consumption` | candidate |
| Lifetime total | `/Yield/System` | high-static |
| Since reset | `/Yield/User` | high-static |

The extraction also recovered `18` charger off-reason labels from `DeviceOffReasons` qmlData.

## Off-reason anchors

`DeviceOffReasons` qmlData contains reason UI strings and should be mined for enum mapping. Known static strings include:

- `#11: No PV input power`
- `#17: Solar charger disabled`
- `Unknown off reason`
- `Solar charging is off because there is no or not enough PV power.`

Candidate protocol path:

- `/DeviceOffReason`

Needed: map numeric `/DeviceOffReason` values to the QML reason strings and help text.

## Candidate Status tab mapping table

| UI field | Candidate path | Candidate transport | Unit / rendering | Confidence | Needs runtime confirmation |
|---|---|---|---|---|---|
| Device name `Solar Charger` | `/CustomName` or `/Description2` or `deviceJson.customName` | Path value or background JSON | string | low | yes |
| Solar voltage | `/Pv/V` | path value or VREG | V | medium | yes |
| Solar current | `/Pv/I` | path value or VREG | A | medium | yes |
| Solar power | `/Yield/Power` | path value or VREG | W | medium | yes |
| Battery voltage | `/Dc/0/Voltage` | path value or VREG | V | medium | yes |
| Battery current | `/Dc/0/Current` | path value or VREG | A | medium | yes |
| Battery/charger energy `3.49 kWh` | likely daily yield/history path | path value or history item | kWh | low | yes |
| Charger state `Off` | `/State` | path value or VREG enum | enum string | medium | yes |
| Charger off reason/help | `/DeviceOffReason` | path value or VREG enum | enum/help string | medium | yes |
| Load output energy `1.75 kWh` | likely load daily/history path | path value or history item | kWh | low | yes |
| Load output state `Off` | `/Load/State` | path value or VREG enum | enum string | medium | yes |
| Load output current | `/Load/I` | path value or VREG | A | medium | yes |
| Load output power | calculated or `/Load/P`-like path | calculated / path value / VREG | W | low | yes |

## Candidate Trends / History mapping table

| UI field | Candidate path / source | Notes | Confidence |
|---|---|---|---|
| `Last 30 days` | QML history/trend range control | UI range label; not a BLE value itself. | high |
| `Detailed` | QML history/trend mode control | UI display mode; not a BLE value itself. | high |
| date buckets + `June` | chart axis from runtime date/history data | Generated by UI from current date and history records. | medium |
| `Lifetime total` | `/Yield/System` | Static QML candidate. | medium |
| `Since reset` | `/Yield/User` | Static QML candidate. | medium |
| daily yield | `/History/Daily/<n>/Yield` or `/History/Daily/0/Yield` | Existing anchors include `/0/Yield`; exact day indexing requires runtime capture. | medium |
| max power | `/History/Daily/<n>/MaxPower` | Static anchor `/0/MaxPower`. | medium |
| max PV voltage | `/History/Daily/<n>/MaxPvVoltage` | Static anchor `/0/MaxPvVoltage`. | medium |
| load consumption | `/History/Daily/<n>/Consumption` | Static anchor `/0/Consumption`; likely load-output energy candidate. | low |
| min battery voltage | `/History/Daily/<n>/MinBatteryVoltage` | Static anchor. | medium |
| max battery voltage | `/History/Daily/<n>/MaxBatteryVoltage` | Static anchor. | medium |
| trend PV input power | `inputPowerVreg` | Needs vregs.json / VregDataTranslator to resolve numeric VREG. | low |
| trend battery voltage | `batteryVoltageVreg` | Needs vregs.json / VregDataTranslator. | low |
| trend battery current | `batteryCurrentVreg` | Needs vregs.json / VregDataTranslator. | low |
| trend PV input voltage | `inputVoltageVreg` | Needs vregs.json / VregDataTranslator. | low |
| trend load current | `loadCurrentVreg` | Needs vregs.json / VregDataTranslator. | low |
| total yield today | `totalYieldToday` | Static QML property; exact path/VREG needs extraction. | low |
| total load today | `totalLoadToday` | Static QML property; exact path/VREG needs extraction. | low |

## Next extraction work

### Static extraction

Done:

1. Extracted targeted `QmlCacheGeneratedCode::*::qmlData` blobs from `libVictronConnect_armeabi-v7a.so`.
2. Parsed UTF-16 QML cache strings and emitted `analysis/generated/qmlcache-strings.json` / `.tsv`.
3. Built `analysis/generated/ui-field-candidates.json` / `.md` from these QML blobs:
   - `PageSolarCharger`
   - `VBusItemsSolarCharger`
   - `DeviceOffReasons`
   - `PageSolarChargerHistory`
   - `SolarChargerHistoryGraphFullSceen`
   - `PageGraphsSolarCharger`
   - `TrendsSolarCharger`
   - `BroadcastDataMppt`
   - `VBusItem`
   - `HistoryValue`

Still todo:

4. Dump Qt resources from the ELF, especially `:/ext/shared-definitions/json/vregs.json`.
5. Parse `vregs.json` into `analysis/generated/vreg-path-map.json`.
6. Disassemble / inspect `VregDataTranslator::{init,findPath,requestValue,setValue,updateItem,publish}` to confirm path-to-VREG and scaling rules.

### Runtime extraction

1. On an owned device, enable Android Bluetooth HCI snoop.
2. Open VictronConnect, connect to the same `Solar Charger` device, visit Status and Trends, wait for values.
3. Collect bugreport + logcat.
4. Decode ATT/GATT traffic for VE.Smart service `306b0001-...` and chars `306b0002/0003/0004-...`.
5. Reassemble Data/LastData CBOR streams.
6. Decode:
   - `0x0d` PathList
   - `0x0b` getPathValues
   - `0x0f` PathValue
   - `0x05` getValues
   - `0x08` Value
7. Join static UI candidates with runtime path indexes / VREG ids.
8. Emit `analysis/generated/resolved-ui-ble-mapping.json` and `.md`.

## History reader scaffold

Implemented no-device-ready history reader:

- `scripts/read-victron-history.py`

Dry run while the BLE device is not nearby:

```bash
python3 scripts/read-victron-history.py --dry-run --days 30 --out analysis/generated/history-dry-run.json
```

When the owned device is nearby and paired/bonded:

```bash
uv run --with bleak python scripts/read-victron-history.py --target 'Solar Charger' --days 30 --out analysis/generated/runtime/solar-charger-history.json
```

The script prepares candidate paths and implements the path protocol:

- `0x0a` `getPathList(instance)`
- `0x0b` `getPathValues(instance, indexes)`
- `0x0d` incoming `PathList`
- `0x0e` incoming `NewPath`
- `0x0f` incoming `PathValue`
- `0x10` incoming `PathResponse`

Live retry result: on the tested `Solar Charger` MPPT, `PathList` was not returned and path requests appeared rejected/unsupported. The script now falls back to observed VREG history/trend records and returns `mode: "vreg-fallback"` when it can capture them.

Observed fallback registers:

| Register | Current interpretation |
|---|---|
| `0x104f`, `0x1050` | 34-byte MPPT history/trend blocks; exact field layout still pending translator/vregs extraction |
| `0xec20` | available trend-VREG block; slots included `0xed8d`, `0xedec`, `0xec3e`, `0xed8c` in the latest capture |
| `0x2001`, `0x2007`, `0x2008`, `0x200b`, `0x2013`, `0x2027` | trend/history-adjacent VREGs pushed during subscription |
| `0xed8c`, `0xed8d`, `0xedbb`, `0xedbc` | confirmed live battery current, battery voltage, solar voltage, solar power |

Generated artifacts:

- `analysis/generated/history-dry-run.json`
- `analysis/generated/runtime/history-fallback-*.json` (ignored, private runtime data)

## Suggested scripts

| Script | Purpose |
|---|---|
| `scripts/extract-qmlcache-strings.py` | Extract QML cache string tables from `libVictronConnect_armeabi-v7a.so`. |
| `scripts/map-qml-fields.py` | Window strings around QML components and produce UI field candidates. |
| `scripts/read-victron-history.py` | Read path-based solar charger history when the owned BLE device is nearby; supports `--dry-run` offline. |
| `scripts/extract-qt-resources-from-elf.py` | Dump Qt resources, especially shared definitions JSON. |
| `scripts/parse-vregs-json.py` | Parse `vregs.json` into path/VREG/conversion mapping. |
| `scripts/decode-vesmart-btsnoop.py` | Reassemble VE.Smart BLE CBOR from HCI/tshark output. |
| `scripts/resolve-ui-ble-mapping.py` | Join static QML + VREG map + runtime BLE events into final mapping. |
| `scripts/frida-victron-map.js` | Optional owned-device native hooks for mapping requests before BLE encoding. |

## Safety

- Do not commit raw screenshots, btsnoop logs, MAC addresses, PINs, PUKs, advertisement keys, serial numbers, or private device data.
- Commit sanitized mappings and redacted fixtures only.
- Treat runtime data from `Solar Charger` as private device data unless explicitly sanitized.
