//! Transaction-safe per-device energy integration state.
//!
//! The accumulator (`total_kwh`) and the last-sample anchor
//! (`last_power_watts` + `last_sample_at_ms`) commit in a single transaction.
//! A sample is only integrated when its timestamp is strictly newer than the
//! stored anchor, so re-processing the same sample after a crash is always a
//! no-op — the fallback energy counter can never double-count across restarts.
//!
//! Trapezoidal formula (matches the collector plan):
//!
//! ```text
//! energy_kwh += ((previous_watts + current_watts) / 2) * elapsed_seconds / 3_600_000
//! ```

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::{StorageConfig, StorageError};

/// One candidate sample for local energy integration.
#[derive(Debug, Clone, PartialEq)]
pub struct EnergySample {
    pub device: String,
    pub power_watts: f64,
    /// Unix timestamp in milliseconds.
    pub sample_at_ms: i64,
}

/// Persisted per-device energy integration state.
#[derive(Debug, Clone, PartialEq)]
pub struct EnergyState {
    pub device: String,
    /// Cumulative integrated energy in kWh. Never negative: the accumulator
    /// only ever adds non-negative deltas and resets reject negative totals.
    pub total_kwh: f64,
    /// Last accepted power sample (the integration anchor).
    pub last_power_watts: Option<f64>,
    /// Timestamp of the last accepted sample, in milliseconds.
    pub last_sample_at_ms: Option<i64>,
    /// Wall-clock time of the last state write, in milliseconds.
    pub updated_at_ms: i64,
}

/// Outcome of one integration step.
#[derive(Debug, Clone, PartialEq)]
pub enum EnergyOutcome {
    /// The sample was integrated; `total_kwh` is the new cumulative value.
    Integrated {
        delta_kwh: f64,
        total_kwh: f64,
        elapsed_ms: i64,
    },
    /// The sample was skipped without adding energy. The reason is explicit so
    /// callers can expose skipped duration instead of silently manufacturing
    /// energy through an outage.
    Skipped { reason: SkipReason },
}

/// Why a sample was not integrated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// No durable previous sample for this device; the sample is stored as the
    /// new anchor but no energy is added.
    FirstSample,
    /// Power is not finite or outside the configured bounds. No anchor is
    /// created or moved, so a first invalid sample leaves no state at all.
    InvalidPower,
    /// The sample timestamp is not strictly newer than the stored anchor
    /// (backward clock or duplicate sample).
    BackwardTime,
    /// The gap to the previous sample exceeds the configured threshold; the
    /// anchor is reset to this sample but no energy is added.
    GapTooLarge { gap_ms: i64, threshold_ms: i64 },
}

