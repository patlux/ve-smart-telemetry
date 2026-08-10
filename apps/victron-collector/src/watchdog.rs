//! Progress-aware systemd readiness and watchdog notifications.
//!
//! The heartbeat task runs independently from BLE work so it can observe a
//! stalled acquisition. It only feeds systemd while the most recent phase
//! transition is inside that phase's explicit progress budget. A blocked
//! current-thread runtime also stops this task, so both orchestration stalls
//! and full runtime deadlocks lead to a watchdog restart.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sd_notify::NotifyState;
use tokio::sync::watch;
use victron_service::{CyclePhase, PhaseObserver};

const MIN_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const MAX_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy)]
struct Progress {
    phase: CyclePhase,
    entered_at: Instant,
    budget: Duration,
}

/// Records phase transitions with per-phase maximum progress budgets.
#[derive(Debug)]
pub struct ProgressObserver {
    progress: Mutex<Progress>,
    transition: Mutex<(CyclePhase, Instant)>,
    phase_timeout: Duration,
    maximum_idle: Duration,
    delivery_timeout: Duration,
}

impl ProgressObserver {
    pub fn new(
        phase_timeout: Duration,
        maximum_idle: Duration,
        delivery_timeout: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            progress: Mutex::new(Progress {
                phase: CyclePhase::Idle,
                entered_at: Instant::now(),
                budget: maximum_idle,
            }),
            transition: Mutex::new((CyclePhase::Idle, Instant::now())),
            phase_timeout,
            maximum_idle,
            delivery_timeout,
        })
    }

    fn budget_for(&self, phase: CyclePhase) -> Duration {
        match phase {
            CyclePhase::Idle | CyclePhase::Backoff => self.maximum_idle,
            CyclePhase::Discovering
            | CyclePhase::Connecting
            | CyclePhase::Negotiating
            | CyclePhase::Subscribing
            | CyclePhase::Requesting
            | CyclePhase::Disconnecting => self.phase_timeout,
            CyclePhase::Collecting | CyclePhase::Persisting => self.phase_timeout,
            CyclePhase::Delivering => self.delivery_timeout,
            CyclePhase::ShuttingDown => Duration::ZERO,
        }
    }

    fn healthy_progress(&self, now: Instant) -> Option<CyclePhase> {
        let progress = *self.progress.lock().unwrap();
        (progress.budget > Duration::ZERO
            && now.duration_since(progress.entered_at) <= progress.budget)
            .then_some(progress.phase)
    }
}

impl PhaseObserver for ProgressObserver {
    fn on_phase(&self, phase: CyclePhase) {
        let now = Instant::now();
        let mut transition = self.transition.lock().unwrap();
        let (previous_phase, entered_at) = *transition;
        tracing::debug!(
            previous_phase = previous_phase.as_str(),
            phase = phase.as_str(),
            previous_elapsed_ms = now.duration_since(entered_at).as_millis() as u64,
            "collector phase transition"
        );
        *transition = (phase, now);
        drop(transition);
        self.on_progress(phase);
    }

    fn on_progress(&self, phase: CyclePhase) {
        *self.progress.lock().unwrap() = Progress {
            phase,
            entered_at: Instant::now(),
            budget: self.budget_for(phase),
        };
    }
}

/// Send READY after initialization and feed systemd only while progress is
/// inside the current phase's budget. Notification failure is fail-closed:
/// startup fails rather than running a watchdog-enabled unit that can never
/// report liveness.
pub fn start(
    observer: Arc<ProgressObserver>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), std::io::Error> {
    let watchdog_timeout = sd_notify::watchdog_enabled();
    let status = match watchdog_timeout {
        Some(timeout) => format!(
            "collector initialized; progress watchdog enabled ({}s)",
            timeout.as_secs()
        ),
        None => "collector initialized; systemd watchdog disabled".to_owned(),
    };
    sd_notify::notify(&[NotifyState::Ready, NotifyState::Status(&status)])?;

    let Some(timeout) = watchdog_timeout else {
        return Ok(());
    };
    let interval = heartbeat_interval(timeout);
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Consume the immediate first tick; READY already proves startup.
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    match observer.healthy_progress(Instant::now()) {
                        Some(phase) => {
                            if let Err(error) = sd_notify::notify(&[
                                NotifyState::Watchdog,
                                NotifyState::Status(phase.as_str()),
                            ]) {
                                tracing::error!(%error, "systemd watchdog notification failed");
                                break;
                            }
                        }
                        None => {
                            tracing::error!("collector progress deadline exceeded; triggering systemd watchdog");
                            if let Err(error) = sd_notify::notify(&[
                                NotifyState::WatchdogTrigger,
                                NotifyState::Status("collector progress deadline exceeded"),
                            ]) {
                                // Even if the explicit trigger fails, stopping
                                // heartbeats still lets systemd's timer recover
                                // a stuck service.
                                tracing::error!(%error, "systemd watchdog trigger failed");
                            }
                            break;
                        }
                    }
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
    });
    Ok(())
}

pub fn stopping() {
    if let Err(error) = sd_notify::notify(&[
        NotifyState::Stopping,
        NotifyState::Status("collector shutting down"),
    ]) {
        tracing::warn!(%error, "systemd stopping notification failed");
    }
}

fn heartbeat_interval(timeout: Duration) -> Duration {
    timeout
        .checked_div(3)
        .unwrap_or(MIN_HEARTBEAT_INTERVAL)
        .clamp(MIN_HEARTBEAT_INTERVAL, MAX_HEARTBEAT_INTERVAL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_is_one_third_with_safe_bounds() {
        assert_eq!(
            heartbeat_interval(Duration::from_millis(1)),
            Duration::from_secs(1)
        );
        assert_eq!(
            heartbeat_interval(Duration::from_secs(90)),
            Duration::from_secs(30)
        );
        assert_eq!(
            heartbeat_interval(Duration::from_secs(300)),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn progress_expires_after_phase_budget() {
        let observer = ProgressObserver::new(
            Duration::from_secs(8),
            Duration::from_secs(365),
            Duration::from_secs(10),
        );
        observer.on_phase(CyclePhase::Persisting);
        let entered = observer.progress.lock().unwrap().entered_at;
        assert_eq!(
            observer.healthy_progress(entered + Duration::from_secs(8)),
            Some(CyclePhase::Persisting)
        );
        assert_eq!(
            observer.healthy_progress(entered + Duration::from_secs(9)),
            None
        );
    }

    #[test]
    fn shutdown_never_feeds_watchdog() {
        let observer = ProgressObserver::new(
            Duration::from_secs(120),
            Duration::from_secs(365),
            Duration::from_secs(10),
        );
        observer.on_phase(CyclePhase::ShuttingDown);
        assert_eq!(observer.healthy_progress(Instant::now()), None);
    }

    #[test]
    fn logging_transition_tracking_does_not_change_progress_budget() {
        let observer = ProgressObserver::new(
            Duration::from_secs(120),
            Duration::from_secs(365),
            Duration::from_secs(10),
        );
        observer.on_phase(CyclePhase::Requesting);
        let progress = *observer.progress.lock().unwrap();
        assert_eq!(progress.phase, CyclePhase::Requesting);
        assert_eq!(progress.budget, Duration::from_secs(120));
        assert_eq!(
            observer.transition.lock().unwrap().0,
            CyclePhase::Requesting
        );
    }
}
