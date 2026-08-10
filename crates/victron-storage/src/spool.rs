//! Durable, ordered outbound batch spool.
//!
//! Semantics (all transitions are `BEGIN IMMEDIATE` transactions):
//!
//! * `enqueue` inserts a ready batch (attempt 0, claimable immediately).
//! * `claim_oldest` atomically leases the oldest ready batch: it increments
//!   the attempt count and pushes `next_attempt_at_ms` forward by
//!   `spool_inflight_ms`. If the process dies mid-delivery, the batch becomes
//!   claimable again after that window — nothing is lost.
//! * `mark_delivered` removes the batch and bumps `spool.delivered_total`.
//! * `record_retry` schedules `next_attempt_at_ms` with exponential backoff or
//!   drops the batch at `max_spool_attempts` (bumping `spool.dropped_total`).
//! * `prune` enforces the configured count, age and attempt-budget bounds.
//!
//! Delivery is at-least-once: a crash between a successful HTTP POST and the
//! delivery commit causes one duplicate delivery, which the VictoriaMetrics
//! timestamp deduplication absorbs.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::{StorageConfig, StorageError};

/// One undelivered outbound batch.
#[derive(Debug, Clone, PartialEq)]
pub struct SpoolBatch {
    pub id: i64,
    pub device: String,
    pub created_at_ms: i64,
    /// Opaque outbound payload (Prometheus text batch); storage never inspects
    /// its contents.
    pub payload: Vec<u8>,
    pub attempts: u32,
    /// Earliest time this batch may be claimed again.
    pub next_attempt_at_ms: i64,
}

/// Result of [`crate::Storage::record_batch_retry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryOutcome {
    /// The batch stays queued and may be claimed at `next_attempt_at_ms`.
    Retried {
        next_attempt_at_ms: i64,
        attempts: u32,
    },
    /// The batch exhausted its attempt budget and was dropped.
    Dropped { attempts: u32 },
}

/// Result of pruning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PruneStats {
    pub removed: u64,
    pub remaining: u64,
}

/// Snapshot of the spool for health metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpoolStats {
    pub queued_batches: u64,
    pub oldest_created_at_ms: Option<i64>,
    pub total_attempts: u64,
}

const SELECT_READY: &str = "SELECT id, device, created_at_ms, payload, attempts, next_attempt_at_ms
    FROM spool_batch
    WHERE next_attempt_at_ms <= ?1 AND attempts < ?2
    ORDER BY created_at_ms, id
    LIMIT 1";

fn row_to_batch(row: &rusqlite::Row<'_>) -> rusqlite::Result<SpoolBatch> {
    Ok(SpoolBatch {
        id: row.get(0)?,
        device: row.get(1)?,
        created_at_ms: row.get(2)?,
        payload: row.get(3)?,
        attempts: row.get::<_, i64>(4)? as u32,
        next_attempt_at_ms: row.get(5)?,
    })
}

pub(crate) fn enqueue(
    conn: &Connection,
    device: &str,
    payload: Vec<u8>,
    now_ms: i64,
) -> Result<i64, StorageError> {
    if device.is_empty() {
        return Err(StorageError::InvalidArgument(
            "device must not be empty".into(),
        ));
    }
    if payload.is_empty() {
        return Err(StorageError::InvalidArgument(
            "payload must not be empty".into(),
        ));
    }
    if now_ms <= 0 {
        return Err(StorageError::InvalidArgument("now_ms must be > 0".into()));
    }
    conn.execute(
        "INSERT INTO spool_batch (device, created_at_ms, payload, attempts, next_attempt_at_ms)
         VALUES (?1, ?2, ?3, 0, ?2)",
        params![device, now_ms, payload],
    )?;
    Ok(conn.last_insert_rowid())
}

pub(crate) fn peek_oldest(
    conn: &Connection,
    now_ms: i64,
    max_attempts: u32,
) -> Result<Option<SpoolBatch>, StorageError> {
    let mut stmt = conn.prepare(SELECT_READY)?;
    let mut rows = stmt.query(params![now_ms, max_attempts])?;
    match rows.next()? {
        Some(row) => Ok(Some(row_to_batch(row)?)),
        None => Ok(None),
    }
}

pub(crate) fn claim_oldest(
    conn: &mut Connection,
    now_ms: i64,
    cfg: &StorageConfig,
) -> Result<Option<SpoolBatch>, StorageError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let candidate = {
        let mut stmt = tx.prepare(SELECT_READY)?;
        let mut rows = stmt.query(params![now_ms, cfg.max_spool_attempts])?;
        match rows.next()? {
            Some(row) => Some(row_to_batch(row)?),
            None => None,
        }
    };

    let batch = match candidate {
        Some(batch) => batch,
        None => {
            tx.commit()?;
            return Ok(None);
        }
    };

    let next_attempt = now_ms + cfg.spool_inflight_ms;
    let changed = tx.execute(
        "UPDATE spool_batch
         SET attempts = attempts + 1, next_attempt_at_ms = ?2
         WHERE id = ?1 AND attempts < ?3",
        params![batch.id, next_attempt, cfg.max_spool_attempts],
    )?;
    if changed == 0 {
        tx.commit()?;
        return Ok(None);
    }

    tx.commit()?;
    Ok(Some(SpoolBatch {
        attempts: batch.attempts + 1,
        next_attempt_at_ms: next_attempt,
        ..batch
    }))
}

