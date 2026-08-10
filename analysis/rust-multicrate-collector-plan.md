# Rust multi-crate plan: Victron BLE collector for Raspberry Pi Zero W

## Objective

Build a small, reliable Rust service for a Raspberry Pi Zero W that reads an owned Victron device through the reverse-engineered VE.Smart BLE protocol and pushes normalized Prometheus samples to the existing VictoriaMetrics instance for Grafana energy dashboards.

Success means:

- the process runs unattended on Raspberry Pi Zero W hardware
- it reconnects after BLE, network, and host failures
- it performs read-only Victron operations
- Grafana receives correct live values and a trustworthy cumulative energy counter
- protocol logic is hardware-independent and covered by captured fixtures
- no new inbound network service is exposed on the Pi

## Hard platform constraint: Raspberry Pi Zero W

The original Pi Zero W uses an ARM1176JZF-S CPU: ARMv6, 32-bit, hard-float Linux.

Primary Rust target:

```text
arm-unknown-linux-gnueabihf
```

This target decision affects every dependency. Before building the full application, prove that the selected Rust toolchain and dependency set can:

1. cross-compile a minimal binary for ARMv6 hard-float
2. run it on the actual Pi Zero W
3. communicate with BlueZ over the system D-Bus
4. perform an HTTP POST to VictoriaMetrics

Do not assume ARMv7 Raspberry Pi binaries will run on the Pi Zero W. Do not use a `target-cpu` requiring ARMv7/NEON.

Recommended release profile:

```toml
[profile.release]
opt-level = "s"
lto = "thin"
codegen-units = 1
panic = "abort"
strip = "symbols"
```

Measure startup time, resident memory, CPU use, and binary size on hardware before further optimization.

## Architecture principles

- Multi-crate Cargo workspace with one-way dependencies.
- Pure protocol crate; no BlueZ, filesystem, HTTP, or runtime coupling.
- Linux/BlueZ code isolated behind a narrow BLE transport trait.
- Domain measurements independent from wire VREGs and Prometheus names.
- Durable state and VictoriaMetrics delivery isolated from acquisition.
- One production daemon binary; one diagnostic CLI binary.
- No generic plugin framework or premature device-family abstraction.
- No write/settings/DFU support in the production dependency graph.
- Bounded channels, buffers, retries, and persistent spool.
- Prefer native cumulative Victron energy; local integration only as explicit fallback.

## Cargo workspace

Suggested repository layout:

```text
Cargo.toml
Cargo.lock
rust-toolchain.toml
.cargo/config.toml

crates/
  victron-protocol/
    Cargo.toml
    src/
      lib.rs
      cbor.rs
      control.rs
      frame.rs
      opcode.rs
      request.rs
      response.rs
      vreg.rs
      value.rs

  victron-domain/
    Cargo.toml
    src/
      lib.rs
      device.rs
      measurement.rs
      quality.rs
      state.rs

  victron-bluez/
    Cargo.toml
    src/
      lib.rs
      adapter.rs
      discovery.rs
      gatt.rs
      session.rs

  victron-storage/
    Cargo.toml
    src/
      lib.rs
      database.rs
      energy.rs
      spool.rs

  victron-metrics/
    Cargo.toml
    src/
      lib.rs
      encode.rs
      metric.rs
      victoria_metrics.rs

  victron-service/
    Cargo.toml
    src/
      lib.rs
      collector.rs
      scheduler.rs
      delivery.rs
      health.rs

apps/
  victron-collector/
    Cargo.toml
    src/main.rs

  victron-cli/
    Cargo.toml
    src/main.rs

fixtures/
  protocol/
  values/

deploy/
  systemd/
    victron-collector.service
    victron-collector.example.toml
  scripts/
    install-release.sh
    verify-installation.sh

docs/
  configuration.md
  operations.md
  metrics.md
```

Do not create one crate per file or technical noun. The proposed crates each have an independently testable responsibility and a clear dependency boundary.

## Crate dependency graph

```text
victron-protocol       victron-domain
        │                    │
        ├────────┐   ┌───────┤
        ↓        ↓   ↓       ↓
victron-bluez  victron-storage  victron-metrics
        └──────────┬────────────┘
                   ↓
             victron-service
                ↙       ↘
     victron-collector  victron-cli
```

