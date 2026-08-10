//! Atomic persistence of one fully rendered acquisition.
//!
//! Energy state, per-device acquisition identity, and the outbound spool batch
//! commit in one `BEGIN IMMEDIATE` transaction. The observation timestamp is
//! the idempotency key: replaying the same or an older acquisition is a no-op.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::{spool, StorageError};

const MAX_ENERGY_KWH: f64 = 1.0e9;

/// Optimistic energy anchor used by [`AcquisitionCommit`].
#[derive(Debug, Clone, PartialEq)]
pub struct AcquisitionEnergyState {
    pub total_kwh: f64,
    pub last_power_watts: Option<f64>,
    pub last_sample_at_ms: Option<i64>,
}

/// All durable writes produced by one acquisition.
#[derive(Debug, Clone, PartialEq)]
pub struct AcquisitionCommit {
    pub device: String,
    /// Positive Unix timestamp in milliseconds; also the idempotency key.
    pub observed_at_ms: i64,
    /// Energy state read before computing `next_energy`.
    pub expected_energy: Option<AcquisitionEnergyState>,
    pub next_energy: AcquisitionEnergyState,
    /// Opaque rendered Prometheus batch.
    pub payload: Vec<u8>,
}

/// Result of [`crate::Storage::commit_acquisition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquisitionCommitOutcome {
    Committed { batch_id: i64 },
    AlreadyCommitted,
}

pub(crate) fn last_success(conn: &Connection, device: &str) -> Result<Option<i64>, StorageError> {
    validate_device(device)?;
    let key = last_success_key(device);
    crate::database::get_kv_i64(conn, &key)
}

pub(crate) fn commit(
    conn: &mut Connection,
    commit: &AcquisitionCommit,
) -> Result<AcquisitionCommitOutcome, StorageError> {
    validate_identity(commit)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let key = last_success_key(&commit.device);

    if crate::database::get_kv_i64(&tx, &key)?.is_some_and(|stored| stored >= commit.observed_at_ms)
    {
        tx.commit()?;
        return Ok(AcquisitionCommitOutcome::AlreadyCommitted);
    }

    validate_state_and_payload(commit)?;
    let current = read_energy(&tx, &commit.device)?;
    if !same_anchor(current.as_ref(), commit.expected_energy.as_ref()) {
        return Err(StorageError::EnergyAnchorConflict);
    }

    let next = &commit.next_energy;
    tx.execute(
        "INSERT INTO energy_state
             (device, total_kwh, last_power_watts, last_sample_at_ms, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(device) DO UPDATE SET
             total_kwh = excluded.total_kwh,
             last_power_watts = excluded.last_power_watts,
             last_sample_at_ms = excluded.last_sample_at_ms,
             updated_at_ms = excluded.updated_at_ms",
        params![
            commit.device,
            next.total_kwh,
            next.last_power_watts,
            next.last_sample_at_ms,
            commit.observed_at_ms
        ],
    )?;
    crate::database::set_kv(
        &tx,
        &key,
        &commit.observed_at_ms.to_string(),
        commit.observed_at_ms,
    )?;
    let batch_id = spool::enqueue(
        &tx,
        &commit.device,
        commit.payload.clone(),
        commit.observed_at_ms,
    )?;
    tx.commit()?;
    Ok(AcquisitionCommitOutcome::Committed { batch_id })
}

fn read_energy(
    conn: &Connection,
    device: &str,
) -> Result<Option<AcquisitionEnergyState>, StorageError> {
    Ok(conn
        .query_row(
            "SELECT total_kwh, last_power_watts, last_sample_at_ms
             FROM energy_state WHERE device = ?1",
            params![device],
            |row| {
                Ok(AcquisitionEnergyState {
                    total_kwh: row.get(0)?,
                    last_power_watts: row.get(1)?,
                    last_sample_at_ms: row.get(2)?,
                })
            },
        )
        .optional()?)
}

fn same_anchor(
    stored: Option<&AcquisitionEnergyState>,
    expected: Option<&AcquisitionEnergyState>,
) -> bool {
    match (stored, expected) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.total_kwh.to_bits() == b.total_kwh.to_bits()
                && optional_f64_eq(a.last_power_watts, b.last_power_watts)
                && a.last_sample_at_ms == b.last_sample_at_ms
        }
        _ => false,
    }
}

fn optional_f64_eq(left: Option<f64>, right: Option<f64>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(a), Some(b)) => a.to_bits() == b.to_bits(),
        _ => false,
    }
}

fn validate_identity(commit: &AcquisitionCommit) -> Result<(), StorageError> {
    validate_device(&commit.device)?;
    if commit.observed_at_ms <= 0 {
        return Err(StorageError::InvalidArgument(
            "observed_at_ms must be > 0".into(),
        ));
    }
    Ok(())
}

fn validate_state_and_payload(commit: &AcquisitionCommit) -> Result<(), StorageError> {
    if commit.payload.is_empty() {
        return Err(StorageError::InvalidArgument(
            "payload must not be empty".into(),
        ));
    }
    if let Some(expected) = &commit.expected_energy {
        validate_energy(expected, commit.observed_at_ms, "expected_energy")?;
    }
    validate_energy(&commit.next_energy, commit.observed_at_ms, "next_energy")
}

fn validate_energy(
    state: &AcquisitionEnergyState,
    observed_at_ms: i64,
    field: &str,
) -> Result<(), StorageError> {
    if !state.total_kwh.is_finite() || !(0.0..=MAX_ENERGY_KWH).contains(&state.total_kwh) {
        return Err(StorageError::InvalidArgument(format!(
            "{field}.total_kwh must be finite and between 0 and {MAX_ENERGY_KWH}"
        )));
    }
    if state
        .last_power_watts
        .is_some_and(|power| !power.is_finite())
    {
        return Err(StorageError::InvalidArgument(format!(
            "{field}.last_power_watts must be finite"
        )));
    }
    if state
        .last_sample_at_ms
        .is_some_and(|timestamp| timestamp <= 0 || timestamp > observed_at_ms)
    {
        return Err(StorageError::InvalidArgument(format!(
            "{field}.last_sample_at_ms must be positive and no later than observed_at_ms"
        )));
    }
    Ok(())
}

fn validate_device(device: &str) -> Result<(), StorageError> {
    if device.is_empty() {
        return Err(StorageError::InvalidArgument(
            "device must not be empty".into(),
        ));
    }
    Ok(())
}

fn last_success_key(device: &str) -> String {
    format!("acquisition.last_success.{device}")
}
