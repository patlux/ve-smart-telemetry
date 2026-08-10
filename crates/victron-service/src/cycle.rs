//! One acquisition cycle: the bounded state machine walk.
//!
//! The runner drives repeated cycles; this module implements a single cycle
//! over [`CycleContext`]. Shutdown is honoured at phase boundaries: an
//! in-flight BLE phase completes (bounded by `phase_timeout`), anything made
//! durable stays durable, and the cycle ends with a graceful teardown.
//!
//! Persistence is **atomic**: one [`crate::ports::storage::StoragePort::commit_acquisition`]
//! call persists the next energy state, the acquisition identity
//! (`observed_at`) and the rendered batch in a single transaction. The
//! sample's own `observed_at` — never a later orchestration clock reading —
//! drives energy integration intervals and the persisted acquisition
//! identity.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::watch;
use victron_domain::{ConnectionHealth, Sample};

use crate::config::ServiceConfig;
use crate::delivery::{deliver_claim, DeliveryStatus};
use crate::energy::{EnergyOutcome, EnergyPolicy};
use crate::health::HealthCounters;
use crate::model::DeviceIdentity;
use crate::ports::ble::BleError;
use crate::ports::clock::Clock;
use crate::ports::delivery::{RenderContext, RenderError};
use crate::ports::protocol::ProtocolError;
use crate::ports::storage::{AcquisitionCommit, AcquisitionCommitOutcome, StorageError};
use crate::ports::CyclePorts;
use crate::scheduler::{BackoffPolicy, IntervalPolicy};
use crate::state::{CyclePhase, PhaseObserver, StateMachine, StateTransitionError};

/// Phase-level failure of one cycle.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CycleError {
    #[error("discover: {0}")]
    Discover(BleError),
    #[error("connect: {0}")]
    Connect(BleError),
    /// The protocol adapter failed to produce the acquire plan.
    #[error("plan: {0}")]
    Plan(ProtocolError),
    #[error("negotiate: {0}")]
    Negotiate(BleError),
    #[error("subscribe: {0}")]
    Subscribe(BleError),
    #[error("request: {0}")]
    Request(BleError),
    /// Parsing or normalization failed.
    #[error("parse: {0}")]
    Parse(ProtocolError),
    /// Persisting the sample/energy/batch failed (sample is not durable).
    #[error("persist: {0}")]
    Persist(StorageError),
    /// Rendering the batch failed (sample is not durable).
    #[error("render: {0}")]
    Render(RenderError),
    #[error("disconnect: {0}")]
    Disconnect(BleError),
    /// Internal state machine failure (programming error, never a device
    /// condition). Surfaced as a typed error instead of being silently
    /// swallowed in release builds.
    #[error("internal state failure: {0}")]
    State(StateTransitionError),
}

/// Outcome of one cycle.
#[derive(Debug, Clone, PartialEq)]
pub enum CycleOutcome {
    Success(CycleResult),
    Failure {
        phase: CyclePhase,
        error: CycleError,
    },
    /// Shutdown requested at a phase boundary; the sample was acquired and
    /// durably persisted, delivery was skipped.
    ShutdownGraceful {
        phase: CyclePhase,
        sample: Sample,
    },
}

/// Everything the cycle produced for reporting/rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct CycleResult {
    pub device: DeviceIdentity,
    pub sample: Sample,
    /// Resolved cumulative kWh (native or integrated) for metrics.
    pub resolved_yield_kwh: f64,
    pub energy: EnergyOutcome,
    /// What happened to the freshly enqueued batch during in-cycle delivery.
    pub delivery: DeliveryStatus,
    /// True when this sample's `observed_at` was already durably committed
    /// (idempotent replay): nothing was rendered or enqueued this cycle.
    pub duplicate: bool,
}

/// All state one collector instance needs for a cycle.
pub struct CycleContext {
    pub config: ServiceConfig,
    pub ports: CyclePorts,
    pub clock: Arc<dyn Clock>,
    /// `true` once a graceful shutdown has been requested; a closed channel
    /// (sender dropped) is also treated as shutdown.
    pub shutdown: watch::Receiver<bool>,
    pub state: StateMachine,
    pub observer: Arc<dyn PhaseObserver>,
    pub backoff: Arc<dyn BackoffPolicy>,
    pub interval: Arc<dyn IntervalPolicy>,
    pub energy: EnergyPolicy,
    pub health: HealthCounters,
}