Allowed dependencies:

```text
victron-protocol -> no workspace crate
victron-domain   -> no workspace crate
victron-bluez    -> victron-protocol, victron-domain
victron-storage  -> victron-domain
victron-metrics  -> victron-domain
victron-service  -> protocol, domain, bluez, storage, metrics
apps             -> victron-service and selected leaf crates
```

Forbidden directions:

- protocol must not know BlueZ, Tokio, HTTP, SQLite, or Prometheus
- domain must not know VREG IDs, GATT UUIDs, or database schemas
- metrics must not connect to BLE
- storage must not encode Prometheus or parse CBOR
- leaf infrastructure crates must not depend on `victron-service`

## Crate responsibilities

### `victron-protocol`

Pure VE.Smart wire implementation.

Responsibilities:

- VE.Smart UUID constants
- control negotiation messages: `fa 80 ff`, `f9 80`
- outbound request encoding:
  - `getDevices`
  - `subscribe`
  - `getValues`
  - later `getPathList` and `getPathValues`
- incoming Control/Data/LastData handling
- chunk reassembly and completion detection
- concatenated CBOR item decoding
- opcode-specific response parsing
- raw VREG payload decoding, scaling, sentinel handling
- typed protocol errors without sensitive payloads in normal display output

Recommended dependency:

- `minicbor` for compact CBOR encode/decode, provided it handles the observed concatenated stream cleanly
- otherwise retain a small protocol-specific decoder based on the proven Python implementation

Keep this crate synchronous and runtime-independent. Make it `no_std + alloc` capable only if this falls out naturally; do not delay the MVP merely to advertise `no_std`.

Important public types:

```rust
pub enum Request { /* read-only operations */ }
pub enum Response { /* DeviceList, Value, PathList, PathValue, ... */ }
pub struct Reassembler { /* bounded incoming buffer */ }
pub struct VregValue { /* register, raw bytes */ }
pub enum ProtocolError { /* bounded typed variants */ }
```

Do not expose write/settings/DFU request variants.

### `victron-domain`

Canonical business values independent of the wire protocol.

Responsibilities:

- device identity configured by a stable local name
- sample timestamp
- electrical measurements with base units
- charger/load states
- source quality:
  - confirmed native
  - candidate
  - derived
  - locally integrated
- validity and freshness
- energy-counter semantics

Example shape:

```rust
pub struct Sample {
    pub observed_at: SystemTime,
    pub pv_voltage_volts: Option<Measurement<f64>>,
    pub pv_current_amperes: Option<Measurement<f64>>,
    pub pv_power_watts: Option<Measurement<f64>>,
    pub battery_voltage_volts: Option<Measurement<f64>>,
    pub battery_current_amperes: Option<Measurement<f64>>,
    pub yield_total_kwh: Option<Measurement<f64>>,
    pub charger_state: Option<ChargerState>,
}
```

Use one canonical representation. Avoid parallel raw/normalized model trees unless a test or persistence requirement needs them.

### `victron-bluez`

Linux BLE adapter using the host BlueZ daemon over D-Bus.

Recommended first choice:

- `bluer` with the BlueZ `bluetoothd` feature

Reason:

- Linux-only production target
- direct BlueZ semantics
- no need to carry cross-platform abstractions for macOS or Windows

Responsibilities:

- select configured Bluetooth adapter, normally `hci0`
- discover by bonded identity plus Victron manufacturer/service evidence
- connect to the configured device
- find VE.Smart service variant `...dfd0` or `...dfd1`
- discover Control, LastData, and Data characteristics
- enable notifications
- forward notification bytes to `victron-protocol`
- perform bounded writes and reads
- disconnect cleanly
- classify contention, authentication, timeout, and BlueZ errors

The application does not perform PIN automation. Pair once with `bluetoothctl`; BlueZ owns bond material outside Git.

Define a narrow trait for tests:

