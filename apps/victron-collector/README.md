# victron-collector

Production daemon for the Victron VE.Smart BLE collector. Wires validated TOML
configuration, logging, shutdown handling and concrete adapters, then starts
the `victron-service` runner. **No business logic lives in `main.rs`.**

## Status: adapters are placeholders (parallel build)

The sibling crates are still being built in parallel lanes. Every adapter in
`src/adapters/` implements the service port trait and returns a precise
`NotWired` error naming the missing wiring instead of faking success. The
daemon therefore starts, validates config, installs SIGTERM/SIGINT handling,
and each cycle fails fast with `NotWired` and backs off.

```text
cargo build
victron-collector --config config.toml --check-config   # exit 0 when valid
victron-collector --config config.toml --run-once       # one cycle, exit 3 (NotWired)
victron-collector --config config.toml                  # daemon loop
```

## Exit codes

| Code | Meaning |
|---|---|
| 0 | OK / graceful shutdown |
| 1 | runtime failure |
| 2 | configuration error (also rejects any `pin` key: secrets never belong here) |
| 3 | cycle failed purely because wiring is pending (NotWired) |

## Configuration

Validated TOML (see `src/config.rs`, `deny_unknown_fields` everywhere). Mirror
of the deployment plan: `[device] name/bluez_alias/instance/adapter`,
`[poll]` intervals/timeouts/backoff/spool bounds/`active_window_utc_hours`,
`[victoria_metrics] url/request_timeout_seconds`, `[storage] path/limits`.
There is **no PIN field**; pairing is done once with `bluetoothctl` and bond
material stays in BlueZ.

## Wiring checklist (parent pass, when sibling crates land)

- [ ] `adapters/ble.rs` — implement `BleSession` with `victron-bluez`
      (BlueZ/D-Bus via `bluer`): discover by bonded alias, GATT service
      `306b0001-...dfd0/dfd1`, CCCD writes, notification handling, chunking.
- [ ] `adapters/protocol.rs` — implement `ProtocolAdapter` with
      `victron-protocol` + `victron-domain`: `fa 80 ff` / `f9 80` negotiation,
      CBOR `subscribe`/`getValues`, reassembly, VREG decoding, sentinel
      rejection, translation to the service `Sample` (VREGs 0xEDBB/0xEDBC first).
- [ ] `adapters/storage.rs` — implement `StoragePort` with `victron-storage`
      (SQLite): schema migration, `spool_batch`/`energy_state`/`collector_state`
      tables, transactional `record_success + save_energy + spool_enqueue`,
      claim TTL + `next_attempt_at` ownership per the contract docs.
- [ ] `adapters/delivery.rs` — implement `MetricsDelivery` + `BatchRenderer`
      with `victron-metrics`: Prometheus text with explicit ms timestamps,
      `POST {url}/api/v1/import/prometheus`, retry classification
      (timeout/5xx retryable, 4xx permanent).
- [ ] `adapters/clock.rs` + `scheduler.rs` — already concrete (no change).
- [ ] Remove the `#[allow(dead_code)]` markers and the placeholder `WHY`
      strings once each adapter reads its configuration fields.