impl CycleContext {
    pub fn new(
        config: ServiceConfig,
        ports: CyclePorts,
        clock: Arc<dyn Clock>,
        shutdown: watch::Receiver<bool>,
        observer: Arc<dyn PhaseObserver>,
        backoff: Arc<dyn BackoffPolicy>,
        interval: Arc<dyn IntervalPolicy>,
    ) -> Self {
        let energy = EnergyPolicy {
            maximum_gap: config.maximum_energy_gap,
        };
        Self {
            config,
            ports,
            clock,
            shutdown,
            state: StateMachine::new(),
            observer,
            backoff,
            interval,
            energy,
            health: HealthCounters::new(),
        }
    }
}

/// Transition the state machine; an illegal transition is a typed
/// programming error, never silently logged away.
fn goto(ctx: &mut CycleContext, phase: CyclePhase) -> Result<(), StateTransitionError> {
    ctx.state.transition(phase)?;
    ctx.observer.on_phase(phase);
    Ok(())
}

/// `goto` mapped onto the cycle-step error type.
fn goto_phase(ctx: &mut CycleContext, phase: CyclePhase) -> Result<(), (CyclePhase, CycleError)> {
    goto(ctx, phase).map_err(|e| (phase, CycleError::State(e)))
}

/// True when shutdown was requested **or** the shutdown sender was dropped
/// (a closed channel is treated as shutdown; without this a vanished sender
/// would leave the runner polling forever).
pub(crate) fn shutdown_requested(ctx: &mut CycleContext) -> bool {
    if *ctx.shutdown.borrow() {
        return true;
    }
    ctx.shutdown.has_changed().is_err()
}

/// Run one acquisition cycle from `Idle` back to `Idle` (or Backoff /
/// ShuttingDown). See module docs for the shutdown semantics.
pub async fn run_cycle(ctx: &mut CycleContext) -> CycleOutcome {
    match cycle_states(ctx).await {
        Ok(Step::Done(result)) => {
            debug_assert_eq!(ctx.state.current(), CyclePhase::Idle);
            CycleOutcome::Success(result)
        }
        Ok(Step::Shutdown(phase, sample)) => finish_gracefully(ctx, phase, sample).await,
        Err((phase, error)) => {
            record_failure_health(ctx, &error);
            match teardown_after_failure(ctx).await {
                Ok(()) => CycleOutcome::Failure { phase, error },
                Err(state_error) => CycleOutcome::Failure {
                    phase,
                    error: state_error,
                },
            }
        }
    }
}

/// Best-effort teardown after a failed phase, reflected in the state machine:
/// walk through `Disconnecting` (the failing phase may hold a connection),
/// then to `Backoff`. A failed disconnect is logged, not fatal.
async fn teardown_after_failure(ctx: &mut CycleContext) -> Result<(), CycleError> {
    if ctx.state.current() != CyclePhase::Disconnecting {
        goto(ctx, CyclePhase::Disconnecting).map_err(CycleError::State)?;
    }
    let _ = tokio::time::timeout(ctx.config.phase_timeout, ctx.ports.ble.disconnect()).await;
    goto(ctx, CyclePhase::Backoff).map_err(CycleError::State)
}

async fn finish_gracefully(
    ctx: &mut CycleContext,
    interrupted_at: CyclePhase,
    sample: Sample,
) -> CycleOutcome {
    match teardown_to_shutdown(ctx).await {
        Ok(()) => CycleOutcome::ShutdownGraceful {
            phase: interrupted_at,
            sample,
        },
        Err(e) => CycleOutcome::Failure {
            phase: interrupted_at,
            error: e,
        },
    }
}

async fn teardown_to_shutdown(ctx: &mut CycleContext) -> Result<(), CycleError> {
    goto(ctx, CyclePhase::Disconnecting).map_err(CycleError::State)?;
    let _ = tokio::time::timeout(ctx.config.phase_timeout, ctx.ports.ble.disconnect()).await;
    goto(ctx, CyclePhase::ShuttingDown).map_err(CycleError::State)
}