```rust
#[async_trait]
pub trait BleTransport {
    async fn open(&mut self) -> Result<(), BleError>;
    async fn read_control(&mut self) -> Result<Vec<u8>, BleError>;
    async fn write_control(&mut self, data: &[u8]) -> Result<(), BleError>;
    async fn write_data(&mut self, data: &[u8]) -> Result<(), BleError>;
    async fn next_notification(&mut self) -> Result<Notification, BleError>;
    async fn close(&mut self);
}
```

If `async-trait` allocation becomes measurable, replace it later with native async traits or concrete generics. Optimize only after hardware measurement.

### `victron-storage`

Durable local state using SQLite.

Recommended dependency:

- `rusqlite`
- prefer system SQLite on Raspberry Pi OS if reproducible packaging is straightforward
- use bundled SQLite only if cross-linking and deployment are more reliable and measured size remains acceptable

Responsibilities:

- schema migration at startup
- failed VictoriaMetrics batch spool
- last successfully delivered timestamp
- optional local cumulative energy state
- last valid power sample for integration
- bounded retention and transaction-safe dequeue

Minimum tables:

```text
schema_version
spool_batch(id, created_at_ms, payload, attempts, next_attempt_at_ms)
energy_state(device, total_kwh, last_power_watts, last_sample_at_ms)
collector_state(key, value)
```

Rules:

- WAL only if Pi storage and shutdown behavior are tested; otherwise use conservative journaling
- bounded database size
- one transaction per acquisition/delivery state transition, not per metric
- no PIN, PUK, BLE key, raw device secret, or unbounded raw capture
- power-loss recovery test required

### `victron-metrics`

Prometheus text generation and VictoriaMetrics client.

Responsibilities:

- stable metric names and label escaping
- conversion from domain sample to Prometheus text
- health/error counters
- explicit millisecond timestamps
- HTTP import client
- response classification for retry versus permanent failure

Target endpoint:

```text
POST http://100.64.0.2:8429/api/v1/import/prometheus
```

Use an HTTP client without TLS features for this internal HTTP endpoint, avoiding unnecessary crypto dependencies on ARMv6. A suitable candidate is `reqwest` with default features disabled, but dependency/target compatibility must be proven in the ARMv6 spike before adoption.

No `/metrics` listener in v1. The Pi makes outbound connections only.

Metric contract begins with:

```text
victron_pv_voltage_volts{device="solar-charger"}
victron_pv_current_amperes{device="solar-charger"}
victron_pv_power_watts{device="solar-charger"}
victron_battery_voltage_volts{device="solar-charger"}
victron_battery_current_amperes{device="solar-charger"}
victron_yield_total_kwh{device="solar-charger"}
victron_charger_state{device="solar-charger",state="bulk"} 1
victron_ble_up{device="solar-charger"}
victron_ble_rssi_dbm{device="solar-charger"}
victron_last_success_unixtime{device="solar-charger"}
victron_sample_age_seconds{device="solar-charger"}
victron_ble_connect_failures_total{device="solar-charger"}
victron_protocol_errors_total{device="solar-charger"}
victron_spool_batches{device="solar-charger"}
```

Keep labels bounded. Do not use raw register IDs, MAC addresses, error messages, or payloads as labels.

### `victron-service`

Application use cases and orchestration.

Responsibilities:

- collector state machine
- scan/connect/negotiate/subscribe/request/collect/disconnect cycle
- poll scheduling
- retry/backoff with deterministic bounds
- sample normalization
- native energy-counter selection
- fallback energy integration policy
- spool and delivery coordination
- health-state transitions
- shutdown handling

Suggested collector state machine:

```text
Idle
 → Discovering
 → Connecting
 → Subscribing
 → Negotiating
 → Requesting
 → Collecting
 → Persisting
 → Delivering
 → Disconnecting
 → Idle

Any state
 → Backoff
 → Discovering
```

Use Tokio current-thread runtime unless hardware measurements prove multiple worker threads are beneficial. BLE and HTTP are I/O-bound; the Pi Zero W has one CPU core.

Keep queue capacities small and explicit. One device and one in-flight acquisition cycle are sufficient.

### `victron-collector`

Production daemon binary.

Responsibilities only:

- parse validated TOML configuration
- initialize tracing/journald-friendly logs
- open database
- create concrete BlueZ/storage/metrics adapters
- start `victron-service`
- handle SIGTERM/SIGINT
- exit nonzero on unrecoverable configuration or schema errors

No business logic in `main.rs`.

Suggested configuration:

```toml
[device]
name = "solar-charger"
bluez_alias = "Solar Charger"
instance = 3
adapter = "hci0"

[poll]
active_interval_seconds = 15
idle_interval_seconds = 60
response_timeout_seconds = 8
maximum_energy_gap_seconds = 300

[victoria_metrics]
url = "http://100.64.0.2:8429/api/v1/import/prometheus"
request_timeout_seconds = 10

[storage]
path = "/var/lib/victron-collector/state.sqlite3"
maximum_spool_batches = 10000
maximum_spool_age_days = 7
```

Do not put the Victron PIN in this file.

### `victron-cli`

Diagnostic executable reusing the same crates.

Initial commands:

```text
victron-cli adapters
victron-cli discover
victron-cli inspect --device 'Solar Charger'
victron-cli read-once --device 'Solar Charger' --instance 3
victron-cli decode-fixture <path>
victron-cli render-metrics <fixture>
victron-cli check-victoriametrics
```

This CLI replaces one-off production debugging scripts. Raw notification output requires an explicit debug flag and must redact or avoid protected material.

## Runtime data flow

One acquisition cycle:

1. scheduler chooses active or idle interval
2. BlueZ adapter resolves the bonded device
3. BLE session connects and discovers VE.Smart GATT
4. notifications start on Control, LastData, Data
5. protocol sends control negotiation
6. protocol sends `subscribe(instance=3)`
7. protocol sends one bounded `getValues` request
8. reassembler produces typed responses
9. VREG decoder converts values to domain measurements
10. validation rejects sentinels and impossible values
11. storage updates native/fallback energy state transactionally
12. metrics crate renders one timestamped batch
13. batch is written to SQLite before or atomically with delivery ownership
14. VictoriaMetrics delivery succeeds and removes the batch, or leaves it queued
15. BLE session disconnects
16. health counters and next interval are updated

The first MVP should request only the smallest validated register set. Add history/path queries later; do not increase BLE payload and parser complexity before the live path is stable.

## Native energy and fallback integration

Canonical energy priority:

1. confirmed Victron lifetime yield
2. confirmed Victron user-reset yield for diagnostics only
3. durable local integration of confirmed PV power

Fallback integration formula:

```text
energy_kwh += ((previous_watts + current_watts) / 2)
              × elapsed_seconds
              / 3_600_000
```

Skip integration when:

- either power sample is invalid
- time moves backward
- gap exceeds 300 seconds
- process has no durable previous sample
- device identity changes

Expose skipped duration; never silently manufacture energy through an outage.

## Dependency shortlist and constraints

Candidate external crates:

| Concern | Candidate | Constraint |
|---|---|---|
| async runtime | `tokio` | current-thread runtime; minimal features |
| BlueZ | `bluer` | Linux `bluetoothd`; prove ARMv6 compile/runtime |
| D-Bus transitively | selected by `bluer` | avoid separate abstraction unless needed |
| CBOR | `minicbor` | must support observed stream and indefinite items needed by fixtures |
| serialization/config | `serde`, `toml` | configuration only; avoid serialization in hot protocol path if unnecessary |
| error context | `thiserror` | typed library errors |
| app errors | `anyhow` | binaries only |
| logging | `tracing`, `tracing-subscriber` | compact text suitable for journald |
| HTTP | `reqwest` without TLS/default features | prove ARMv6 dependency graph |
| SQLite | `rusqlite` | choose system vs bundled during spike |
| time | `time` | only if std time is insufficient |
| CLI | `clap` | CLI and daemon config arguments only |

Before accepting a crate:

- inspect supported targets and transitive native libraries
- compile for `arm-unknown-linux-gnueabihf`
- check binary size and stripped dependency footprint
- avoid TLS/HTTP2/ICU/default features not needed by this service
- pin via committed `Cargo.lock`

## Cross-compilation and release

### Toolchain spike

Provide a reproducible development shell or build environment containing:

