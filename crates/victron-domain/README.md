# victron-domain

Hardware- and wire-independent domain model for the Victron BLE collector
(planned Rust workspace crate per `analysis/rust-multicrate-collector-plan.md`).

This crate owns the canonical business values of the collector:

- validated [`DeviceId`](src/device.rs) (stable local name, label-safe charset)
- [`Sample`](src/sample.rs) with a `std::time::SystemTime` timestamp and named
  optional measurement fields
- [`Measurement`](src/measurement.rs) with `f64` value and
  [`Quality`](src/quality.rs) (`ConfirmedNative`, `Candidate`, `Derived`,
  `LocallyIntegrated`)
- bounded [`ChargerState`](src/state.rs) / [`LoadState`](src/state.rs) enums
  that preserve unknown numeric codes as `Unknown(u8)`
- documented conservative physical validation ranges in
  [`bounds`](src/bounds.rs)
- sample freshness and validity helpers
- derived PV current and load power helpers (valid inputs only)

## Rules

- No VREG IDs, GATT UUIDs, Prometheus metric names, SQLite, HTTP, Tokio,
  BlueZ, or any I/O.
- Zero external dependencies. `std` only (`SystemTime`); the planned
  workspace keeps `no_std` optional and not an MVP goal.
- All fallible constructors reject non-finite values and physically
  impossible values outside the documented conservative ranges.

## Build and test

```sh
cargo build
cargo test
cargo doc --no-deps
cargo clippy --all-targets -- -D warnings
```

## Usage

```rust
use std::time::Duration;
use victron_domain::{ChargerState, DeviceId, Quality, Sample};

let device = DeviceId::new("solar-charger")?;
let sample = Sample::builder_now(device)
    .pv_voltage_volts(34.2, Quality::ConfirmedNative)?
    .pv_power_watts(96.0, Quality::ConfirmedNative)?
    .battery_voltage_volts(12.6, Quality::Candidate)?
    .charger_state(ChargerState::Bulk)
    .build();

assert!(sample.is_fresh(Duration::from_secs(300)));
assert_eq!(sample.derived_pv_current().map(|m| m.quality()), Some(Quality::Derived));
# Ok::<(), victron_domain::DomainError>(())
```
