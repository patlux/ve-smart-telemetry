//! Delivery-spool coordination.
//!
//! Batches are claimed one at a time (single owner), delivered, then completed
//! or retried. A claimed batch's `attempts` is the **current 1-based attempt**
//! (claiming increments the stored counter); the service never adds another
//! attempt. A batch is dropped when the current attempt reaches the configured
//! maximum, or immediately on a permanent rejection. Successful completion and
//! drops are separate storage operations, so a drop can never increment a
//! delivered counter.
//!
//! `drain_spool` replays queued batches oldest-first before the next
//! acquisition; `deliver_claim` is shared with in-cycle delivery.

use crate::cycle::{shutdown_requested, CycleContext};
use crate::ports::storage::{ClaimedBatch, RetryOutcome, StorageError};
use crate::state::CyclePhase;

/// Result of delivering one batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryStatus {
    /// Delivered and spool claim completed.
    Delivered,
    /// Delivery failed (retryable) and the batch was re-queued.
    Queued { attempts: u32 },
    /// Bounded drop: permanent rejection or retry budget exhausted.
    Dropped { attempts: u32 },
    /// No batch was enqueued for this acquisition (idempotent replay of an
    /// already-committed sample).
    Skipped,
    /// Storage bookkeeping failed; the batch may be re-claimed after TTL.
    Failed(StorageError),
}

/// Result of one drain pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainResult {
    /// Spool is empty (nothing due).
    Empty,
    /// At least one batch was delivered before the spool emptied.
    Delivered(u32),
    /// Shutdown requested mid-drain; stop and let the runner exit gracefully.
    StoppedGracefully,
    /// Storage failed; stop draining to avoid a tight error loop.
    Error(StorageError),
}

/// Deliver one claimed batch and update spool ownership accordingly.
pub async fn deliver_claim(ctx: &mut CycleContext, claim: ClaimedBatch) -> DeliveryStatus {
    ctx.observer.on_progress(CyclePhase::Delivering);
    let result = ctx.ports.delivery.deliver(&claim.payload).await;
    // Bound the following SQLite bookkeeping independently from the HTTP
    // request that just completed.
    ctx.observer.on_progress(CyclePhase::Delivering);
    match result {
        Ok(()) => match ctx.ports.storage.spool_complete(&claim).await {
            Ok(()) => {
                ctx.health.record_delivery(true);
                DeliveryStatus::Delivered
            }
            Err(e) => {
                // Delivered but bookkeeping failed; the TTL will eventually
                // re-claim it and VictoriaMetrics dedups by timestamp/series.
                ctx.health.record_delivery(true);
                DeliveryStatus::Failed(e)
            }
        },
        Err(e) => {
            ctx.health.record_delivery(false);
            if e.retryable() {
                if claim.attempts >= ctx.config.spool_max_attempts {
                    // The current attempt reached the configured maximum:
                    // drop the batch (bounded spool).
                    drop_claim(ctx, &claim).await
                } else {
                    // The adapter schedules the next attempt from its own
                    // bounded backoff; it may still drop if its own budget is
                    // exhausted.
                    match ctx.ports.storage.spool_retry(&claim, ctx.clock.now()).await {
                        Ok(RetryOutcome::Retried { attempts }) => {
                            DeliveryStatus::Queued { attempts }
                        }
                        Ok(RetryOutcome::Dropped { attempts }) => {
                            ctx.health.record_spool_dropped();
                            DeliveryStatus::Dropped { attempts }
                        }
                        Err(e2) => DeliveryStatus::Failed(e2),
                    }
                }
            } else {
                // Permanent rejection: dropping bounds the spool.
                tracing::warn!(error = ?e, "permanent delivery failure; dropping batch");
                drop_claim(ctx, &claim).await
            }
        }
    }
}

/// Remove a claimed batch from the spool and count the bounded drop. Uses the
/// dedicated drop operation so a drop never increments a delivered counter.
async fn drop_claim(ctx: &mut CycleContext, claim: &ClaimedBatch) -> DeliveryStatus {
    match ctx.ports.storage.spool_drop(claim).await {
        Ok(()) => {
            ctx.health.record_spool_dropped();
            DeliveryStatus::Dropped {
                attempts: claim.attempts,
            }
        }
        Err(e) => DeliveryStatus::Failed(e),
    }
}

/// Replay queued batches oldest-first until the spool is empty, storage
/// fails, or shutdown is requested.
pub async fn drain_spool(ctx: &mut CycleContext) -> DrainResult {
    let mut delivered = 0u32;
    loop {
        if shutdown_requested(ctx) {
            return DrainResult::StoppedGracefully;
        }
        // Spool replay happens before the acquisition state machine enters
        // Delivering. Still report each claim as concrete delivery progress
        // so a stuck storage/HTTP operation cannot hide behind an idle budget.
        ctx.observer.on_progress(CyclePhase::Delivering);
        if shutdown_requested(ctx) {
            return DrainResult::StoppedGracefully;
        }
        match ctx
            .ports
            .storage
            .spool_claim_next(ctx.config.spool_claim_ttl, ctx.clock.now())
            .await
        {
            Ok(None) => {
                return if delivered > 0 {
                    DrainResult::Delivered(delivered)
                } else {
                    DrainResult::Empty
                }
            }
            Ok(Some(claim)) => match deliver_claim(ctx, claim).await {
                DeliveryStatus::Delivered => delivered += 1,
                // Re-queued, dropped or skipped: continue with the next due
                // batch.
                DeliveryStatus::Queued { .. }
                | DeliveryStatus::Dropped { .. }
                | DeliveryStatus::Skipped => {}
                DeliveryStatus::Failed(e) => return DrainResult::Error(e),
            },
            Err(e) => return DrainResult::Error(e),
        }
    }
}