- Rust toolchain with `arm-unknown-linux-gnueabihf` target
- ARM hard-float GCC/binutils linker
- ARM sysroot compatible with the Pi OS release
- `pkg-config` cross configuration when linking system libraries

Set the linker in `.cargo/config.toml`, conceptually:

```toml
[target.arm-unknown-linux-gnueabihf]
linker = "arm-linux-gnueabihf-gcc"
```

Exact linker/package names belong in the declarative Nix development environment, not ad-hoc global installation commands.

### Build strategy

Preferred order:

1. cross-compile on the development machine
2. copy one stripped binary plus config/systemd assets to the Pi
3. execute smoke test on the Pi

Native compilation on the Pi Zero W is not the normal release path because of CPU and memory constraints.

### Compatibility verification

On every release candidate verify on the actual Pi:

```text
file victron-collector
readelf -A victron-collector
ldd victron-collector
victron-collector --version
victron-cli adapters
victron-cli read-once ...
```

Record expected glibc and shared-library requirements. Prefer dynamically linked glibc compatible with the installed Raspberry Pi OS unless a static strategy is explicitly proven smaller and more reliable.

## Deployment design

Runtime files:

```text
/usr/local/bin/victron-collector
/usr/local/bin/victron-cli
/etc/victron-collector/config.toml
/var/lib/victron-collector/state.sqlite3
```

Systemd behavior:

- starts after `bluetooth.service` and network readiness
- dedicated `victron-collector` user
- supplementary group/permissions required for BlueZ D-Bus only
- read/write access only to `/var/lib/victron-collector`
- restart on failure with delay
- bounded memory and process count
- writable paths restricted
- no inbound socket
- logs to journald

Do not over-harden the first unit before confirming BlueZ D-Bus access. Add restrictions incrementally and verify BLE after each restriction.

Network exposure:

- outbound HTTP from Pi to `100.64.0.2:8429`
- no listener, NodePort, port-forward, tunnel, or public endpoint
- verify the VictoriaMetrics destination is reachable only through intended LAN/tailnet paths

## Testing strategy

### `victron-protocol`

Fixture-driven unit tests:

- exact CBOR request bytes
- Control opcode decoding
- Data/LastData chunk reassembly
- concatenated CBOR values
- DeviceList/Value/Path response decoding
- signed and unsigned VREG scaling
- sentinels and truncated frames
- maximum incoming buffer enforcement

Every captured BLE regression becomes a sanitized binary fixture plus expected typed response.

### `victron-domain`

- validation ranges
- derived current/power calculations
- quality precedence
- charger-state mapping

### `victron-storage`

- migrations from empty database
- enqueue/dequeue transactionality
- retry scheduling
- maximum spool pruning
- process restart without duplicate energy
- corrupt or interrupted write handling

### `victron-metrics`

- golden Prometheus output
- label escaping
- timestamps
- NaN/invalid omission
- HTTP retry classification

### `victron-service`

Use fake BLE, storage, clock, and delivery adapters:

- successful cycle
- discovery timeout
- authentication failure
- VictronConnect contention/disconnect
- malformed protocol response
- delivery outage and ordered replay
- graceful SIGTERM between acquisition and delivery
- active/idle interval transition

### Hardware tests

On Pi Zero W:

- ten reconnect/read cycles
- 30-minute coexistence test while VictronConnect is opened and closed
- Bluetooth service restart
- device moves out of range and returns
- network loss and spool replay
- Pi reboot and automatic recovery
- storage power-loss simulation using safe test data
- 24-hour run, then 7-day soak

## Implementation phases

### Phase 0 — ARMv6 and BlueZ feasibility spike

- [ ] Create minimal Cargo workspace and release profile.
- [ ] Add reproducible ARMv6 cross toolchain configuration.
- [ ] Compile and run a hello-world binary on the Pi Zero W.
- [ ] Compile `bluer`; list adapter and bonded device through D-Bus.
- [ ] Compile HTTP client with TLS/default features disabled; POST a test metric to a non-production test series.
- [ ] Compile chosen SQLite mode and perform create/write/reopen test.
- [ ] Measure stripped binary size and idle RSS.