pub(crate) fn integrate(
    conn: &mut Connection,
    cfg: &StorageConfig,
    sample: &EnergySample,
) -> Result<EnergyOutcome, StorageError> {
    if sample.device.is_empty() {
        return Err(StorageError::InvalidArgument(
            "device must not be empty".into(),
        ));
    }
    if sample.sample_at_ms <= 0 {
        return Err(StorageError::InvalidArgument(
            "sample_at_ms must be > 0".into(),
        ));
    }
    // Validate power before any read or write. An invalid sample must never
    // become an anchor, so the very first sample of a device must not create
    // a row either: a poisoned anchor would make every later integration step
    // measure against garbage. This check therefore precedes both the
    // first-anchor path and the existing-state path.
    if !sample.power_watts.is_finite()
        || sample.power_watts < cfg.energy_min_power_watts
        || cfg
            .energy_max_power_watts
            .is_some_and(|max| sample.power_watts > max)
    {
        return Ok(EnergyOutcome::Skipped {
            reason: SkipReason::InvalidPower,
        });
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let previous: Option<(f64, Option<f64>, Option<i64>)> = tx
        .query_row(
            "SELECT total_kwh, last_power_watts, last_sample_at_ms
             FROM energy_state WHERE device = ?1",
            params![sample.device],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;

    let outcome = match previous {
        // No durable anchor yet: store this sample as the anchor, add nothing.
        None => {
            tx.execute(
                "INSERT INTO energy_state (device, total_kwh, last_power_watts, last_sample_at_ms, updated_at_ms)
                 VALUES (?1, 0.0, ?2, ?3, ?3)",
                params![sample.device, sample.power_watts, sample.sample_at_ms],
            )?;
            EnergyOutcome::Skipped {
                reason: SkipReason::FirstSample,
            }
        }
        Some((total_kwh, last_power_watts, last_sample_at_ms)) => {
            let last_sample_at_ms = match last_sample_at_ms {
                // Inconsistent pre-existing row without an anchor: repair by
                // treating this sample as a fresh anchor.
                None => {
                    tx.execute(
                        "UPDATE energy_state
                         SET last_power_watts = ?2, last_sample_at_ms = ?3, updated_at_ms = ?3
                         WHERE device = ?1",
                        params![sample.device, sample.power_watts, sample.sample_at_ms],
                    )?;
                    return finalize(
                        tx,
                        EnergyOutcome::Skipped {
                            reason: SkipReason::FirstSample,
                        },
                    );
                }
                Some(ts) => ts,
            };

            if sample.sample_at_ms <= last_sample_at_ms {
                // Never integrate on non-monotonic time; also the restart
                // double-count guard (re-processing an already committed
                // sample lands here).
                EnergyOutcome::Skipped {
                    reason: SkipReason::BackwardTime,
                }
            } else {
                let gap_ms = sample.sample_at_ms - last_sample_at_ms;
                if gap_ms > cfg.energy_gap_threshold_ms {
                    // Reset the anchor without adding energy: the outage is
                    // reported, not silently bridged.
                    tx.execute(
                        "UPDATE energy_state
                         SET last_power_watts = ?2, last_sample_at_ms = ?3, updated_at_ms = ?3
                         WHERE device = ?1",
                        params![sample.device, sample.power_watts, sample.sample_at_ms],
                    )?;
                    EnergyOutcome::Skipped {
                        reason: SkipReason::GapTooLarge {
                            gap_ms,
                            threshold_ms: cfg.energy_gap_threshold_ms,
                        },
                    }
                } else {
                    let previous_power = last_power_watts.unwrap_or(sample.power_watts);
                    let average_watts = (previous_power + sample.power_watts) / 2.0;
                    let elapsed_seconds = gap_ms as f64 / 1000.0;
                    let delta_kwh = average_watts * elapsed_seconds / 3_600_000.0;
                    let new_total = total_kwh + delta_kwh;
                    tx.execute(
                        "UPDATE energy_state
                         SET total_kwh = ?2, last_power_watts = ?3, last_sample_at_ms = ?4, updated_at_ms = ?4
                         WHERE device = ?1",
                        params![
                            sample.device,
                            new_total,
                            sample.power_watts,
                            sample.sample_at_ms
                        ],
                    )?;
                    EnergyOutcome::Integrated {
                        delta_kwh,
                        total_kwh: new_total,
                        elapsed_ms: gap_ms,
                    }
                }
            }
        }
    };

    finalize(tx, outcome)
}

fn finalize(
    tx: rusqlite::Transaction<'_>,
    outcome: EnergyOutcome,
) -> Result<EnergyOutcome, StorageError> {
    tx.commit()?;
    Ok(outcome)
}

pub(crate) fn get(conn: &Connection, device: &str) -> Result<Option<EnergyState>, StorageError> {
    let row: Option<(f64, Option<f64>, Option<i64>, i64)> = conn
        .query_row(
            "SELECT total_kwh, last_power_watts, last_sample_at_ms, updated_at_ms
             FROM energy_state WHERE device = ?1",
            params![device],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    Ok(row.map(
        |(total_kwh, last_power_watts, last_sample_at_ms, updated_at_ms)| EnergyState {
            device: device.to_string(),
            total_kwh,
            last_power_watts,
            last_sample_at_ms,
            updated_at_ms,
        },
    ))
}

/// Overwrites the stored cumulative energy and clears the anchor. Used for
/// diagnostics/reconciliation only; clearing the anchor means the next sample
/// becomes a `FirstSample` anchor instead of integrating from stale state.
pub(crate) fn reset(
    conn: &mut Connection,
    device: &str,
    total_kwh: f64,
    now_ms: i64,
) -> Result<(), StorageError> {
    if device.is_empty() {
        return Err(StorageError::InvalidArgument(
            "device must not be empty".into(),
        ));
    }
    if !total_kwh.is_finite() || total_kwh < 0.0 {
        return Err(StorageError::InvalidArgument(
            "total_kwh must be finite and >= 0".into(),
        ));
    }
    if now_ms <= 0 {
        return Err(StorageError::InvalidArgument("now_ms must be > 0".into()));
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO energy_state (device, total_kwh, last_power_watts, last_sample_at_ms, updated_at_ms)
         VALUES (?1, ?2, NULL, NULL, ?3)
         ON CONFLICT(device) DO UPDATE SET
             total_kwh = excluded.total_kwh,
             last_power_watts = NULL,
             last_sample_at_ms = NULL,
             updated_at_ms = excluded.updated_at_ms",
        params![device, total_kwh, now_ms],
    )?;
    tx.commit()?;
    Ok(())
}
