//! Durable local persistence for the Victron BLE collector.
//!
//! `victron-storage` is a small, **synchronous** SQLite persistence layer used
//! by `victron-service` through Tokio's `spawn_blocking`. It is
//! deliberately independent of BLE, HTTP and Prometheus: callers pass opaque
//! byte payloads and small storage DTOs defined in this crate and map them to
//! their own domain types.
//!
//! # Responsibilities
//!
//! * Idempotent schema migration via `PRAGMA user_version`.
//! * A durable, ordered outbound batch spool (FIFO replay, lease-or-peek,
//!   bounded retry with next-attempt scheduling, bounded pruning by count/age).
//! * Collector key/value state (`collector_state`).
//! * Transaction-safe per-device energy integration state with a trapezoidal
//!   integration helper and explicit skip reasons.
//!
//! # Durability model
//!
//! The default journaling is conservative for SD-card and power-loss behavior:
//! `DELETE` journaling with `synchronous = FULL`. Every mutation is wrapped in
//! a single explicit transaction, so each state transition commits atomically
//! and a crash leaves either the complete before-state or the complete
//! after-state. [`JournalMode::Wal`] is available as an explicit opt-in for
//! deployments that have validated shutdown/checkpoint behavior.
//!
//! Energy integration never double-counts across restarts: the accumulator and
//! the last-sample anchor commit in the same transaction, and a sample whose
//! timestamp is not strictly newer than the stored anchor is skipped.
//!
//! # Security
//!
//! This crate never stores PINs, PUKs, BLE bond keys, protected payloads or
//! unbounded raw capture data. Spool payloads are opaque outbound text and are
//! bounded by pruning.
//!
//! # Example
//!
//! ```no_run
//! use std::path::Path;
//! use victron_storage::{EnergySample, Storage, StorageConfig};
//!
//! let storage = Storage::open(Path::new("/var/lib/victron-collector/state.sqlite3"),
//!                             StorageConfig::default())?;
//!
//! let now_ms = 1_700_000_000_000i64;
//! // Durable outbound batch
//! let id = storage.enqueue_batch("solar-charger", b"victron_pv_power_watts 123 1700000000000".to_vec(), now_ms)?;
//! // ... service POSTs the payload to VictoriaMetrics via spawn_blocking ...
//! storage.mark_batch_delivered(id)?;
//!
//! // Fallback energy integration
//! let outcome = storage.integrate_energy(&EnergySample {
//!     device: "solar-charger".into(),
//!     power_watts: 123.5,
//!     sample_at_ms: now_ms,
//! })?;
//! # Ok::<(), victron_storage::StorageError>(())
//! ```

#![forbid(unsafe_code)]

mod acquisition;
mod database;
mod energy;
mod spool;

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;

pub use acquisition::{AcquisitionCommit, AcquisitionCommitOutcome, AcquisitionEnergyState};
pub use database::KvEntry;
pub use energy::{EnergyOutcome, EnergySample, EnergyState, SkipReason};
pub use spool::{PruneStats, RetryOutcome, SpoolBatch, SpoolStats};

/// SQLite journaling and synchronous-mode pairing.
///
/// Every option here is a deliberate, documented tradeoff; the crate defaults
/// to the conservative variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalMode {
    /// `journal_mode = DELETE`, `synchronous = FULL`.
    ///
    /// Maximum power-loss safety at the cost of more fsyncs per commit. This
    /// is the default: it is the most conservative option for SD-card-backed
    /// storage and needs no shutdown checkpointing.
    Conservative,
    /// `journal_mode = WAL`, `synchronous = NORMAL`.
    ///
    /// Better write throughput and readers never block writers. On power loss,
    /// WAL recovery is crash-safe but the most recent commits may be lost
    /// (no corruption). Leaves `-wal`/`-shm` sidecar files that must be
    /// checkpointed on clean shutdown; only use after validating shutdown
    /// behavior on the target hardware.
    Wal,
}

/// Read-back of the `PRAGMA synchronous` level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynchronousMode {
    Off,
    Normal,
    Full,
    Extra,
}

impl From<i64> for SynchronousMode {
    fn from(v: i64) -> Self {
        match v {
            0 => SynchronousMode::Off,
            1 => SynchronousMode::Normal,
            3 => SynchronousMode::Extra,
            _ => SynchronousMode::Full,
        }
    }
}

