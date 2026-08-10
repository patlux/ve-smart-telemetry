# Victron BLE → VictoriaMetrics → Grafana plan

## Goal

Run an always-on application near the Victron device that reads VE.Smart BLE values and sends normalized Prometheus metrics to the existing VictoriaMetrics instance. Grafana then displays live electrical values, cumulative energy, daily production, BLE health, and data quality.

The collector talks to the Victron **device**, not to the VictronConnect application. VictronConnect remains useful for pairing, configuration, and validation, but can contend for the same BLE connection.

## Existing environment

- Reverse-engineered protocol and Python experiments:
  - `scripts/read-victron-live-values.py`
  - `scripts/read-victron-history.py`
  - `analysis/victronconnect-protocol-reference.md`
- Target BLE device observed as `Solar Charger`, VE.Smart instance `3`.
- Existing metrics backend:
  - VictoriaMetrics on `metrics-node`
  - Prometheus-compatible import endpoint: `http://100.64.0.2:8429/api/v1/import/prometheus`
  - retention: 2 years
- Existing Grafana source of truth:
  - `~/dev/Personal/infra/nomad/jobs/monitoring/grafana/`
- Existing energy dashboards already use VictoriaMetrics and cumulative energy counters.

## Recommended architecture

```text
Victron MPPT / charger
  VE.Smart BLE GATT
        ↓
Raspberry Pi Zero W close to the device
  victron-ble-exporter
  - BlueZ pairing/bonding
  - BLE scan/connect/subscribe/read
  - VE.Smart control + CBOR protocol
  - VREG decoding and validation
  - local state/spool
  - Prometheus metric generation
        ↓ outbound HTTP only
VictoriaMetrics import API on metrics-node
        ↓
Grafana energy dashboard
```

Use a **push collector**, not a remotely scraped `/metrics` server. This avoids opening a new inbound LAN/tailnet port on the Raspberry Pi and tolerates temporary loss of connectivity to `metrics-node` through a small local spool.

## Technology choice

### Collector

The production collector will be a Rust multi-crate Cargo workspace targeting Raspberry Pi Zero W ARMv6. The existing Python scripts remain protocol-reference and fixture-capture tools, not the deployed daemon.

Detailed crate boundaries, dependency graph, ARMv6 cross-compilation strategy, deployment, and phased implementation are defined in:

- `analysis/rust-multicrate-collector-plan.md`

Core production choices:

- Rust target `arm-unknown-linux-gnueabihf`
- Linux BlueZ access through a dedicated `victron-bluez` crate
- pure runtime-independent `victron-protocol` crate
- SQLite durable state/spool through `victron-storage`
- Prometheus/VictoriaMetrics output through `victron-metrics`
- orchestration in `victron-service`
- one production daemon plus one diagnostic CLI
- systemd service on Raspberry Pi OS, or a declarative NixOS service if the Pi is moved to NixOS

### Deployment

First production target: Raspberry Pi Zero W near `Solar Charger`.

- BlueZ owns the bond and reconnects to the known device.
- systemd restarts the collector after failure.
- journal contains operational messages, never PIN/PUK/bond keys.
- configuration contains device identity, instance, intervals, and VictoriaMetrics URL.
- PIN is entered once locally during BlueZ pairing; it is not stored in the repository or application config.

## Data acquisition strategy

### 1. Connection mode

Use short periodic BLE sessions rather than a permanently held connection:

1. scan for the configured device
2. connect
3. discover VE.Smart service
4. subscribe to Control, LastData, and Data
5. perform control negotiation (`fa 80 ff`, `f9 80`)
6. subscribe to instance `3`
7. request values
8. wait for a complete response
9. disconnect

Recommended initial interval: 15 seconds while solar power is active, 60 seconds while idle. Add jitter and exponential backoff after failures.

This reduces competition with VictronConnect. If runtime tests prove that repeated bonding/connection setup is unreliable or expensive, switch to a continuous connection with an explicit disconnect window after VictronConnect contention.

### 2. First supported values

Use only read operations. Do not implement settings, PIN changes, PUK operations, or DFU.

Initial live metrics:

| Metric | Source candidate | Unit |
|---|---|---:|
| PV voltage | VREG `0xEDBB` | V |
| PV current | VREG `0xEDBD`, or derive `power / voltage` | A |
| PV power | VREG `0xEDBC` | W |
| Battery voltage | VREG `0xED8D` | V |
| Battery current | VREG `0xED8C` | A |
| Charger state | VREG/path mapping still to validate | enum |
| Load state | VREG `0xEDA8` candidate | enum |
| Load current | VREG `0xEDAD` candidate | A |
| Load power | VREG `0xEDAA`, or derive `voltage × current` | W |
| Lifetime yield | `/Yield/System` or matching VREG | kWh |
| User-reset yield | `/Yield/User` or matching VREG | kWh |
| Yield today | history path/VREG after validation | kWh |

