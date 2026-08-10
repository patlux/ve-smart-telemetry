//! The collector run loop.
//!
//! Single-threaded, one device, one in-flight cycle. Each iteration:
//!
//! 1. drain the delivery spool oldest-first (replay before fresh acquisition);
//! 2. check shutdown (a closed shutdown channel counts as shutdown);
//! 3. wait the scheduled interval (active/idle per the last committed
//!    sample's solar activity) plus deterministic backoff when cycles fail;
//! 4. run one acquisition cycle.
//!
//! Shutdown returns a [`RunSummary`]; the process may exit `0` gracefully.
//! An illegal state transition is a typed [`RunError`], never silently
//! swallowed in release builds.

use std::time::Instant;

use tokio::sync::watch;
use tracing::Instrument;

use crate::cycle::{run_cycle, shutdown_requested, CycleContext, CycleOutcome};
use crate::delivery::drain_spool;
use crate::health::HealthSnapshot;
use crate::scheduler::{solar_activity, IntervalContext, IntervalKind, SolarActivity};
use crate::state::{CyclePhase, StateTransitionError};

/// Outcome of a terminated run loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSummary {
    pub cycles: u64,
    pub cycles_succeeded: u64,
    /// True when the loop exited because shutdown was requested.
    pub graceful: bool,
    pub health: HealthSnapshot,
}

/// A fatal run-loop failure (programming error, not a device condition).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    #[error("internal state machine failure: {0}")]
    State(StateTransitionError),
}

/// Run the collector until shutdown is requested.
pub async fn run(mut ctx: CycleContext) -> Result<RunSummary, RunError> {
    let mut cycles: u64 = 0;
    let mut cycles_succeeded: u64 = 0;
    // Solar activity of the last successfully committed sample; `None` until
    // the first commit (first cycle uses the active cadence).
    let mut last_solar: Option<SolarActivity> = None;
    // The loop only exits via shutdown paths, so graceful is always true.
    let graceful: bool;

    loop {
        ctx.observer.on_progress(ctx.state.current());
        match drain_spool(&mut ctx).await {
            crate::delivery::DrainResult::Error(error) => {
                tracing::error!(
                    error_kind = error.kind(),
                    "spool drain failed; continuing to poll"
                );
            }
            crate::delivery::DrainResult::Delivered(n) => {
                tracing::info!(delivered = n, "spool drained");
            }
            _ => {}
        }
        // Spool replay reports Delivering progress even though it runs before
        // the acquisition state machine. Restore the actual state-machine
        // phase before shutdown checks or an interval sleep.
        ctx.observer.on_progress(ctx.state.current());

        if shutdown_requested(&mut ctx) {
            disconnect_for_shutdown(&mut ctx).await;
            goto_idle_or_shutdown(&mut ctx)?;
            graceful = true;
            break;
        }

        if ctx.state.current() != CyclePhase::Idle {
            goto_idle(&mut ctx)?;
        }

        // Schedule the next poll: interval policy + deterministic backoff.
        if cycles > 0 {
            let now = ctx.clock.now();
            let consecutive_failures = ctx.health.consecutive_failures();
            let kind = ctx.interval.kind(&IntervalContext {
                now,
                last_success: ctx.health.last_success(),
                last_solar,
                consecutive_failures,
            });
            let base = match kind {
                IntervalKind::Active => ctx.config.active_interval,
                IntervalKind::Idle => ctx.config.idle_interval,
            };
            let backoff = ctx.backoff.delay(consecutive_failures);
            let wait = base.saturating_add(backoff);
            tracing::debug!(
                interval_kind = ?kind,
                base_interval_ms = base.as_millis() as u64,
                backoff_ms = backoff.as_millis() as u64,
                wait_ms = wait.as_millis() as u64,
                consecutive_failures,
                "scheduled next acquisition cycle"
            );
            if !wait.is_zero() {
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = ctx.shutdown.changed() => {}
                }
                if shutdown_requested(&mut ctx) {
                    disconnect_for_shutdown(&mut ctx).await;
                    goto_idle_or_shutdown(&mut ctx)?;
                    graceful = true;
                    break;
                }
            }
        }

        let cycle_id = cycles.saturating_add(1);
        let cycle_started_at = Instant::now();
        let span = tracing::info_span!(
            "acquisition_cycle",
            cycle_id,
            device = %ctx.config.device_name,
            instance = ctx.config.instance
        );
        let outcome = run_cycle(&mut ctx).instrument(span).await;
        let elapsed_ms = cycle_started_at.elapsed().as_millis() as u64;
        cycles += 1;
        match outcome {
            CycleOutcome::Success(result) => {
                cycles_succeeded += 1;
                last_solar = Some(solar_activity(
                    &result.sample,
                    ctx.config.solar_active_threshold_watts,
                ));
                ctx.health.record_cycle(true);
                ctx.health.set_last_success(result.sample.observed_at());
                tracing::info!(
                    cycle_id,
                    device = %result.device.name(),
                    elapsed_ms,
                    energy_kind = ?result.energy.kind,
                    duplicate = result.duplicate,
                    delivery = ?result.delivery,
                    "acquisition cycle succeeded"
                );
            }
            CycleOutcome::Failure { phase, error } => {
                ctx.health.record_cycle(false);
                tracing::warn!(
                    cycle_id,
                    device = %ctx.config.device_name,
                    elapsed_ms,
                    phase = %phase.as_str(),
                    error_kind = error.kind(),
                    operation = error.operation().unwrap_or("none"),
                    "acquisition cycle failed"
                );
            }
            CycleOutcome::ShutdownGraceful { phase, sample } => {
                // The acquisition completed and was durably persisted before
                // shutdown; count it as a successful acquisition.
                cycles_succeeded += 1;
                ctx.health.record_cycle(true);
                ctx.health.set_last_success(sample.observed_at());
                tracing::info!(
                    cycle_id,
                    elapsed_ms,
                    phase = %phase.as_str(),
                    "acquisition persisted before graceful shutdown"
                );
                graceful = true;
                break;
            }
        }
    }

    Ok(RunSummary {
        cycles,
        cycles_succeeded,
        graceful,
        health: ctx.health.snapshot(),
    })
}

async fn disconnect_for_shutdown(ctx: &mut CycleContext) {
    ctx.observer.on_progress(CyclePhase::Disconnecting);
    let _ = tokio::time::timeout(ctx.config.phase_timeout, ctx.ports.ble.disconnect()).await;
}

fn goto_idle(ctx: &mut CycleContext) -> Result<(), RunError> {
    goto(ctx, CyclePhase::Idle).map_err(RunError::State)
}

fn goto_idle_or_shutdown(ctx: &mut CycleContext) -> Result<(), RunError> {
    if ctx.state.current() != CyclePhase::ShuttingDown {
        goto(ctx, CyclePhase::ShuttingDown).map_err(RunError::State)?;
    }
    Ok(())
}

fn goto(ctx: &mut CycleContext, phase: CyclePhase) -> Result<(), StateTransitionError> {
    ctx.state.transition(phase)?;
    ctx.observer.on_phase(phase);
    Ok(())
}

/// Helper for tests and binaries: install a shutdown watch pair.
pub fn shutdown_channel() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}