/// Configuration for [`Storage`].
///
/// The defaults target a single-device Raspberry Pi Zero W collector:
/// conservative journaling, a spool of at most 10,000 batches or 7 days, and a
/// 5-minute energy integration gap threshold (see the Grafana integration
/// plan). Override before calling [`Storage::open`].
#[derive(Debug, Clone)]
pub struct StorageConfig {
    pub journal: JournalMode,
    /// Milliseconds a statement waits for a locked database before failing.
    pub busy_timeout_ms: u32,
    /// Maximum delivery attempts per batch before it is dropped.
    pub max_spool_attempts: u32,
    /// Delay added to a claimed batch while it is in flight. If the process
    /// dies mid-delivery, the batch becomes claimable again after this window.
    pub spool_inflight_ms: i64,
    /// Base exponential backoff for a failed delivery (doubles per attempt).
    pub spool_retry_base_ms: i64,
    /// Upper bound for the retry backoff.
    pub spool_retry_max_ms: i64,
    /// Keep at most this many undelivered batches (pruning bound).
    pub max_spool_batches: u64,
    /// Drop undelivered batches older than this (pruning bound).
    pub max_spool_age_ms: i64,
    /// Energy integration: maximum allowed gap between consecutive samples.
    /// Larger gaps are skipped and reset the integration anchor.
    pub energy_gap_threshold_ms: i64,
    /// Energy integration: powers below this are treated as invalid samples
    /// (confirmed PV power is never negative).
    pub energy_min_power_watts: f64,
    /// Energy integration: powers above this are treated as invalid samples.
    /// The default (30 kW) matches the domain plan's conservative PV maximum
    /// of `[0.0, 30_000.0]` W (250 V × 100 A = 25 kW worst case plus margin),
    /// so legitimate supported-family readings are never rejected.
    pub energy_max_power_watts: Option<f64>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        StorageConfig::conservative()
    }
}

impl StorageConfig {
    /// Conservative SD-card/power-loss defaults.
    pub fn conservative() -> Self {
        StorageConfig {
            journal: JournalMode::Conservative,
            busy_timeout_ms: 5_000,
            max_spool_attempts: 5,
            spool_inflight_ms: 60_000,
            spool_retry_base_ms: 30_000,
            spool_retry_max_ms: 1_800_000, // 30 min
            max_spool_batches: 10_000,
            max_spool_age_ms: 604_800_000,    // 7 days
            energy_gap_threshold_ms: 300_000, // 5 min
            energy_min_power_watts: 0.0,
            energy_max_power_watts: Some(30_000.0),
        }
    }

    /// Same defaults but with WAL journaling (`synchronous = NORMAL`).
    pub fn wal() -> Self {
        StorageConfig {
            journal: JournalMode::Wal,
            ..StorageConfig::conservative()
        }
    }

    /// Validates invariants this crate relies on. Called by [`Storage::open`];
    /// exposed so binaries can fail fast on configuration before opening.
    pub fn validate(&self) -> Result<(), StorageError> {
        if self.max_spool_attempts == 0 {
            return Err(StorageError::InvalidArgument(
                "max_spool_attempts must be >= 1".into(),
            ));
        }
        // Delivery timing: every delay must be strictly positive and the
        // retry ceiling must not sit below the base backoff (the spool's
        // exponential backoff clamps to the ceiling).
        if self.spool_inflight_ms <= 0
            || self.spool_retry_base_ms <= 0
            || self.spool_retry_max_ms <= 0
        {
            return Err(StorageError::InvalidArgument(
                "spool retry delays must be > 0".into(),
            ));
        }
        if self.spool_retry_max_ms < self.spool_retry_base_ms {
            return Err(StorageError::InvalidArgument(
                "spool_retry_max_ms must be >= spool_retry_base_ms".into(),
            ));
        }
        // Pruning bounds. A zero count bound would empty the spool on the
        // first prune, and `max_spool_batches` is cast to a SQLite INTEGER
        // (i64) in the count-bound DELETE, so values above i64::MAX would
        // overflow that cast.
        if self.max_spool_batches == 0 {
            return Err(StorageError::InvalidArgument(
                "max_spool_batches must be >= 1".into(),
            ));
        }
        if self.max_spool_batches > i64::MAX as u64 {
            return Err(StorageError::InvalidArgument(
                "max_spool_batches must fit in a SQLite INTEGER".into(),
            ));
        }
        // A non-positive age bound would prune every batch older than "now".
        if self.max_spool_age_ms <= 0 {
            return Err(StorageError::InvalidArgument(
                "max_spool_age_ms must be > 0".into(),
            ));
        }
        if self.energy_gap_threshold_ms <= 0 {
            return Err(StorageError::InvalidArgument(
                "energy_gap_threshold_ms must be > 0".into(),
            ));
        }
        // Energy power bounds: the integration check compares samples directly
        // against these, so they must be finite and form a usable interval
        // (min < max).
        if !self.energy_min_power_watts.is_finite()
            || self
                .energy_max_power_watts
                .is_some_and(|m| !m.is_finite() || m <= self.energy_min_power_watts)
        {
            return Err(StorageError::InvalidArgument(
                "energy power bounds must be finite and min < max".into(),
            ));
        }
        Ok(())
    }
}