**Exit criterion:** one ARMv6 binary proves BlueZ, HTTP, SQLite, and graceful shutdown on the real Pi. If any selected dependency fails, replace it before building the application layers.

### Phase 1 — protocol crate from existing evidence

- [ ] Port request encoding and response decoding from the Python scripts.
- [ ] Add captured sanitized fixtures.
- [ ] Implement bounded chunk reassembly.
- [ ] Port confirmed VREG decoders first.
- [ ] Cross-check Rust output against Python output for identical fixtures.

**Exit criterion:** Rust and Python produce the same requests and normalized values for all accepted fixtures.

### Phase 2 — diagnostic CLI and live read

- [ ] Implement `victron-domain`.
- [ ] Implement `victron-bluez` transport.
- [ ] Build `victron-cli read-once`.
- [ ] Pair once through BlueZ outside the application.
- [ ] Validate at least 20 live samples against VictronConnect.
- [ ] Identify and confirm native lifetime yield.

**Exit criterion:** repeated CLI reads return confirmed PV power, PV voltage, battery voltage, charger state, and lifetime yield.

### Phase 3 — durable collector MVP

- [ ] Implement `victron-storage` schema and spool.
- [ ] Implement `victron-metrics` rendering and import client.
- [ ] Implement the `victron-service` state machine.
- [ ] Build production `victron-collector` binary.
- [ ] Add structured health metrics and bounded retries.

**Exit criterion:** 24-hour unattended run with at least 99% of expected acquisition cycles either delivered or durably queued, with no manual restart.

### Phase 4 — declarative release and deployment

- [ ] Add reproducible release build.
- [ ] Add systemd unit and install/verification scripts.
- [ ] Keep host configuration declarative in the appropriate infrastructure source.
- [ ] Verify reboot recovery, BlueZ restart recovery, and spool replay.
- [ ] Record exact runtime dependencies and rollback steps.

**Exit criterion:** clean install and rollback on the actual Pi, with automatic recovery after reboot.

### Phase 5 — Grafana integration

- [ ] Add provisioned dashboard under `infra/nomad/jobs/monitoring/grafana/dashboards/energy/`.
- [ ] Show live power, voltages/current, charger state, cumulative energy delta, and sample age.
- [ ] Compare Victron DC yield with OpenDTU AC yield only after semantics are confirmed.
- [ ] Validate Europe/Berlin day boundaries and counter behavior.

**Exit criterion:** Grafana range totals agree with VictronConnect/source counters within documented tolerance, without manual dashboard edits.

### Phase 6 — hardening

- [ ] Seven-day soak test.
- [ ] Tune connection lifecycle and polling interval.
- [ ] Add daylight-aware stale-data alerts.
- [ ] Add database/spool maintenance.
- [ ] Document pairing replacement and operations.
- [ ] Decide whether path-based 30-day history is still needed.

**Exit criterion:** stable seven-day service with understood BLE contention and accepted data completeness.

## MVP scope

Include:

- one configured Victron device
- one BlueZ adapter
- service UUID variants `dfd0` and `dfd1`
- read-only negotiation, subscription, and `getValues`
- confirmed live metrics
- native lifetime yield when confirmed
- optional local energy fallback
- VictoriaMetrics push with durable spool
- health metrics
- diagnostic CLI
- systemd service

Exclude:

- configuration writes
- PIN/PUK manipulation
- DFU
- web UI
- public or local HTTP server
- multi-device scheduling
- automatic Grafana API mutation
- speculative support for all Victron product families
- full 30-day path history until the live pipeline is stable

## First vertical slice

Implement in this exact order:

1. workspace, cross toolchain, ARMv6 hello world
2. `victron-protocol` request/fixture tests
3. `victron-bluez` plus `victron-cli read-once`
4. validate five core values against VictronConnect
5. `victron-metrics` push of one timestamped sample
6. `victron-storage` durable spool
7. `victron-service` periodic state machine
8. deploy systemd service
9. 24-hour soak
10. provision first Grafana dashboard

This keeps every layer attached to a working end-to-end product and avoids building history, abstractions, or dashboards before the ARMv6 BLE path is proven.
