//! Durable state and delivery spool port.
//!
//! The concrete implementation lives in `victron-storage` (SQLite). The
//! service coordinates *ownership* of batches through this trait; it never
//! renders or interprets payloads.
//!
//! # Atomic acquisition persistence
//!
//! One acquisition is committed with a single [`StoragePort::commit_acquisition`]
//! call: the next energy state, the acquisition identity (`observed_at`) and
//! the rendered batch are persisted in **one transaction**. The old
//! three-call sequence (`save_energy` + `record_success` + `spool_enqueue`)
//! is gone because it could leave energy advanced without a queued batch.
//!
//! The parent storage adapter requires one new SQLite transaction/API
//! addition (`commit_acquisition`); the storage lane implements it. Reads
//! needed to prepare the commit ([`StoragePort::energy_state`],
//! [`StoragePort::last_success`]) stay separate from the commit itself.
//!
//! # Idempotent / duplicate semantics
//!
//! `commit_acquisition` is idempotent per `(device, observed_at)`: when the
//! stored acquisition identity is already at or after `observed_at`, the
//! commit is a no-op returning [`AcquisitionCommitOutcome::AlreadyCommitted`]
//! — nothing is double-counted and no second batch is enqueued. The commit
//! also verifies the optimistic energy anchor: if the stored energy state no
//! longer matches `expected_energy`, it fails with
//! [`StorageError::EnergyAnchorConflict`] instead of overwriting a
//! concurrent modification.
//!
//! # Delivery ownership contract
//!
//! - `spool_claim_next` returns the oldest batch that is both *due*
//!   (`next_attempt_at <= now`) and either unclaimed or with an expired claim
//!   (claimed more than `claim_ttl` ago). A batch with an unexpired claim
//!   MUST NOT be returned again: exactly one owner at a time. Claiming
//!   increments the stored attempt counter, so a freshly enqueued batch is
//!   claimed as **attempt 1** and `ClaimedBatch.attempts` is the 1-based
//!   attempt of the current delivery.
//! - `spool_complete` removes a claimed batch after the network call
//!   succeeded and bumps the delivered counter (at-most-once under crash +
//!   TTL recovery).
//! - `spool_retry` releases the claim and schedules the next attempt; the
//!   adapter computes the next deadline from its own bounded backoff. It
//!   returns [`RetryOutcome`] so the service can distinguish a re-queued
//!   batch from an adapter-side budget drop.
//! - `spool_drop` removes a claimed batch after a permanent rejection or an
//!   exhausted retry budget and bumps the dropped counter. It NEVER bumps the
//!   delivered counter: a drop is not a delivery.

use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use victron_domain::DeviceId;

/// A batch claimed for delivery. `attempts` is the 1-based attempt number of
/// the current delivery (claiming increments the stored counter, so a freshly
/// enqueued batch is claimed as attempt 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedBatch {
    pub id: u64,
    pub payload: Vec<u8>,
    pub attempts: u32,
}

/// Durable local energy integration state.
#[derive(Debug, Clone, PartialEq)]
pub struct EnergyState {
    /// Accumulated kWh (fallback integration only; native yield lives in the
    /// device and is preferred whenever reported confirmed).
    pub total_kwh: f64,
    /// Last valid confirmed PV power used for trapezoidal integration.
    pub last_power_watts: Option<f64>,
    /// Observation time of the last persisted sample.
    pub last_sample_at: Option<SystemTime>,
}

/// One atomic acquisition commit.
#[derive(Debug, Clone, PartialEq)]
pub struct AcquisitionCommit {
    /// Canonical device identity (the storage key).
    pub device: DeviceId,
    /// Observation time of the sample; the persisted acquisition identity
    /// and the idempotency key. Must be positive (after the Unix epoch).
    pub observed_at: SystemTime,
    /// Energy state read before computing `next_energy` (optimistic anchor).
    pub expected_energy: Option<EnergyState>,
    /// Energy state to persist (may equal `expected_energy` when skipped).
    pub next_energy: EnergyState,
    /// Rendered batch to enqueue (opaque bytes to the storage layer).
    pub payload: Vec<u8>,
}

