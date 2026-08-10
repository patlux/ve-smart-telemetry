//! SQLite storage/spool adapter.
//!
//! The synchronous `victron-storage` facade is owned by `Arc` and every call
//! runs through `spawn_blocking`, keeping SQLite work off the current-thread
//! Tokio runtime while preserving one serialized connection.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use victron_service::{
    AcquisitionCommit, AcquisitionCommitOutcome, ClaimedBatch, EnergyState, RetryOutcome,
    SpoolHealth, StorageError, StoragePort,
};
use victron_storage::{
    AcquisitionCommit as SqliteCommit, AcquisitionCommitOutcome as SqliteCommitOutcome,
    AcquisitionEnergyState, Storage, StorageConfig,
};

#[derive(Clone)]
pub struct SqliteStorage {
    inner: Arc<Storage>,
    device: String,
}

impl SqliteStorage {
    pub fn open(path: &Path, device: String, config: StorageConfig) -> Result<Self, StorageError> {
        let inner = Storage::open(path, config).map_err(map_error)?;
        Ok(Self {
            inner: Arc::new(inner),
            device,
        })
    }
}

#[async_trait]
impl StoragePort for SqliteStorage {
    async fn last_success(&self) -> Result<Option<SystemTime>, StorageError> {
        let storage = Arc::clone(&self.inner);
        let device = self.device.clone();
        blocking(move || storage.last_acquisition_success(&device))
            .await?
            .map(ms_to_time)
            .transpose()
    }

    async fn energy_state(&self) -> Result<Option<EnergyState>, StorageError> {
        let storage = Arc::clone(&self.inner);
        let device = self.device.clone();
        blocking(move || storage.get_energy(&device))
            .await?
            .map(|state| {
                Ok(EnergyState {
                    total_kwh: state.total_kwh,
                    last_power_watts: state.last_power_watts,
                    last_sample_at: state.last_sample_at_ms.map(ms_to_time).transpose()?,
                })
            })
            .transpose()
    }

    async fn commit_acquisition(
        &mut self,
        commit: AcquisitionCommit,
    ) -> Result<AcquisitionCommitOutcome, StorageError> {
        let sqlite_commit = SqliteCommit {
            device: commit.device.as_str().to_string(),
            observed_at_ms: time_to_ms(commit.observed_at)?,
            expected_energy: commit
                .expected_energy
                .map(service_energy_to_sqlite)
                .transpose()?,
            next_energy: service_energy_to_sqlite(commit.next_energy)?,
            payload: commit.payload,
        };
        let storage = Arc::clone(&self.inner);
        match blocking(move || storage.commit_acquisition(&sqlite_commit)).await? {
            SqliteCommitOutcome::Committed { .. } => Ok(AcquisitionCommitOutcome::Committed),
            SqliteCommitOutcome::AlreadyCommitted => Ok(AcquisitionCommitOutcome::AlreadyCommitted),
        }
    }

    async fn spool_health(&self, now: SystemTime) -> Result<SpoolHealth, StorageError> {
        let now_ms = time_to_ms(now)?;
        let storage = Arc::clone(&self.inner);
        let stats = blocking(move || storage.spool_stats()).await?;
        let depth = usize::try_from(stats.queued_batches)
            .map_err(|_| StorageError::Schema("spool depth exceeds usize".into()))?;
        let oldest_age = stats
            .oldest_created_at_ms
            .map(|oldest| Duration::from_millis(now_ms.saturating_sub(oldest).max(0) as u64));
        Ok(SpoolHealth { depth, oldest_age })
    }

    async fn spool_claim_next(
        &mut self,
        claim_ttl: Duration,
        now: SystemTime,
    ) -> Result<Option<ClaimedBatch>, StorageError> {
        let now_ms = time_to_ms(now)?;
        let claim_ms = duration_to_ms(claim_ttl)?;
        if claim_ms != self.inner.config().spool_inflight_ms {
            return Err(StorageError::Schema(format!(
                "service claim TTL ({claim_ms} ms) differs from SQLite spool TTL ({} ms)",
                self.inner.config().spool_inflight_ms
            )));
        }
        let storage = Arc::clone(&self.inner);
        blocking(move || storage.claim_oldest_batch(now_ms))
            .await?
            .map(|batch| {
                Ok(ClaimedBatch {
                    id: u64::try_from(batch.id).map_err(|_| StorageError::Corrupt)?,
                    payload: batch.payload,
                    attempts: batch.attempts,
                })
            })
            .transpose()
    }

