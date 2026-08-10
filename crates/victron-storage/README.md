# victron-storage

Durable SQLite persistence for the Victron BLE collector (Raspberry Pi Zero W).

A small, **synchronous** library called by `victron-service` through
`tokio::task::spawn_blocking`. It is deliberately independent of BLE, HTTP and
Prometheus: it stores opaque byte payloads and exposes small storage DTOs
(`SpoolBatch`, `EnergySample`, `EnergyState`, `KvEntry`, ...) that the service
maps onto its own domain types. It does **not** depend on `victron-domain` and
can be built on its own.

## What it provides

| Area | API | Notes |
|---|---|---|
| Schema migration | `Storage::open` | Idempotent, `PRAGMA user_version`-based, one transaction per migration, refuses newer databases |
| Outbound spool | `enqueue_batch`, `peek_oldest_batch`, `claim_oldest_batch`, `mark_batch_delivered`, `record_batch_retry`, `prune_spool`, `spool_stats` | FIFO replay, crash-safe lease, bounded attempts + exponential backoff, bounded pruning by count/age |
| KV state | `get_state`, `set_state`, `get_state_i64`, `set_state_i64`, `get_state_entries` | For last-delivered timestamp, counters, etc. |
| Energy integration | `integrate_energy`, `get_energy`, `reset_energy` | Transaction-safe trapezoidal integration per device with explicit skip reasons |

## Spool semantics (durable, ordered, at-least-once)

1. `enqueue_batch` writes the batch ready for delivery.
2. `claim_oldest_batch` atomically leases the oldest ready batch: it bumps the
   attempt count and schedules the batch `spool_inflight_ms` in the future. A
   crash between claim and delivery can therefore never lose the batch — it
   simply becomes claimable again after that window.
3. The caller POSTs the payload, then either `mark_batch_delivered` (row
   removed, `spool.delivered_total` bumped) or `record_batch_retry`
   (exponential backoff, or the row is dropped at `max_spool_attempts` with
   `spool.dropped_total` bumped).
4. `prune_spool` enforces the configured count, age and attempt-budget bounds.

Delivery is at-least-once: a crash between a successful HTTP POST and the
delivery commit causes one duplicate push. VictoriaMetrics' timestamp
deduplication absorbs this. Batches are ordered by `(created_at_ms, id)`.

## Energy integration (fallback counter)

The accumulator and the last-sample anchor commit in **one** transaction. A
sample is integrated only when its timestamp is strictly newer than the stored
anchor, so re-processing the same sample after a crash is always a no-op —
the fallback counter never double-counts across restarts.

Trapezoid: `energy_kwh += ((prev_w + curr_w) / 2) * elapsed_s / 3_600_000`.

Explicit skip reasons (`EnergyOutcome::Skipped { reason }`):

- `FirstSample` — no durable anchor yet; sample stored as anchor, nothing added.
- `InvalidPower` — NaN/±inf or outside the configured power bounds; no
  anchor is created or moved, so an invalid first sample leaves no state.
- `BackwardTime` — sample timestamp not strictly newer (also the restart
  double-count guard).
- `GapTooLarge` — gap exceeds `energy_gap_threshold_ms` (default 5 min); the
  anchor is reset but no energy is added, so outages are reported, never
  silently bridged.

## Durability choices (SD card / power loss)

All settings are explicit and conservative by default:

| Setting | Default | Why |
|---|---|---|
| `journal_mode` | `DELETE` | No WAL sidecar files, no checkpointing; simplest power-loss story on SD cards. |
| `synchronous` | `FULL` | Every commit is fsynced; maximum crash durability. |
| Transactions | `BEGIN IMMEDIATE` | Write lock taken up front; every state transition is atomic. |
| `busy_timeout` | 5 s | Maintenance/second-process access waits instead of failing. |
| `foreign_keys` | ON | Referential robustness for future tables. |

`JournalMode::Wal` (with `synchronous = NORMAL`) is available as an explicit
opt-in when write throughput matters and shutdown/checkpoint behavior has been
validated on the target hardware. WAL `NORMAL` is still crash-safe (no
corruption), but the most recent commits may be lost on power loss.

Every mutation is one explicit transaction — the plan's "one transaction per
acquisition/delivery state transition, not per metric".

## SQLite linkage

Two mutually exclusive options, controlled by Cargo features:

- **`bundled-sqlite` (default)** — compiles SQLite from source into the crate.
  Self-contained, reproducible, no version drift, and the most robust choice
  for cross-compiling the ARMv6 Pi Zero W target (`arm-unknown-linux-gnueabihf`)
  where system pkg-config/linker paths are painful. Slightly larger binary.
- **`system-sqlite`** — links the system `libsqlite3` via pkg-config. Smaller
  binary, but the deployed system must provide a recent SQLite (≥ 3.37, needed
  for the `STRICT` tables used in the schema; Raspberry Pi OS Bookworm ships
  3.40+).

Do not enable both. The workspace should pick one for all targets.

## Schema

```
collector_state(key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at_ms INTEGER NOT NULL)
spool_batch(id INTEGER PK AUTOINCREMENT, device TEXT, created_at_ms INTEGER,
            payload BLOB, attempts INTEGER, next_attempt_at_ms INTEGER)  -- + FIFO index
energy_state(device TEXT PRIMARY KEY, total_kwh REAL, last_power_watts REAL,
             last_sample_at_ms INTEGER, updated_at_ms INTEGER)
```

All tables are `STRICT` with `CHECK` constraints. Timestamps are integer
milliseconds since the Unix epoch. Delivered spool rows are deleted, so the
database stays bounded (pruning removes old/over-budget batches).

## Security

This crate never stores PINs, PUKs, BLE bond keys, protected payloads or
unbounded raw capture data. Spool payloads are opaque outbound text with
bounded retention. `#![forbid(unsafe_code)]`.

## Integration notes for `victron-service`

- Wrap `Storage` in `Arc<Storage>`; it is `Send + Sync` (single `Mutex`-guarded
  connection) and safe to move into `spawn_blocking` closures.
- One acquisition cycle: read BLE → `integrate_energy` (commits anchor +
  accumulator) → render metrics → `enqueue_batch` → `claim_oldest_batch` →
  POST → `mark_batch_delivered` / `record_batch_retry`. Enqueue before
  delivery so a crashed cycle leaves the batch durably queued.
- Health metrics: `spool_stats().queued_batches`, `oldest_created_at_ms`,
  plus `spool.delivered_total` / `spool.dropped_total` counters from
  `get_state_i64`; energy skip reasons map to
  `victron_energy_integration_gap_seconds_total` / dropped-sample counters.
- Run `prune_spool` on a maintenance schedule (e.g. once per cycle is cheap).
- Map `StorageError` variants to service error logs; `DatabaseTooNew` means a
  binary/schema mismatch and should be fatal at startup.
