# ESPHome / Home Assistant integration plan

Goal: run the Victron VE.Smart reader on an ESP device, integrated into ESPHome and Home Assistant.

## Target platform

Use an ESP32-class device, not ESP8266.

Reasons:

- BLE client support is required.
- The component must subscribe to GATT notifications and write GATT characteristics.
- It needs enough RAM for path lists, CBOR buffers, and history values.

Recommended baseline:

- ESP32, ESP32-S3, or ESP32-C3 with ESPHome BLE support.
- Prefer ESP32/S3 if range and memory matter.
- Use ESPHome `external_components` for the custom protocol implementation.

## Final architecture

```text
Victron device
  BLE VE.Smart service 306b0001-...
    Control  306b0002-...
    LastData 306b0003-...
    Data     306b0004-...
      ↓
ESP32 ESPHome external component
  BLE client
  VE.Smart chunk/control layer
  CBOR encoder/decoder
  path list + VREG/path value resolver
  state/history polling scheduler
      ↓
ESPHome native API
      ↓
Home Assistant entities
```

## Why current Python scripts still matter

The Python scripts are lab tools, not the final runtime.

They should be used to:

1. validate opcodes and field mappings
2. capture actual raw values while the device is nearby
3. define fixtures for the ESPHome component
4. avoid debugging everything on embedded hardware first

Final code should be C++ inside an ESPHome external component.

## ESPHome component shape

Suggested external component name:

`victron_vesmart`

Possible repo layout later:

```text
esphome/components/victron_vesmart/
  __init__.py
  sensor.py
  text_sensor.py
  binary_sensor.py
  button.py
  victron_vesmart.h
  victron_vesmart.cpp
  vesmart_cbor.h
  vesmart_cbor.cpp
  vesmart_paths.h
  vesmart_paths.cpp
```

### Component responsibilities

| Layer | Responsibility |
|---|---|
| BLE client | connect to target MAC/name, discover VE.Smart service, subscribe notifications |
| Control protocol | handle `f7`, `f8`, `f9`, `fa`, ready credits, chunk size negotiation |
| Chunking | reassemble Data/LastData notifications and split outbound CBOR writes |
| CBOR | minimal uint/int/array/bytes/string decoder and encoder |
| Path API | `getPathList`, `getPathValues`, `PathList`, `NewPath`, `PathValue` |
| VREG API | optional `getValues` for fast live values if path API is too heavy |
| Scheduler | poll live values frequently, history less frequently |
| ESPHome entities | publish sensors/text sensors/binary sensors/buttons |

## BLE protocol subset required

### Mandatory for live values

- subscribe to VE.Smart Control / LastData / Data characteristics
- negotiate control/chunking enough for data transfer
- encode `getValues` or `getPathValues`
- decode `Value` or `PathValue`

### Mandatory for history

- `0x0a` `getPathList(instance)`
- `0x0b` `getPathValues(instance, indexes)`
- `0x0d` incoming `PathList`
- `0x0e` incoming `NewPath`
- `0x0f` incoming `PathValue`
- Qt `qUncompress` path-list payload support, likely zlib-compatible

### Optional later

- writes/settings
- DFU
- pairing/PIN automation
- encrypted advertisement instant-readout parsing

Do not implement writes/settings in the first ESPHome version.

## Candidate Home Assistant entities

### Live status sensors

| HA entity | Candidate path | Type | Unit |
|---|---|---|---|
| Solar voltage | `/Pv/V` | sensor | V |
| Solar current | `/Pv/I` | sensor | A |
| Solar power | `/Yield/Power` | sensor | W |
| Battery voltage | `/Dc/0/Voltage` | sensor | V |
| Battery current | `/Dc/0/Current` | sensor | A |
| Load output current | `/Load/I` | sensor | A |
| Load output voltage | `/Load/V` | sensor | V |
| Load output power | calculated from `/Load/V * /Load/I` or dedicated path if found | sensor | W |

### State / diagnostic entities

| HA entity | Candidate path | Type |
|---|---|---|
| Charger state | `/State` | text_sensor |
| Charger off reason | `/DeviceOffReason` | text_sensor |
| Load output state | `/Load/State` | text_sensor or binary_sensor |
| Device name | `/CustomName`, `/Description2`, or BLE name | text_sensor |
| BLE connected | internal connection state | binary_sensor |
| Last update age | internal timestamp | sensor |

### Energy/history entities

| HA entity | Candidate path | Type | Unit |
|---|---|---|---|
| Lifetime total | `/Yield/System` | sensor | kWh |
| Since reset | `/Yield/User` | sensor | kWh |
| Days available | `/History/Overall/DaysAvailable` | sensor | days |
| Yield today | `/History/Daily/0/Yield` or relative `/0/Yield` | sensor | kWh |
| Load consumption today | `/History/Daily/0/Consumption` or relative `/0/Consumption` | sensor | kWh |
| Max power today | `/History/Daily/0/MaxPower` | sensor | W |
| Max PV voltage today | `/History/Daily/0/MaxPvVoltage` | sensor | V |
| Min battery voltage today | `/History/Daily/0/MinBatteryVoltage` | sensor | V |
| Max battery voltage today | `/History/Daily/0/MaxBatteryVoltage` | sensor | V |

