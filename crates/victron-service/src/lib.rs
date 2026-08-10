//! Application orchestration for the Victron VE.Smart BLE collector.
//!
//! This crate owns the collector *use cases*: the bounded cycle state machine
//! (discover/connect/subscribe/negotiate/request/collect/persist/deliver/
//! disconnect/backoff), deterministic retry/backoff, active/idle interval
//! switching driven by the last committed sample's solar activity, durable
//! energy fallback integration and delivery-spool coordination.
//!
//! It deliberately contains **no** protocol, BLE, database or HTTP logic.
//! Everything external is behind narrow port traits ([`ports`]); the binaries
//! inject concrete adapters. The canonical domain model (`Sample`,
//! `ChargerState`, `DeviceId`, `Quality`, ...) comes from `victron-domain`
//! and is re-exported here; this crate owns no parallel sample model.
//!
//! # Design rules
//!
//! - One device, one in-flight acquisition cycle, Tokio current-thread
//!   friendly; the BLE session trait is `?Send` so the BlueZ transport can be
//!   awaited without a `Send` requirement.
//! - Deterministic backoff: no randomness anywhere in the scheduling path.
//! - Explicit timeouts on every BLE phase (`phase_timeout`), plus the protocol
//!   response timeout handed to the session.
//! - Graceful cancellation: a `watch` shutdown signal is honoured at phase
//!   boundaries; a closed channel (sender dropped) is also shutdown. Anything
//!   already made durable stays durable.
//! - Atomic acquisition persistence: one `StoragePort::commit_acquisition`
//!   call persists energy state, acquisition identity and the rendered batch
//!   in a single transaction, idempotent per `(device, observed_at)`.
//! - Delivery ownership: batches are claimed one at a time and completed only
//!   after the network call succeeds; crash recovery is bounded by a claim
//!   TTL. Drops are a separate storage operation and never count as
//!   deliveries.
//! - Illegal state transitions surface as typed errors ([`CycleError::State`],
//!   [`RunError::State`]) instead of being silently swallowed in release
//!   builds.

pub mod config;
pub mod cycle;
pub mod delivery;
pub mod energy;
pub mod health;
pub mod model;
pub mod ports;
pub mod runner;
pub mod scheduler;
pub mod state;

pub use config::{ConfigError, ServiceConfig};
pub use cycle::{run_cycle, CycleContext, CycleError, CycleOutcome, CycleResult};
pub use delivery::{deliver_claim, drain_spool, DeliveryStatus, DrainResult};
pub use energy::{EnergyKind, EnergyOutcome, EnergyPolicy};
pub use health::{HealthCounters, HealthSnapshot};
pub use model::DeviceIdentity;
pub use ports::ble::{BleError, BleSession};
pub use ports::clock::{Clock, SystemClock};
pub use ports::delivery::{
    BatchRenderer, DeliveryError, MetricsDelivery, RenderContext, RenderError,
};
pub use ports::protocol::{AcquirePlan, ProtocolAdapter, ProtocolError, RawValue};
pub use ports::storage::{
    AcquisitionCommit, AcquisitionCommitOutcome, ClaimedBatch, EnergyState, RetryOutcome,
    SpoolHealth, StorageError, StoragePort,
};
pub use ports::CyclePorts;
pub use runner::{run, RunError, RunSummary};
pub use scheduler::{
    solar_activity, BackoffPolicy, ConstantIntervalPolicy, ExponentialBackoff, IntervalContext,
    IntervalKind, IntervalPolicy, SolarActivity, SolarActivityPolicy,
};
pub use state::{
    CyclePhase, NoopObserver, PhaseObserver, RecordingObserver, StateMachine, StateTransitionError,
};

// Canonical domain model, re-exported so consumers use one vocabulary.
pub use victron_domain::{ChargerState, ConnectionHealth, DeviceId, LoadState, Quality, Sample};