Values marked candidate must not be published as production energy data until compared with the VictronConnect UI across several operating points.

### 3. Energy-counter policy

Grafana energy calculations need a monotonically increasing cumulative counter.

Priority:

1. **Preferred:** publish the Victron lifetime-yield counter after its VREG/path and scaling are confirmed.
2. **Secondary:** publish the Victron user-reset counter as diagnostic data, not as the canonical long-term counter.
3. **Fallback only:** integrate PV power locally into a durable cumulative kWh counter.

If local integration is needed:

- trapezoidal integration between valid samples
- reject gaps longer than a configurable threshold, initially 5 minutes
- persist the accumulator and last sample transactionally in SQLite
- expose gap and estimated-energy metrics
- never silently bridge long outages
- later replace the synthetic counter with the native lifetime counter once decoded

Do not use “yield today” as the primary Grafana counter; it resets each day and creates awkward reset handling.

## Prometheus metric contract

Use stable base units and low-cardinality labels.

```text
victron_pv_voltage_volts{device="solar-charger"}
victron_pv_current_amperes{device="solar-charger"}
victron_pv_power_watts{device="solar-charger"}
victron_battery_voltage_volts{device="solar-charger"}
victron_battery_current_amperes{device="solar-charger"}
victron_load_power_watts{device="solar-charger"}
victron_yield_total_kwh{device="solar-charger"}
victron_yield_today_kwh{device="solar-charger"}
victron_charger_state{device="solar-charger",state="bulk"} 1

victron_ble_up{device="solar-charger"}
victron_ble_rssi_dbm{device="solar-charger"}
victron_last_success_unixtime{device="solar-charger"}
victron_sample_age_seconds{device="solar-charger"}
victron_ble_connect_failures_total{device="solar-charger"}
victron_protocol_errors_total{device="solar-charger"}
victron_samples_dropped_total{device="solar-charger",reason="invalid_value"}
victron_energy_integration_gap_seconds_total{device="solar-charger"}
```

Rules:

- no MAC address, exception text, register ID, or raw payload as unbounded labels
- state represented by a bounded state label or a documented numeric enum
- timestamps assigned at collector read time
- omit invalid/sentinel readings instead of exporting impossible values
- push batches with explicit timestamps to VictoriaMetrics

## VictoriaMetrics delivery

Write Prometheus text batches to:

```text
POST http://100.64.0.2:8429/api/v1/import/prometheus
```

Delivery behavior:

- batch one acquisition cycle into one request
- connect/read timeout
- retry with exponential backoff
- store failed batches in SQLite
- replay oldest batches first
- bounded spool size and age
- deduplicate by timestamp/series; VictoriaMetrics already has a 5-second deduplication interval
- expose spool depth and oldest queued-sample age

The endpoint is currently reachable on the tailnet address. Deployment must verify that the Pi has the intended route and that no public Internet exposure is introduced.

## Grafana plan

Create a provisioned dashboard in the infra repository, for example:

```text
~/dev/Personal/infra/nomad/jobs/monitoring/grafana/dashboards/energy/victron-solar-charger.json
```

Dashboard sections:

### Overview

- PV power now
- production today
- lifetime production
- charger state
- BLE/data freshness status

### Live electrical values

- PV voltage/current/power
- battery voltage/current
- load power/current
- state timeline

### Energy

- production over selected range from cumulative counter
- daily production bars for 30/90 days
- comparison with OpenDTU AC yield
- conversion/loss estimate: Victron DC yield minus OpenDTU AC yield, only after source semantics are confirmed

Example PromQL:

```promql
victron_pv_power_watts{device="solar-charger"}
```

```promql
clamp_min(delta(victron_yield_total_kwh{device="solar-charger"}[$__range]), 0)
```

```promql
(clamp_min(delta(victron_yield_total_kwh{device="solar-charger"}[1d]), 0))[30d:1d]
```

For counters that VictoriaMetrics treats conventionally, compare `increase()` and `delta()` against known Victron UI totals before choosing the final dashboard query. Existing home energy dashboards use both patterns depending on source behavior.

Add recording rules only if dashboard queries become expensive or counter-reset normalization needs a canonical expression.

## Operational alerts

Add alerts after one week of stable observations:

- no successful sample for 10 minutes during daylight
- BLE connection failures above threshold
- collector spool growing for 15 minutes
- native energy counter decreases unexpectedly
- impossible voltage/current/power values
- power remains zero despite high solar radiation, as a warning rather than immediate fault

Suppress expected nighttime zero-power alerts using the existing Open-Meteo solar/daylight metrics.

## Implementation phases

### Phase 0 — device and host readiness