    async fn spool_complete(&mut self, claim: &ClaimedBatch) -> Result<(), StorageError> {
        let id = claim_id(claim.id)?;
        let storage = Arc::clone(&self.inner);
        let changed = blocking(move || storage.mark_batch_delivered(id)).await?;
        require_changed(changed, claim.id, "complete")
    }

    async fn spool_retry(
        &mut self,
        claim: &ClaimedBatch,
        now: SystemTime,
    ) -> Result<RetryOutcome, StorageError> {
        let id = claim_id(claim.id)?;
        let now_ms = time_to_ms(now)?;
        let storage = Arc::clone(&self.inner);
        match blocking(move || storage.record_batch_retry(id, now_ms)).await? {
            victron_storage::RetryOutcome::Retried { attempts, .. } => {
                Ok(RetryOutcome::Retried { attempts })
            }
            victron_storage::RetryOutcome::Dropped { attempts } => {
                Ok(RetryOutcome::Dropped { attempts })
            }
        }
    }

    async fn spool_drop(&mut self, claim: &ClaimedBatch) -> Result<(), StorageError> {
        let id = claim_id(claim.id)?;
        let storage = Arc::clone(&self.inner);
        let changed = blocking(move || storage.drop_batch(id)).await?;
        require_changed(changed, claim.id, "drop")
    }
}

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T, victron_storage::StorageError> + Send + 'static,
) -> Result<T, StorageError> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| StorageError::Io(format!("SQLite worker failed: {error}")))?
        .map_err(map_error)
}

fn service_energy_to_sqlite(state: EnergyState) -> Result<AcquisitionEnergyState, StorageError> {
    Ok(AcquisitionEnergyState {
        total_kwh: state.total_kwh,
        last_power_watts: state.last_power_watts,
        last_sample_at_ms: state.last_sample_at.map(time_to_ms).transpose()?,
    })
}

fn time_to_ms(time: SystemTime) -> Result<i64, StorageError> {
    let duration = time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StorageError::InvalidTimestamp(time))?;
    let millis =
        i64::try_from(duration.as_millis()).map_err(|_| StorageError::InvalidTimestamp(time))?;
    if millis <= 0 {
        return Err(StorageError::InvalidTimestamp(time));
    }
    Ok(millis)
}

fn ms_to_time(ms: i64) -> Result<SystemTime, StorageError> {
    let millis = u64::try_from(ms).map_err(|_| StorageError::Corrupt)?;
    if millis == 0 {
        return Err(StorageError::Corrupt);
    }
    UNIX_EPOCH
        .checked_add(Duration::from_millis(millis))
        .ok_or(StorageError::Corrupt)
}

fn duration_to_ms(duration: Duration) -> Result<i64, StorageError> {
    i64::try_from(duration.as_millis())
        .ok()
        .filter(|millis| *millis > 0)
        .ok_or_else(|| StorageError::Schema("claim TTL must fit in positive milliseconds".into()))
}

fn claim_id(id: u64) -> Result<i64, StorageError> {
    i64::try_from(id).map_err(|_| StorageError::Corrupt)
}

fn require_changed(changed: bool, id: u64, operation: &str) -> Result<(), StorageError> {
    if changed {
        Ok(())
    } else {
        Err(StorageError::Io(format!(
            "spool batch {id} disappeared before {operation}"
        )))
    }
}

fn map_error(error: victron_storage::StorageError) -> StorageError {
    match error {
        victron_storage::StorageError::DatabaseTooNew { found, supported } => StorageError::Schema(
            format!("database schema {found} is newer than supported {supported}"),
        ),
        victron_storage::StorageError::EnergyAnchorConflict => StorageError::EnergyAnchorConflict,
        victron_storage::StorageError::Inconsistent(message) => {
            tracing::error!(error = %message, "SQLite consistency check failed");
            StorageError::Corrupt
        }
        victron_storage::StorageError::Sqlite(error) => StorageError::Io(error.to_string()),
        victron_storage::StorageError::InvalidArgument(message) => StorageError::Schema(message),
    }
}
