# victron-bluez

Linux-only async BLE transport for Victron VE.Smart devices over the host
BlueZ daemon (D-Bus). Wire-agnostic: moves **raw bytes + characteristic
identity**, never CBOR/VREG/domain values.

## Capabilities

- adapter selection (`hci0` by default) with a pure, unit-tested selection fn
- powered-state handling that never silently flips host-wide adapter policy
  (`PowerPolicy::RequireManual` default; `EnableIfOff` is explicit opt-in); a
  failed power mutation classifies as `Other`, never `NotFound`
- config validation (`TransportConfig::validate`) before any D-Bus action:
  the device selector must hold a non-empty bounded alias and/or address, and
  all timeouts plus `write_chunk_size` must be positive. `DeviceSelector` has
  no `Default`; construction is fallible
- resolution of a bonded, configured device by alias/address **plus** Victron
  advertisement evidence (manufacturer id `0x02e1` / `0x10` byte, or a
  VE.Smart service UUID), with a bounded discovery-scan fallback. When both
  alias and address are configured **both must match** — a device that merely
  shares the alias while its address differs is never selected. Unrelated
  devices with transient property errors are skipped and logged without raw
  addresses; errors on the explicitly configured address stay actionable
- connect with deadline, VE.Smart service variant `...dfd0` / `...dfd1`
  location, Control / LastData / Data characteristic discovery + flag
  validation (notify/indicate on all three, read on Control, write capability
  on the outbound Control/Data characteristics with Control-vs-Data error
  labels), notification subscription on all three
- **transactional `open`**: if connect succeeds but GATT locate or any
  subscription fails, started notifications are stopped and the local device
  is disconnected before returning; a failed open leaves the transport closed
  and reusable with no stale session/adapter/device/GATT state
- Control read, bounded write using the write procedure validated at locate
  (write-without-response preferred, write-with-response accepted)
- typed notifications tagged with their source characteristic
- RSSI read, clean close (stop notifications + disconnect)
- every potentially hanging BlueZ operation is bounded by the single coherent
  `operation_timeout` (default 20s): adapter/device property reads, GATT
  resolution, notify subscription, Control reads, writes, RSSI, and
  disconnect/cleanup. `connect`, the discovery scan, and `next_notification`
  keep their own domain timeouts
- error classification: timeout / auth / contention / not-found / D-Bus;
  errors and logs never contain MAC addresses, raw payloads, or unbounded
  BlueZ messages

## Out of scope

- pairing / PIN / PUK automation (BlueZ owns the pre-established bond)
- protocol parsing, CBOR, VREGs, measurements

## Dependency & feature notes

- `bluer` 0.17.4 with the `bluetoothd` feature (validated against the actual
  crate source: `bluer::Session`, `Adapter`, `Device`, `gatt::remote::{Service,
  Characteristic}`, `Error { kind, message }`, `ErrorKind`)
- feature `bluer` (default): the BlueZ backend. Disabling leaves the pure
  identity/error layers usable without BlueZ
- `tokio` (time/macros/sync/rt), `futures`, `uuid`, `log`, `async-trait`
  (dyn-compatible test seam, per the collector plan)

## Build requirements (Linux / cross)

- `libdbus-1` (`libdbus-1-dev`, `pkg-config`); for ARMv6 cross-compilation
  also the `armhf` variant in the sysroot (`PKG_CONFIG_PATH` set for the
  target)

## Runtime requirements

- system D-Bus with `bluetoothd` running (starts after
  `bluetooth.service`)
- a powered adapter (or explicit `EnableIfOff` config)
- a device pre-bonded via `bluetoothctl pair ...`
- BlueZ `org.bluez` access over the system bus (default policy allows the
  `bluetooth` group / root)

## Integration notes

- chunking of VE.Smart CBOR frames is the protocol crate's job; this
  transport bounds each write to `write_chunk_size` (default 20) and rejects
  oversized payloads with `BleError::PayloadTooLarge`
- `next_notification` applies `notification_timeout` internally; the service
  layer can additionally wrap it with `tokio::time::timeout`
- every other BlueZ operation is bounded by `operation_timeout`; bluer's
  `Device::services()` internally waits for GATT service resolution (its own
  ceiling is ~120s) and our `service-discovery` deadline bounds the whole
  resolution from outside
- `require_advertisement_evidence` defaults to `true`; a bonded device that is
  not advertising right now resolves after the bounded scan window
  (`discovery_timeout`). Set `false` to rely on GATT verification after
  connect instead
