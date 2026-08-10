# Metrics — victron-collector

The collector renders one Prometheus text batch per acquisition cycle and
POSTs it to the VictoriaMetrics import endpoint
(`POST {url}/api/v1/import/prometheus`, config key
`victoria_metrics.url`). Batches carry explicit millisecond timestamps set
at read time; VictoriaMetrics deduplicates on a 5-second interval, so
per-cycle batches are safe.

## Contract

Stable base units, low-cardinality labels. The only label is `device`,
whose value is the configured `[device] name` (e.g. `solar-charger`).

### Device values

| Metric | Type | Unit | Notes |
|---|---|---|---|
| `victron_pv_voltage_volts{device}` | gauge | V | |
| `victron_pv_current_amperes{device}` | gauge | A | native or derived |
| `victron_pv_power_watts{device}` | gauge | W | |
| `victron_battery_voltage_volts{device}` | gauge | V | |
| `victron_battery_current_amperes{device}` | gauge | A | |
| `victron_load_power_watts{device}` | gauge | W | once validated |
| `victron_yield_total_kwh{device}` | counter | kWh | native lifetime yield (preferred) |
| `victron_yield_today_kwh{device}` | counter | kWh | diagnostics only; resets daily |
| `victron_charger_state{device,state="bulk"}` | info/gauge | — | bounded state label, `1` when active |

Invalid/sentinel readings are omitted, never exported as impossible values.

### BLE / delivery health

| Metric | Type | Notes |
|---|---|---|
| `victron_ble_up{device}` | gauge | 1 when the last cycle connected |
| `victron_ble_rssi_dbm{device}` | gauge | last RSSI |
| `victron_last_success_unixtime{device}` | gauge | last successful cycle |
| `victron_sample_age_seconds{device}` | gauge | freshness for alerting |
| `victron_ble_connect_failures_total{device}` | counter | |
| `victron_protocol_errors_total{device}` | counter | |
| `victron_samples_dropped_total{device,reason="..."}` | counter | bounded reason label |
| `victron_energy_integration_gap_seconds_total{device}` | counter | time never silently bridged |
| `victron_spool_batches{device}` | gauge | undelivered batches |
| `victron_spool_oldest_age_seconds{device}` | gauge | spool freshness |

## Naming rules

- No MAC addresses, exception text, register IDs, or raw payloads as labels.
- `device` is the only free label; keep its cardinality at 1.
- Counters use `_total`; gauges carry units in the name.
- Base units everywhere (V, A, W, kWh, s) — no millivolts or milliwhours.

## Energy semantics

Preference order: native lifetime yield counter → user-reset yield
(diagnostics) → durable local trapezoidal integration of PV power
(fallback). The fallback rejects gaps longer than
`maximum_energy_gap_seconds` and never bridges outages silently
(`victron_energy_integration_gap_seconds_total` tracks skipped time).
Grafana should read `victron_yield_total_kwh` as a monotonic counter; use
`delta()`/`increase()` with `clamp_min` to absorb reset behaviour.

## Query examples (VictoriaMetrics / Grafana)

```promql
victron_pv_power_watts{device="solar-charger"}
victron_sample_age_seconds{device="solar-charger"}
clamp_min(delta(victron_yield_total_kwh{device="solar-charger"}[$__range]), 0)
(clamp_min(delta(victron_yield_total_kwh{device="solar-charger"}[1d]), 0))[30d:1d]
```

Validate day totals against the VictronConnect display and Europe/Berlin
midnight boundaries before relying on `increase()` versus `delta()`.

## Delivery

- One acquisition cycle → one HTTP batch with explicit timestamps.
- Failed batches are stored in SQLite and replayed oldest-first.
- Spool is bounded (`maximum_spool_batches`, `maximum_spool_age_days`).
- Retention at VictoriaMetrics: 2 years (existing backend).
- The import endpoint is plain HTTP on the tailnet (no TLS/auth today);
  see `security.md` for reachability and verification.