/// Errors returned by [`Storage`].
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database schema version {found} is newer than this binary supports ({supported}); refusing to modify it")]
    DatabaseTooNew { found: i64, supported: i64 },
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    #[error("inconsistent database state: {0}")]
    Inconsistent(String),
    #[error("energy anchor changed between read and acquisition commit")]
    EnergyAnchorConflict,
}

/// The persistence facade. Cheap to construct, `Send + Sync`, and safe to
/// share across `spawn_blocking` closures via `Arc<Storage>`.
///
/// All access is serialized on a single SQLite connection held behind a
/// `Mutex`; state transitions are wrapped in `BEGIN IMMEDIATE` transactions so
/// they stay atomic even if a second process ever opens the same file.
pub struct Storage {
    conn: Mutex<Connection>,
    cfg: StorageConfig,
}

impl Storage {
    /// Opens (creating if needed) the database at `path`, applies migrations
    /// idempotently, and applies the configured journaling/synchronous
    /// pragmas. Fails with [`StorageError::DatabaseTooNew`] if the file was
    /// written by a newer schema version.
    pub fn open(path: &Path, cfg: StorageConfig) -> Result<Storage, StorageError> {
        cfg.validate()?;
        let conn = database::open_connection(path, &cfg)?;
        Ok(Storage {
            conn: Mutex::new(conn),
            cfg,
        })
    }

    /// Immutable view of the configuration in effect.
    pub fn config(&self) -> &StorageConfig {
        &self.cfg
    }