fn record_failure_health(ctx: &mut CycleContext, error: &CycleError) {
    match error {
        CycleError::Discover(_) => ctx.health.record_ble_discover_failure(),
        CycleError::Connect(_) => ctx.health.record_ble_connect_failure(),
        CycleError::Negotiate(_)
        | CycleError::Subscribe(_)
        | CycleError::Request(_)
        | CycleError::Disconnect(_) => ctx.health.record_ble_session_failure(),
        CycleError::Plan(_) | CycleError::Parse(_) => ctx.health.record_protocol_error(),
        CycleError::Persist(_) | CycleError::Render(_) => ctx.health.record_sample_dropped(),
        // A state machine failure is a programming error; no device counter
        // applies.
        CycleError::State(_) => {}
    }
}

enum Step {
    Done(CycleResult),
    Shutdown(CyclePhase, Sample),
}

type CycleStepResult = Result<Step, (CyclePhase, CycleError)>;

async fn cycle_states(ctx: &mut CycleContext) -> CycleStepResult {
    let device = ctx.config.device();

    goto_phase(ctx, CyclePhase::Discovering)?;
    phase(ctx.config.phase_timeout, ctx.ports.ble.discover())
        .await
        .map_err(|e| (CyclePhase::Discovering, CycleError::Discover(e)))?;

    goto_phase(ctx, CyclePhase::Connecting)?;
    phase(ctx.config.phase_timeout, ctx.ports.ble.connect())
        .await
        .map_err(|e| (CyclePhase::Connecting, CycleError::Connect(e)))?;

    goto_phase(ctx, CyclePhase::Negotiating)?;
    let plan = ctx
        .ports
        .protocol
        .acquire_plan(ctx.config.instance, ctx.ports.protocol.vregs())
        .map_err(|e| (CyclePhase::Negotiating, CycleError::Plan(e)))?;
    phase(
        ctx.config.phase_timeout,
        ctx.ports.ble.negotiate(&plan.negotiation_frames),
    )
    .await
    .map_err(|e| (CyclePhase::Negotiating, CycleError::Negotiate(e)))?;

    goto_phase(ctx, CyclePhase::Subscribing)?;
    phase(
        ctx.config.phase_timeout,
        ctx.ports
            .ble
            .subscribe(ctx.config.instance, &plan.subscribe_payload),
    )
    .await
    .map_err(|e| (CyclePhase::Subscribing, CycleError::Subscribe(e)))?;

    goto_phase(ctx, CyclePhase::Requesting)?;
    let raw = phase(
        ctx.config.phase_timeout,
        ctx.ports
            .ble
            .request_values(&plan.values_payload, ctx.config.response_timeout),
    )
    .await
    .map_err(|e| (CyclePhase::Requesting, CycleError::Request(e)))?;

    goto_phase(ctx, CyclePhase::Collecting)?;
    let values = ctx
        .ports
        .protocol
        .parse_response(ctx.config.instance, &raw)
        .map_err(|e| (CyclePhase::Collecting, CycleError::Parse(e)))?;
    let sample = ctx
        .ports
        .protocol
        .translate(ctx.config.instance, &values)
        .map_err(|e| (CyclePhase::Collecting, CycleError::Parse(e)))?;
    if sample.is_empty() {
        return Err((
            CyclePhase::Collecting,
            CycleError::Parse(ProtocolError::EmptyResponse),
        ));
    }

    goto_phase(ctx, CyclePhase::Persisting)?;
    let observed_at = sample.observed_at();
    // Seam guard: epoch/pre-epoch timestamps are rejected before any
    // persistence (the storage adapter enforces the same positive-ms rule).
    if observed_at <= SystemTime::UNIX_EPOCH {
        return Err((
            CyclePhase::Persisting,
            CycleError::Persist(StorageError::InvalidTimestamp(observed_at)),
        ));
    }
    // Reads needed to prepare the commit stay separate from the commit.
    let prev_energy = ctx
        .ports
        .storage
        .energy_state()
        .await
        .map_err(|e| (CyclePhase::Persisting, CycleError::Persist(e)))?;
    let energy = ctx.energy.apply(prev_energy.clone(), &sample);
    let last_success = ctx
        .ports
        .storage
        .last_success()
        .await
        .map_err(|e| (CyclePhase::Persisting, CycleError::Persist(e)))?;
    // Idempotent replay: this observed_at was already durably committed.
    // Nothing is rendered or enqueued again; the commit below is the
    // authoritative backstop for races.
    let mut duplicate = last_success.is_some_and(|ls| observed_at <= ls);

    let mut delivery = DeliveryStatus::Skipped;
    if !duplicate {
        // Query spool health before rendering and project the newly enqueued
        // batch consistently (depth + 1; the new batch is the newest, so the
        // oldest age is unchanged unless the spool was empty).
        let spool = ctx
            .ports
            .storage
            .spool_health(ctx.clock.now())
            .await
            .map_err(|e| (CyclePhase::Persisting, CycleError::Persist(e)))?;
        let projected_depth = spool.depth + 1;
        let projected_oldest = spool.oldest_age.or(Some(Duration::ZERO));
        let sample_age = ctx.clock.now().duration_since(observed_at).ok();
        let gap_seconds = energy.skipped_gap_seconds.map(|d| d.as_secs()).unwrap_or(0);
        let render_ctx = RenderContext {
            device: &device.device,
            sample: &sample,
            resolved_yield_kwh: energy.total_kwh,
            energy_kind: energy.kind,
            ble_up: sample
                .connection_health()
                .map(|h| h == ConnectionHealth::Up),
            ble_rssi_dbm: sample.ble_rssi_dbm().map(|m| m.value() as i32),
            // The sample being committed IS a success: project its observed
            // time as the current successful acquisition timestamp.
            last_success: Some(observed_at),
            sample_age,
            health: &ctx.health.snapshot(),
            spool_depth: projected_depth,
            spool_oldest_age: projected_oldest,
            energy_gap_skipped_seconds: ctx.health.energy_gap_skipped_seconds() + gap_seconds,
        };
        let payload = ctx
            .ports
            .renderer
            .render(&render_ctx)
            .map_err(|e| (CyclePhase::Persisting, CycleError::Render(e)))?;
        let commit = AcquisitionCommit {
            device: device.device.clone(),
            observed_at,
            expected_energy: prev_energy,
            next_energy: energy.next_state.clone(),
            payload,
        };
        let outcome = ctx
            .ports
            .storage
            .commit_acquisition(commit)
            .await
            .map_err(|e| (CyclePhase::Persisting, CycleError::Persist(e)))?;
        match outcome {
            AcquisitionCommitOutcome::Committed => {
                // The gap is durable now: record it in the cumulative counter.
                if let Some(gap) = energy.skipped_gap_seconds {
                    ctx.health.record_energy_gap(gap);
                }
            }
            AcquisitionCommitOutcome::AlreadyCommitted => {
                // Raced with a concurrent commit of the same observed_at.
                duplicate = true;
            }
        }
    }

    // Graceful shutdown is honoured at the delivery boundary: an in-flight
    // acquisition completes, the sample is durably persisted, and delivery
    // is skipped. Anything already durable stays durable.
    if shutdown_requested(ctx) {
        return Ok(Step::Shutdown(CyclePhase::Delivering, sample));
    }

    if !duplicate {
        goto_phase(ctx, CyclePhase::Delivering)?;
        delivery = match ctx
            .ports
            .storage
            .spool_claim_next(ctx.config.spool_claim_ttl, ctx.clock.now())
            .await
        {
            Ok(Some(claim)) => deliver_claim(ctx, claim).await,
            Ok(None) => {
                // Freshly enqueued batch not visible yet: leave it for the
                // next drain pass rather than synthesizing success.
                tracing::warn!("fresh batch not claimable after enqueue; will drain later");
                DeliveryStatus::Queued { attempts: 0 }
            }
            Err(e) => DeliveryStatus::Failed(e),
        };
    }

    goto_phase(ctx, CyclePhase::Disconnecting)?;
    phase(ctx.config.phase_timeout, ctx.ports.ble.disconnect())
        .await
        .map_err(|e| (CyclePhase::Disconnecting, CycleError::Disconnect(e)))?;

    goto_phase(ctx, CyclePhase::Idle)?;
    Ok(Step::Done(CycleResult {
        device,
        sample,
        resolved_yield_kwh: energy.total_kwh,
        energy,
        delivery,
        duplicate,
    }))
}

/// Wrap one BLE phase with the hard outer timeout.
async fn phase<T>(
    timeout: Duration,
    future: impl Future<Output = Result<T, BleError>>,
) -> Result<T, BleError> {
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| BleError::Timeout)?
}
