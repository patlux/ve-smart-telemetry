# victron-metrics

Prometheus text encoding and VictoriaMetrics import client for the Victron
BLE collector (Raspberry Pi Zero W / `arm-unknown-linux-gnueabihf`).

Part of the Rust multi-crate workspace planned in
`analysis/rust-multicrate-collector-plan.md`. This crate has **no BLE, storage,
or domain-model coupling**; it only renders samples as Prometheus text and
delivers them to VictoriaMetrics.

## Public API

| Item | Purpose |
|---|---|
| `MetricBatchBuilder` | deterministic batch builder; `device` label always applied; ergonomic `gauge` / `counter` / `state` helpers; strict positive-ms timestamp required before adding points |
| `MetricPoint` | validated single sample (finite value, strictly positive ms timestamp, kind) |
| `MetricName`, `MetricKind`, `MetricError` | validation types |
| `names` | pinned metric contract (names + label keys + bounded charger-state vocabulary) |
| `encode` / `escape_label_value` / `format_value` | text encoding primitives |
| `adapter::SampleView` | plain-data integration shim; `TryFrom<SampleView> for MetricBatchBuilder` |
| `domain` (feature) | fills measurement fields of a caller-supplied `SampleView` from `victron_domain::Sample` (workspace-only; never fabricates health) |
| `VictoriaMetricsClient` | async HTTP/1.1 client for `POST /api/v1/import/prometheus` |
| `Outcome`, `ImportFailure`, `FailureCategory` | delivery classification |

Example:

```rust
let mut batch = MetricBatchBuilder::new("solar-charger")?
    .with_timestamp_ms(1_700_000_000_123)?;   // strictly positive Unix ms
batch.gauge(victron_metrics::names::PV_POWER_WATTS, 136.4)?;
batch.state(victron_metrics::names::CHARGER_STATE, victron_metrics::names::states::BULK)?;
let text = batch.encode();
```

Or fill a `SampleView` and convert:

```rust
let batch = MetricBatchBuilder::try_from(sample_view)?;   // one acquisition cycle
let text = batch.encode();
```

## Metric contract decisions

- Names/units pinned in `src/names.rs`, taken verbatim from
  `analysis/grafana-integration-plan.md` (base SI units).
- `victron_charger_state` is a **state label** (`state="bulk"`) with value `1`,
  bounded to `[a-z0-9_]`, ≤32 bytes (see `names::states`). No numeric enum on
  the wire.
- Counters are "typed" by construction: `MetricKind::Counter` plus a mandatory
  `_total` name suffix (enforced). The caller supplies the cumulative value;
  the builder never auto-suffixes. `victron_yield_total_kwh` (no `_total`
  suffix) is therefore constructed as a gauge even though it is cumulative —
  the text format carries no TYPE metadata, and Grafana reads it as a
  monotonic series on the query side.
- `device` is the only always-present label and is always first. Extra labels
  are sorted alphabetically. No MAC, register ID, error text, or payload is
  ever used as a label.
- Non-finite values (NaN, ±Inf) are never encoded: direct `MetricPoint`
  construction rejects them (`MetricError::NonFiniteValue`), the ergonomic
  builder helpers omit them (`Ok(false)`), and the encoder skips them as
  defense in depth. `None` fields in `SampleView` are omitted entirely.
- Explicit millisecond timestamps on every line, strictly positive Unix
  milliseconds. `MetricPoint::new` and `with_timestamp_ms` reject `<= 0`
  (`MetricError::InvalidTimestamp`); the builder starts without a timestamp
  and its helpers return `MetricError::TimestampNotSet` until
  `with_timestamp_ms` / `now` is called.
- Deterministic output: series sorted by (name, label set, timestamp); a
  true duplicate (same name, labels, **and** timestamp) collapses last-wins;
  distinct timestamps for the same series are all emitted, so no sample is
  ever lost to collapsing.
- Health fields (`ble_up`, `*_total` counters, spool depth) are emitted when
  known (`Some`), including a known zero, so dashboards can compute rates
  without gaps. `None` (unknown) omits the series: unknown health is never
  rendered as a known zero, so no fabricated health series can appear. This
  includes `victron_energy_integration_gap_seconds_total` (cumulative
  **seconds** skipped by the durable local energy integration, never silently
  bridged — not a gap-event count).
