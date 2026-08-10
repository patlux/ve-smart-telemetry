//! Shared fakes and harness for the service integration tests.
//!
//! All timing is deterministic: `#[tokio::test(start_paused = true)]` for
//! async waits, plus a manual wall clock for persistence/retry deadlines.
//! Fakes are shared through `Arc<Mutex<_>>` and the port traits are
//! implemented for the Arc wrapper, so tests keep full access to scripts,
//! gates and recorded calls after the runner consumes the context.
//!
//! The runner future is `!Send` (the BLE session trait is `?Send` to match
//! the BlueZ lane), so tests drive it through a `tokio::task::LocalSet` via
//! [`Harness::drive`].
//!
//! This module is compiled into every test binary, and each binary uses a
//! different subset of the fixtures; dead-code analysis is therefore
//! suppressed for the shared fixture surface.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use tokio::sync::watch;

use victron_service::*;

mod fakes;
mod storage;

pub use fakes::*;
pub use storage::*;

pub const FIXED_TS: u64 = 1_700_000_000;

pub fn t(secs: u64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
}

// ---------------------------------------------------------------------------
// Clock + interval policy fakes
// ---------------------------------------------------------------------------

pub struct ManualClock {
    now: Mutex<SystemTime>,
}

impl ManualClock {
    pub fn at(secs: u64) -> Self {
        Self {
            now: Mutex::new(t(secs)),
        }
    }

    pub fn set(&self, secs: u64) {
        *self.now.lock().unwrap() = t(secs);
    }

    pub fn advance(&self, d: Duration) {
        let mut now = self.now.lock().unwrap();
        *now += d;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> SystemTime {
        *self.now.lock().unwrap()
    }
}

/// Switches interval kinds on each call, then stays Idle.
pub struct SwitchPolicy {
    pub seq: Mutex<VecDeque<IntervalKind>>,
}

impl IntervalPolicy for SwitchPolicy {
    fn kind(&self, _ctx: &IntervalContext) -> IntervalKind {
        self.seq
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(IntervalKind::Idle)
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

pub struct Harness {
    pub ctx: Option<CycleContext>,
    pub shutdown_tx: watch::Sender<bool>,
    pub ble: Arc<Mutex<FakeBle>>,
    pub ble_calls: Arc<Mutex<BleCalls>>,
    pub protocol: Arc<Mutex<FakeProtocol>>,
    pub storage: Arc<Mutex<FakeStorage>>,
    pub delivery: Arc<Mutex<FakeDelivery>>,
    pub delivery_calls: Arc<Mutex<DeliveryCalls>>,
    pub renderer: Arc<Mutex<FakeRenderer>>,
    pub clock: Arc<ManualClock>,
    pub observer: Arc<RecordingObserver>,
    local: tokio::task::LocalSet,
}

impl Harness {
    pub fn new(config: ServiceConfig) -> Self {
        let ble_calls = Arc::new(Mutex::new(BleCalls::default()));
        let delivery_calls = Arc::new(Mutex::new(DeliveryCalls::default()));
        let ble = Arc::new(Mutex::new(FakeBle::new(Arc::clone(&ble_calls))));
        let protocol = Arc::new(Mutex::new(FakeProtocol::new()));
        let storage = Arc::new(Mutex::new(FakeStorage::new()));
        let delivery = Arc::new(Mutex::new(FakeDelivery::new(Arc::clone(&delivery_calls))));
        let renderer = Arc::new(Mutex::new(FakeRenderer::new()));
        let clock = Arc::new(ManualClock::at(0));
        let observer = Arc::new(RecordingObserver::new());

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let ports = CyclePorts::new(
            Box::new(SharedBle(Arc::clone(&ble))),
            Arc::new(SharedProtocol(Arc::clone(&protocol))) as Arc<dyn ProtocolAdapter>,
            Box::new(SharedStorage(Arc::clone(&storage))),
            Box::new(SharedDelivery(Arc::clone(&delivery))),
            Arc::new(SharedRenderer(Arc::clone(&renderer))) as Arc<dyn BatchRenderer>,
        );
        let ctx = CycleContext::new(
            config.clone(),
            ports,
            Arc::clone(&clock) as Arc<dyn Clock>,
            shutdown_rx,
            Arc::clone(&observer) as Arc<dyn PhaseObserver>,
            Arc::new(config.backoff()),
            Arc::new(ConstantIntervalPolicy::active()),
        );
        Self {
            ctx: Some(ctx),
            shutdown_tx,
            ble,
            ble_calls,
            protocol,
            storage,
            delivery,
            delivery_calls,
            renderer,
            clock,
            observer,
            local: tokio::task::LocalSet::new(),
        }
    }

    /// Consume the context (e.g. to spawn `run(ctx)`). Fakes stay reachable
    /// through the shared `Arc` handles.
    pub fn take_ctx(&mut self) -> CycleContext {
        self.ctx.take().expect("context already taken")
    }

    /// Detach the shutdown sender so a test can drop it, simulating a
    /// vanished sender task (the receiver then sees a closed channel).
    pub fn detach_shutdown_sender(&mut self) -> watch::Sender<bool> {
        std::mem::replace(&mut self.shutdown_tx, watch::channel(false).0)
    }

    /// Spawn the runner on the harness's `LocalSet` (the runner future is
    /// `!Send` because the BLE session trait is `?Send`).
    pub fn spawn_runner(
        &self,
        ctx: CycleContext,
    ) -> tokio::task::JoinHandle<Result<RunSummary, RunError>> {
        self.local.spawn_local(run(ctx))
    }

    /// Drive `f` on the harness's `LocalSet`, interleaving the spawned runner.
    pub async fn drive<F, T>(&self, f: F) -> T
    where
        F: Future<Output = T>,
    {
        self.local.run_until(f).await
    }

    pub async fn samples_persisted(&self) -> u64 {
        self.storage.lock().unwrap().enqueues()
    }

    pub async fn spool_depth(&self) -> usize {
        self.storage.lock().unwrap().spool_depth()
    }
}

pub fn base_config() -> ServiceConfig {
    ServiceConfig::default()
}