## 30-day history in Home Assistant

ESPHome sensors are best for current scalar values. Full 30-day arrays need a choice.

Options:

1. **Minimal first version**
   - expose today/yesterday/lifetime/since-reset as normal sensors
   - let Home Assistant recorder build history from then on

2. **Per-day sensors**
   - expose `yield_day_0` through `yield_day_29`
   - expose `consumption_day_0` through `consumption_day_29`
   - simple but creates many entities

3. **JSON text sensor**
   - publish a compact JSON string for all daily history
   - fewer entities, but harder to graph directly in HA

4. **Custom HA integration later**
   - ESPHome exposes an API/service or MQTT topic
   - a Home Assistant integration stores structured history
   - most work, but cleanest for full historical arrays

Recommended path:

- v0.1: live sensors + lifetime/since-reset + today/yesterday history
- v0.2: optional per-day history sensors or JSON text sensor
- v1.0: decide whether a native HA integration is worth it

## Polling strategy

Suggested defaults:

| Data | Poll interval |
|---|---:|
| live status values | 15-60 s |
| state/off reason | 30-120 s |
| lifetime/since-reset | 5-15 min |
| daily history | 15-60 min |
| full 30-day history | on boot + manual button + 1-4 times/day |

Avoid keeping BLE connected continuously unless needed. VictronConnect mobile app may compete for the connection.

## ESPHome YAML sketch

This is illustrative; exact config keys depend on the final component implementation.

```yaml
external_components:
  - source:
      type: local
      path: components

esp32_ble_tracker:

ble_client:
  - mac_address: AA:BB:CC:DD:EE:FF
    id: victron_ble

victron_vesmart:
  id: solar-charger
  ble_client_id: victron_ble
  instance: 3
  service_suffix: dfd0
  update_interval: 30s
  history_update_interval: 1h

sensor:
  - platform: victron_vesmart
    victron_vesmart_id: solar-charger
    solar_voltage:
      name: Solar Charger Solar Voltage
    solar_current:
      name: Solar Charger Solar Current
    solar_power:
      name: Solar Charger Solar Power
    battery_voltage:
      name: Solar Charger Battery Voltage
    battery_current:
      name: Solar Charger Battery Current
    lifetime_yield:
      name: Solar Charger Lifetime Yield
    since_reset_yield:
      name: Solar Charger Since Reset Yield
    yield_today:
      name: Solar Charger Yield Today

text_sensor:
  - platform: victron_vesmart
    victron_vesmart_id: solar-charger
    charger_state:
      name: Solar Charger Charger State
    charger_off_reason:
      name: Solar Charger Charger Off Reason

button:
  - platform: victron_vesmart
    victron_vesmart_id: solar-charger
    refresh_history:
      name: Solar Charger Refresh History
```

## Risks and open questions

| Risk / unknown | Impact | Mitigation |
|---|---|---|
| Pairing/encryption required | ESP32 may need bonding support and stored keys | test with owned device; start with already-paired or unprotected data |
| Path indexes are runtime-defined | cannot hardcode indexes | request `PathList` at connect |
| `qUncompress` memory use | path list may be large | stream/limit buffer; use PSRAM if available; request only once |
| Chunking details incomplete | writes/reads may fail | validate with Python + HCI capture first |
| Scaling/units incomplete | HA values may be wrong | derive from QML/vregs.json/runtime fixtures |
| Full 30-day history is many values | HA entity explosion | start with today/lifetime; add optional history mode |
| Mobile app connection contention | ESP may block VictronConnect | poll periodically, disconnect after reads |

## Implementation phases

### Phase 1 — offline/static

- [x] Extract QML UI strings.
- [x] Map screenshot fields to candidate paths.
- [x] Create Python history reader scaffold.
- [ ] Extract Qt resource `vregs.json`.
- [ ] Parse path/VREG/scaling metadata.

### Phase 2 — runtime validation with owned device

- [ ] Run `scripts/read-victron-history.py` against the device.
- [ ] Capture PathList and PathValue responses.
- [ ] Confirm day indexing and units.
- [ ] Confirm state/off-reason enum values.
- [ ] Save sanitized fixtures.

### Phase 3 — ESPHome proof of concept

- [ ] Create ESPHome external component skeleton.
- [ ] Implement BLE connect/discover/notify/write.
- [ ] Implement minimal CBOR and chunking.
- [ ] Read and publish live values.
- [ ] Read and publish lifetime/since-reset/today history.

### Phase 4 — production hardening

- [ ] reconnect/backoff logic
- [ ] BLE contention handling
- [ ] configurable polling
- [ ] HA diagnostics
- [ ] sanitized tests using captured fixtures
- [ ] optional full 30-day history mode

## Immediate next best step

Continue without the device:

1. dump `:/ext/shared-definitions/json/vregs.json` from the native Qt resource section
2. parse path/VREG/scaling metadata
3. generate an ESPHome entity mapping spec from `ui-field-candidates.json` + `vreg-path-map.json`

Then, when the device is nearby, use `scripts/read-victron-history.py` to validate the mapping before writing embedded C++.