- `victron_yield_today_kwh` is diagnostic only (daily reset); the canonical
  Grafana cumulative series is `victron_yield_total_kwh`.
- `victron_spool_batches` and `victron_spool_oldest_age_seconds` are gauges
  supplied by the storage layer (this crate only renders them); the oldest-age
  series is omitted when the spool is empty.

## Footprint (ARMv6)

The internal endpoint is plaintext HTTP
(`http://100.64.0.2:8429/api/v1/import/prometheus`), so TLS and the whole
reqwest/hyper/rustls/native-tls tree are **not** pulled in. The import client
speaks HTTP/1.1 directly on `tokio::net::TcpStream` (`Connection: close`,
explicit `Content-Length`, bounded error bodies, per-read and overall
deadlines). Only plain IPv4/DNS `http://` URLs are accepted; `https://`,
IPv6 brackets, userinfo, query/fragment, and any host/path bytes that could
corrupt the request line or `Host` header (control characters, whitespace,
`@`, `"` `<>\\^`{|}#?`) are rejected at construction with a clear error.

Runtime dependency graph (verified with
`cargo check --target arm-unknown-linux-gnueabihf`):

```text
victron-metrics
└── tokio (features: net, time, io-util; no default features)
    ├── bytes
    ├── libc
    ├── mio
    ├── pin-project-lite
    └── socket2
```

No listener is ever created; the Pi makes outbound connections only.

## Import client behavior

- Request: `POST <path> HTTP/1.1` with `Host`, `Content-Type: text/plain;
  version=0.0.4`, `Content-Length`, `Connection: close`, then the batch body.
- Response parsing: status line + headers, body via `Content-Length`,
  chunked transfer, or close-delimited (bounded). Interim 1xx responses
  (e.g. `100 Continue`) are informational, never final: the client consumes a
  bounded number of them and keeps reading until the final response (a
  `100`-only peer that stays open therefore times out and is retryable, not
  permanent). `101 Switching Protocols` is treated as unsupported/malformed.
  204/304 never carry a body and are returned immediately without waiting for
  peer close.
- Classification:
  - 2xx → `Outcome::Success`
  - 408, 429, 5xx, network/timeout/malformed → `Outcome::Retryable`
  - other 4xx → `Outcome::Permanent`
- Failure messages are bounded (~300 chars of the error body) and never
  contain sensitive payloads. Retry/backoff scheduling belongs to the
  service layer (`victron-service`), not here.

## Tests

```sh
cargo test                      # unit + golden + HTTP-mock + doc tests
cargo clippy --all-targets      # zero warnings
cargo fmt --check
cargo check --target arm-unknown-linux-gnueabihf   # ARMv6 dependency proof
```

HTTP tests run against an ephemeral `127.0.0.1` mock server (OS-assigned
port) covering success, 400/429/503, chunked error bodies, malformed
responses, read timeouts, closed connections, and connection refused. The
real production endpoint (`100.64.0.2:8429`) is never contacted.

## Workspace integration

1. Add `crates/victron-metrics` to the workspace `members`.
2. Optional domain wiring: the `domain` feature is wired to the sibling
   `victron-domain` crate. `src/domain.rs` fills the measurement
   and charger-state fields of a **caller-supplied** `SampleView` from a real
   `victron_domain::Sample`; the caller's view already carries the real
   health context (BLE link state, cumulative counters, spool depth) from the
   service layer, and the domain sample never fabricates health series —
   pass a view with `None` health fields when there is no health context.
   All `ChargerState` variants (`Unknown(u8)` / `StartingUp` /
   `AutoRecondition` / `ExternalControl`) are mapped. Note: `victron-domain`
   requires rustc ≥ 1.83, so the `domain` feature raises the effective MSRV
   above the default build's declared 1.75.
3. The release profile (`opt-level = "s"`, `lto = "thin"`, `panic = "abort"`,
   `strip = "symbols"`) belongs in the workspace root `Cargo.toml`, not here.