- [ ] Confirm the target host is the Raspberry Pi Zero W and it runs a supported BlueZ stack.
- [ ] Confirm the Pi is within reliable BLE range.
- [ ] Pair/bond `Solar Charger` once through `bluetoothctl` using local hidden PIN input.
- [ ] Confirm the Pi can reach VictoriaMetrics on `100.64.0.2:8429`.
- [ ] Record the stable BlueZ device identity without committing credentials.

**Exit criterion:** ten manual BLE reads succeed over 30 minutes, including reconnects.

### Phase 1 — protocol fixture and value validation

- [ ] Refactor current scripts into reusable protocol modules.
- [ ] Capture sanitized notification fixtures from the owned device.
- [ ] Validate every scaling rule against the VictronConnect screen.
- [ ] Identify and confirm the native lifetime-yield value.
- [ ] Confirm behavior when VictronConnect is opened concurrently.

**Exit criterion:** PV power, PV voltage, battery voltage, state, and lifetime yield match VictronConnect within documented tolerances.

### Phase 2 — MVP collector

- [ ] Build asynchronous scan/connect/read/disconnect loop.
- [ ] Add config validation and structured logs.
- [ ] Add VictoriaMetrics batch push.
- [ ] Add BLE and exporter health metrics.
- [ ] Add SQLite state/spool.
- [ ] Add unit tests from captured fixtures.

**Exit criterion:** 24-hour unattended run with at least 99% expected samples delivered and no manual restart.

### Phase 3 — declarative deployment

- [ ] Package the collector for the Pi.
- [ ] Add systemd/Nix service with restart policy and constrained permissions.
- [ ] Keep secrets outside Git; pairing material stays in BlueZ storage.
- [ ] Add log retention and resource limits suitable for Pi Zero W.
- [ ] Verify reboot recovery and offline spool replay.

**Exit criterion:** collector resumes automatically after Pi and network reboots.

### Phase 4 — Grafana dashboard

- [ ] Create provisioned dashboard JSON under `dashboards/energy/`.
- [ ] Add live, energy, health, and OpenDTU comparison panels.
- [ ] Validate all range calculations against VictronConnect totals.
- [ ] Deploy through the repository-supported Grafana sync/deploy process.

**Exit criterion:** selected-day and selected-range energy totals agree with source counters and no panel depends on manual Grafana edits.

### Phase 5 — hardening

- [ ] Add daylight-aware freshness alerts.
- [ ] Run for 7 days and measure connection success, missing intervals, and contention.
- [ ] Tune poll interval and reconnect strategy.
- [ ] Document recovery, pairing replacement, and device migration.
- [ ] Decide whether historical 30-day Victron records add value beyond VictoriaMetrics retention.

**Exit criterion:** seven-day stable operation with accepted data completeness and documented failure handling.

## Tests and verification

### Protocol tests

- CBOR request encoding snapshots
- chunk reassembly across Data/LastData notifications
- response opcode parsing
- signed/unsigned/scaling fixtures
- sentinel and malformed payload rejection

### Energy tests

- monotonic counter behavior
- native counter reset handling
- local integration with regular samples
- integration with missing samples and long gaps
- restart persistence without double counting

### Runtime tests

- Bluetooth disabled/enabled
- target unavailable
- device moves out of range
- VictronConnect takes the connection
- Pi reboots
- VictoriaMetrics unavailable and later restored
- malformed or changed firmware response

### Acceptance measurements

- compare at least 20 live samples with VictronConnect
- compare morning-to-evening yield with the Victron display
- compare 24-hour Grafana delta with source yield
- verify timestamps and timezone boundaries around Europe/Berlin midnight
- verify no PIN, PUK, raw bond key, or sensitive payload is logged

## Main risks

| Risk | Mitigation |
|---|---|
| BLE bond cannot be established on the Pi | validate BlueZ pairing first; do not build the daemon around an unproven connection |
| VictronConnect and collector contend for one connection | short polling sessions, retry/backoff, explicit contention tests |
| register scaling is still partly candidate-level | fixture tests plus UI comparison before publishing production metrics |
| native lifetime counter is not yet decoded | complete VREG/path validation; use durable integration only as an explicit fallback |
| Pi Zero W CPU/RAM constraints | small Python process, bounded buffers/spool, no web UI, measured resource limits |
| BLE identity changes or privacy addressing | resolve by bonded BlueZ identity and Victron manufacturer/service data |
| missing samples distort energy | prefer native cumulative yield; never interpolate across long gaps silently |

## Recommended first implementation slice

Build only this vertical slice first:

1. Pi bonds to `Solar Charger`.
2. Collector reads PV power, PV voltage, battery voltage, charger state, and lifetime yield every 15 seconds.
3. Collector pushes those values plus health metrics to VictoriaMetrics.
4. One temporary Grafana dashboard shows live power, lifetime-yield delta, and sample age.
5. Run for 24 hours and compare totals with VictronConnect.

Only after this works end to end should load values, historical arrays, alerts, or OpenDTU efficiency comparisons be added.
