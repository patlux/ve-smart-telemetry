# victron-service

Application orchestration for the Victron VE.Smart BLE collector (Raspberry Pi
Zero W → VictoriaMetrics). This crate owns the collector *use cases* and **no
protocol, BLE, database or HTTP logic**. Everything external is behind narrow
port traits; the binaries inject concrete adapters.

The canonical domain model (`Sample`, `ChargerState`, `DeviceId`, `Quality`)
comes from `victron-domain` and is re-exported here; this crate owns **no
parallel sample model**.

This crate compiles standalone and its full test suite runs against fake ports,
because the sibling crates (`victron-bluez`, `victron-protocol`,
`victron-domain`, `victron-storage`, `victron-metrics`) are being built in
parallel lanes.

## Bounded state machine

```text
Idle -> Discovering -> Connecting -> Negotiating -> Subscribing -> Requesting
     -> Collecting -> Persisting -> Delivering -> Disconnecting -> Idle
```

- `Backoff` is reachable from every working phase.
- `Disconnecting` (graceful teardown) is reachable from every working phase.
- `ShuttingDown` is terminal.
- Illegal transitions are rejected by `StateMachine` (`state.rs`) and surface
  as typed errors (`CycleError::State`, `RunError::State`) instead of being
  silently swallowed in release builds.

Shutdown (`watch` channel) is honoured at the Persisting→Delivering boundary:
an in-flight acquisition completes, the sample is durably persisted, delivery
is skipped, the session is torn down, and the runner exits `graceful`. A
**closed** shutdown channel (sender dropped) is also treated as shutdown, so a
vanished sender task can never leave the daemon polling forever.

## Atomic acquisition persistence

One acquisition is committed with a single `StoragePort::commit_acquisition`
call: the next energy state, the acquisition identity (`observed_at`) and the
rendered batch persist in **one transaction** (the parent storage adapter
requires one new SQLite transaction/API addition). The commit is idempotent
per `(device, observed_at)`: reprocessing the same observed timestamp is a
no-op — nothing double-counts and no second batch is enqueued. Reads needed to
prepare the commit (`energy_state`, `last_success`) stay separate.

Energy integration uses the sample's own `observed_at` — never a later
orchestration clock reading — so delayed processing cannot manufacture energy.
Native yield is used only when its quality is `ConfirmedNative`; only
confirmed PV power is integrated or stored as the anchor. Pre-epoch timestamps
are rejected at the persistence seam.

## Ports (narrow traits)

| Port | Trait | Sibling crate |
|---|---|---|
| BLE session | `BleSession` (`?Send`) | `victron-bluez` |
| Protocol request/response | `ProtocolAdapter` | `victron-protocol` + `victron-domain` |
| Durable state + spool | `StoragePort` | `victron-storage` |
| Metrics delivery | `MetricsDelivery` | `victron-metrics` |
| Batch rendering | `BatchRenderer` (`RenderContext`) | `victron-metrics` |
| Wall clock | `Clock` | std (`SystemClock`) |
| Interval policy | `IntervalPolicy` | `SolarActivityPolicy` (service) |
| Backoff policy | `BackoffPolicy` | `ExponentialBackoff` (deterministic) |

The BLE session trait is `?Send`-friendly to match the BlueZ lane's
`victron_bluez::BleTransport`; the collector stays one-device/current-thread.

## Delivery ownership contract

Batches are claimed one at a time; a claimed batch is not handed out again
until the claim expires (`spool_claim_ttl`). A claimed batch's `attempts` is
the **current 1-based attempt** (claiming increments the stored counter); the
service never adds another attempt. `spool_complete` runs only after the
network call succeeds → at-most-once under crash + TTL recovery. Retries are
bounded by `spool_max_attempts`; a batch is dropped when the current attempt
reaches the maximum, or immediately on a permanent rejection. Drops use the
dedicated `spool_drop` operation and **never** increment a delivered counter.
Retry classification matches `victron-metrics`: network, timeout, malformed
response, HTTP 408/429 and 5xx retry; other 4xx are permanent.

## Scheduling

Active/idle cadence is driven by the **last successfully committed sample's
confirmed solar activity** (`SolarActivityPolicy`, threshold in watts), not by
a UTC hour window. The first cycle uses the active cadence. Backoff is
deterministic and bounded (`min(cap, base * factor^(n-1))`).

## Tests

```text
cargo test                    # 37 unit + 22 integration tests
```

Covered scenarios: success cycle, BLE phase timeout, BLE contention, malformed
response, delivery outage with oldest-first replay + exactly-once ownership,
bounded retries (exact attempts 1..max), permanent-error drop, drop-never-
counts-as-delivery, graceful shutdown between acquisition and delivery,
closed-sender shutdown, teardown after failures, atomic all-or-nothing commit
failure, idempotent duplicate replay, pre-epoch rejection, delayed processing,
truthful render context, 600 s energy-gap seconds, solar active→idle→active
cadence, config validation, energy fallback integration, state-transition
legality.