    /// Current `journal_mode` of the open connection (`delete`, `wal`, ...).
    pub fn journal_mode(&self) -> Result<String, StorageError> {
        let conn = self.lock();
        Ok(conn.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))?)
    }

    /// Current `synchronous` level of the open connection.
    pub fn synchronous(&self) -> Result<SynchronousMode, StorageError> {
        let conn = self.lock();
        let v: i64 = conn.pragma_query_value(None, "synchronous", |row| row.get(0))?;
        Ok(SynchronousMode::from(v))
    }

    /// Current `user_version` (applied schema version).
    pub fn user_version(&self) -> Result<i64, StorageError> {
        let conn = self.lock();
        Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    /// Last successfully committed acquisition timestamp for `device`.
    pub fn last_acquisition_success(&self, device: &str) -> Result<Option<i64>, StorageError> {
        let conn = self.lock();
        acquisition::last_success(&conn, device)
    }

    /// Atomically persists energy, acquisition identity, and one spool batch.
    /// Replaying the same or an older observation is an idempotent no-op.
    pub fn commit_acquisition(
        &self,
        commit: &AcquisitionCommit,
    ) -> Result<AcquisitionCommitOutcome, StorageError> {
        let mut conn = self.lock();
        acquisition::commit(&mut conn, commit)
    }

    /// Enqueues one outbound batch for delivery. Returns its spool id.
    pub fn enqueue_batch(
        &self,
        device: &str,
        payload: Vec<u8>,
        now_ms: i64,
    ) -> Result<i64, StorageError> {
        let conn = self.lock();
        spool::enqueue(&conn, device, payload, now_ms)
    }

    /// Non-mutating look at the oldest batch that is ready for delivery.
    pub fn peek_oldest_batch(&self, now_ms: i64) -> Result<Option<SpoolBatch>, StorageError> {
        let conn = self.lock();
        spool::peek_oldest(&conn, now_ms, self.cfg.max_spool_attempts)
    }

    /// Atomically leases the oldest ready batch: increments its attempt count
    /// and schedules it `spool_inflight_ms` in the future so a crash during
    /// delivery cannot lose it. The caller must follow up with
    /// [`Storage::mark_batch_delivered`] or [`Storage::record_batch_retry`].
    pub fn claim_oldest_batch(&self, now_ms: i64) -> Result<Option<SpoolBatch>, StorageError> {
        let mut conn = self.lock();
        spool::claim_oldest(&mut conn, now_ms, &self.cfg)
    }

    /// Marks a batch as delivered, removing it from the spool and bumping the
    /// `spool.delivered_total` counter. Returns `false` if the id was unknown
    /// (already delivered/pruned).
    pub fn mark_batch_delivered(&self, id: i64) -> Result<bool, StorageError> {
        let mut conn = self.lock();
        let now_ms = database::now_ms();
        spool::mark_delivered(&mut conn, id, now_ms)
    }

    /// Drops a batch without counting it as delivered. Returns `false` when
    /// the id was already delivered, dropped, or pruned.
    pub fn drop_batch(&self, id: i64) -> Result<bool, StorageError> {
        let mut conn = self.lock();
        let now_ms = database::now_ms();
        spool::drop_batch(&mut conn, id, now_ms)
    }

    /// Records a failed delivery: schedules the next attempt with exponential
    /// backoff, or drops the batch once `max_spool_attempts` is reached
    /// (bumping `spool.dropped_total`).
    pub fn record_batch_retry(&self, id: i64, now_ms: i64) -> Result<RetryOutcome, StorageError> {
        let mut conn = self.lock();
        spool::record_retry(&mut conn, id, now_ms, &self.cfg)
    }

    /// Enforces the configured spool bounds: age, attempt budget and count.
    /// Returns how many batches were removed and how many remain.
    pub fn prune_spool(&self, now_ms: i64) -> Result<PruneStats, StorageError> {
        let mut conn = self.lock();
        spool::prune(&mut conn, now_ms, &self.cfg)
    }

    /// Current spool depth, oldest queued batch and total attempt count
    /// (useful for the `victron_spool_batches` health metric).
    pub fn spool_stats(&self) -> Result<SpoolStats, StorageError> {
        let conn = self.lock();
        spool::stats(&conn)
    }

    /// Reads one collector state entry.
    pub fn get_state(&self, key: &str) -> Result<Option<String>, StorageError> {
        let conn = self.lock();
        database::get_kv(&conn, key)
    }

    /// Writes (upserts) one collector state entry with a millisecond timestamp.
    pub fn set_state(&self, key: &str, value: &str, now_ms: i64) -> Result<(), StorageError> {
        let conn = self.lock();
        database::set_kv(&conn, key, value, now_ms)
    }

    /// Convenience accessor for numeric counters stored in collector state.
    pub fn get_state_i64(&self, key: &str) -> Result<Option<i64>, StorageError> {
        let conn = self.lock();
        database::get_kv_i64(&conn, key)
    }

    /// Convenience writer for numeric counters stored in collector state.
    pub fn set_state_i64(&self, key: &str, value: i64, now_ms: i64) -> Result<(), StorageError> {
        let conn = self.lock();
        database::set_kv(&conn, key, &value.to_string(), now_ms)
    }

    /// Lists all collector state entries.
    pub fn get_state_entries(&self) -> Result<Vec<KvEntry>, StorageError> {
        let conn = self.lock();
        database::list_kv(&conn)
    }

    /// Current energy integration state for one device, if any exists yet.
    pub fn get_energy(&self, device: &str) -> Result<Option<EnergyState>, StorageError> {
        let conn = self.lock();
        energy::get(&conn, device)
    }

    /// Applies one trapezoidal integration step for `sample` in a single
    /// transaction. See [`EnergyOutcome`] for the explicit skip reasons.
    pub fn integrate_energy(&self, sample: &EnergySample) -> Result<EnergyOutcome, StorageError> {
        let mut conn = self.lock();
        energy::integrate(&mut conn, &self.cfg, sample)
    }

    /// Overwrites the stored cumulative energy for `device` and clears the
    /// integration anchor (diagnostics/reconciliation only).
    pub fn reset_energy(
        &self,
        device: &str,
        total_kwh: f64,
        now_ms: i64,
    ) -> Result<(), StorageError> {
        let mut conn = self.lock();
        energy::reset(&mut conn, device, total_kwh, now_ms)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        // Recover from a poisoned lock instead of propagating a panic: our
        // code paths do not panic, and a poisoned lock would otherwise wedge
        // the collector.
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