/// Result of one acquisition commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionCommitOutcome {
    /// The transaction committed: energy advanced, identity recorded, batch
    /// enqueued.
    Committed,
    /// The same `observed_at` was already committed: idempotent no-op,
    /// nothing double-counted, no second batch.
    AlreadyCommitted,
}

/// Spool health snapshot for rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpoolHealth {
    /// Number of queued (unclaimed or retry-pending) batches.
    pub depth: usize,
    /// Age of the oldest queued batch (`None` when the spool is empty).
    pub oldest_age: Option<Duration>,
}

/// Result of scheduling a retry, mirroring `victron-storage::RetryOutcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryOutcome {
    /// The batch stays queued; the adapter scheduled the next attempt.
    Retried { attempts: u32 },
    /// The adapter's attempt budget is exhausted; the batch was dropped.
    Dropped { attempts: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StorageError {
    #[error("storage open failed: {0}")]
    Open(String),
    #[error("storage io failed: {0}")]
    Io(String),
    #[error("schema error: {0}")]
    Schema(String),
    #[error("storage is corrupt")]
    Corrupt,
    #[error("storage lock poisoned")]
    Poisoned,
    #[error("timestamp must be positive (after the Unix epoch); got {0:?}")]
    InvalidTimestamp(SystemTime),
    #[error("energy anchor changed between read and commit (concurrent modification)")]
    EnergyAnchorConflict,
    #[error("not wired: {0}")]
    NotWired(&'static str),
}

impl StorageError {
    /// Stable, SQLite-message-free classification for structured logs.
    pub fn kind(&self) -> &'static str {
        match self {
            StorageError::Open(_) => "open",
            StorageError::Io(_) => "io",
            StorageError::Schema(_) => "schema",
            StorageError::Corrupt => "corrupt",
            StorageError::Poisoned => "poisoned",
            StorageError::InvalidTimestamp(_) => "invalid_timestamp",
            StorageError::EnergyAnchorConflict => "energy_anchor_conflict",
            StorageError::NotWired(_) => "not_wired",
        }
    }
}

/// Durable state + bounded delivery spool.
#[async_trait]
pub trait StoragePort: Send {
    /// Last successfully committed acquisition timestamp (the acquisition
    /// identity). `None` before the first commit.
    async fn last_success(&self) -> Result<Option<SystemTime>, StorageError>;

    /// Current durable energy integration state, if any (pre-commit read).
    async fn energy_state(&self) -> Result<Option<EnergyState>, StorageError>;

    /// Atomically commit one acquisition (see module docs for the idempotent
    /// and all-or-nothing semantics).
    async fn commit_acquisition(
        &mut self,
        commit: AcquisitionCommit,
    ) -> Result<AcquisitionCommitOutcome, StorageError>;

    /// Spool health (depth + oldest age) for rendering.
    async fn spool_health(&self, now: SystemTime) -> Result<SpoolHealth, StorageError>;

    /// Claim the next due batch (see ownership contract).
    async fn spool_claim_next(
        &mut self,
        claim_ttl: Duration,
        now: SystemTime,
    ) -> Result<Option<ClaimedBatch>, StorageError>;

    /// Remove a claimed batch after successful delivery (bumps the delivered
    /// counter).
    async fn spool_complete(&mut self, claim: &ClaimedBatch) -> Result<(), StorageError>;

    /// Release a claim and schedule the next attempt; the adapter computes
    /// the next deadline from its own bounded backoff. Drops the batch when
    /// the adapter's attempt budget is exhausted (bumps the dropped counter).
    async fn spool_retry(
        &mut self,
        claim: &ClaimedBatch,
        now: SystemTime,
    ) -> Result<RetryOutcome, StorageError>;

    /// Remove a claimed batch after a permanent rejection or an exhausted
    /// retry budget. Never bumps the delivered counter.
    async fn spool_drop(&mut self, claim: &ClaimedBatch) -> Result<(), StorageError>;
}