pub(crate) fn mark_delivered(
    conn: &mut Connection,
    id: i64,
    now_ms: i64,
) -> Result<bool, StorageError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute("DELETE FROM spool_batch WHERE id = ?1", params![id])?;
    if changed > 0 {
        crate::database::bump_counter(&tx, "spool.delivered_total", now_ms)?;
    }
    tx.commit()?;
    Ok(changed > 0)
}

pub(crate) fn drop_batch(
    conn: &mut Connection,
    id: i64,
    now_ms: i64,
) -> Result<bool, StorageError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let changed = tx.execute("DELETE FROM spool_batch WHERE id = ?1", params![id])?;
    if changed > 0 {
        crate::database::bump_counter(&tx, "spool.dropped_total", now_ms)?;
    }
    tx.commit()?;
    Ok(changed > 0)
}

pub(crate) fn record_retry(
    conn: &mut Connection,
    id: i64,
    now_ms: i64,
    cfg: &StorageConfig,
) -> Result<RetryOutcome, StorageError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let attempts: Option<i64> = tx
        .query_row(
            "SELECT attempts FROM spool_batch WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()?;
    let attempts = match attempts {
        Some(a) => a as u32,
        None => {
            tx.commit()?;
            return Err(StorageError::Inconsistent(format!(
                "spool batch {id} not found for retry (already delivered or pruned)"
            )));
        }
    };

    if attempts >= cfg.max_spool_attempts {
        tx.execute("DELETE FROM spool_batch WHERE id = ?1", params![id])?;
        crate::database::bump_counter(&tx, "spool.dropped_total", now_ms)?;
        tx.commit()?;
        return Ok(RetryOutcome::Dropped { attempts });
    }

    // Exponential backoff: base * 2^(attempts - 1), capped. The shift exponent
    // is bounded so it can never overflow.
    let exp = (attempts.saturating_sub(1)).min(20) as u32;
    let delay = cfg
        .spool_retry_base_ms
        .saturating_mul(1i64 << exp)
        .min(cfg.spool_retry_max_ms);
    let next = now_ms.saturating_add(delay);

    tx.execute(
        "UPDATE spool_batch SET next_attempt_at_ms = ?2 WHERE id = ?1",
        params![id, next],
    )?;
    tx.commit()?;
    Ok(RetryOutcome::Retried {
        next_attempt_at_ms: next,
        attempts,
    })
}

pub(crate) fn prune(
    conn: &mut Connection,
    now_ms: i64,
    cfg: &StorageConfig,
) -> Result<PruneStats, StorageError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut removed: u64 = 0;

    // Age bound: drop everything older than max_spool_age_ms.
    let age_cutoff = now_ms.saturating_sub(cfg.max_spool_age_ms);
    removed += tx.execute(
        "DELETE FROM spool_batch WHERE created_at_ms < ?1",
        params![age_cutoff],
    )? as u64;

    // Attempt-budget garbage (defensive; record_retry normally drops these).
    removed += tx.execute(
        "DELETE FROM spool_batch WHERE attempts >= ?1",
        params![cfg.max_spool_attempts],
    )? as u64;

    // Count bound: keep the newest max_spool_batches rows.
    removed += tx.execute(
        "DELETE FROM spool_batch
         WHERE id IN (
             SELECT id FROM spool_batch
             ORDER BY created_at_ms DESC, id DESC
             LIMIT -1 OFFSET ?1
         )",
        params![cfg.max_spool_batches as i64],
    )? as u64;

    let remaining: i64 = tx.query_row("SELECT COUNT(*) FROM spool_batch", [], |row| row.get(0))?;
    tx.commit()?;
    Ok(PruneStats {
        removed,
        remaining: remaining as u64,
    })
}

pub(crate) fn stats(conn: &Connection) -> Result<SpoolStats, StorageError> {
    let queued: i64 = conn.query_row("SELECT COUNT(*) FROM spool_batch", [], |row| row.get(0))?;
    let oldest: Option<i64> =
        conn.query_row("SELECT MIN(created_at_ms) FROM spool_batch", [], |row| {
            row.get(0)
        })?;
    let total_attempts: i64 = conn.query_row(
        "SELECT COALESCE(SUM(attempts), 0) FROM spool_batch",
        [],
        |row| row.get(0),
    )?;
    Ok(SpoolStats {
        queued_batches: queued as u64,
        oldest_created_at_ms: oldest,
        total_attempts: total_attempts as u64,
    })
}
