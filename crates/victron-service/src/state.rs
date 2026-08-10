//! Bounded cycle state machine.
//!
//! The collector walks the canonical cycle
//! `Idle -> Discovering -> Connecting -> Negotiating -> Subscribing ->
//! Requesting -> Collecting -> Persisting -> Delivering -> Disconnecting ->
//! Idle`, with `Backoff` and `ShuttingDown` reachable from every working
//! state. Illegal transitions are rejected by [`StateMachine`] and fail the
//! test suite if they ever occur at runtime (they indicate a programming
//! error, not a device condition).

use std::sync::Mutex;

/// All observable phases of the collector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CyclePhase {
    Idle,
    Discovering,
    Connecting,
    Negotiating,
    Subscribing,
    Requesting,
    Collecting,
    Persisting,
    Delivering,
    Disconnecting,
    Backoff,
    ShuttingDown,
}

impl CyclePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            CyclePhase::Idle => "idle",
            CyclePhase::Discovering => "discovering",
            CyclePhase::Connecting => "connecting",
            CyclePhase::Negotiating => "negotiating",
            CyclePhase::Subscribing => "subscribing",
            CyclePhase::Requesting => "requesting",
            CyclePhase::Collecting => "collecting",
            CyclePhase::Persisting => "persisting",
            CyclePhase::Delivering => "delivering",
            CyclePhase::Disconnecting => "disconnecting",
            CyclePhase::Backoff => "backoff",
            CyclePhase::ShuttingDown => "shutting_down",
        }
    }

    /// Whether a direct transition `self -> next` is legal.
    pub fn can_transition_to(self, next: CyclePhase) -> bool {
        use CyclePhase::*;
        match self {
            Idle => matches!(next, Discovering | ShuttingDown),
            Discovering => matches!(next, Connecting | Disconnecting | Backoff | ShuttingDown),
            Connecting => matches!(next, Negotiating | Disconnecting | Backoff | ShuttingDown),
            Negotiating => matches!(next, Subscribing | Disconnecting | Backoff | ShuttingDown),
            Subscribing => matches!(next, Requesting | Disconnecting | Backoff | ShuttingDown),
            Requesting => matches!(next, Collecting | Disconnecting | Backoff | ShuttingDown),
            Collecting => matches!(next, Persisting | Disconnecting | Backoff | ShuttingDown),
            Persisting => matches!(next, Delivering | Disconnecting | Backoff | ShuttingDown),
            Delivering => matches!(next, Disconnecting | Backoff | ShuttingDown),
            Disconnecting => matches!(next, Idle | Backoff | ShuttingDown),
            Backoff => matches!(next, Idle | Discovering | ShuttingDown),
            ShuttingDown => false,
        }
    }

    /// True for the terminal phase; no transitions leave it.
    pub fn is_terminal(self) -> bool {
        self == CyclePhase::ShuttingDown
    }
}

/// An illegal state transition (programming error).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("illegal state transition {from:?} -> {to:?}")]
pub struct StateTransitionError {
    pub from: CyclePhase,
    pub to: CyclePhase,
}

/// Guards the phase machine.
#[derive(Debug, Clone, Copy)]
pub struct StateMachine {
    current: CyclePhase,
    transitions: u64,
}

impl StateMachine {
    pub fn new() -> Self {
        Self {
            current: CyclePhase::Idle,
            transitions: 0,
        }
    }

    pub fn current(&self) -> CyclePhase {
        self.current
    }

    /// Total number of successful transitions so far.
    pub fn transitions(&self) -> u64 {
        self.transitions
    }

    /// Attempt `next`; rejects illegal transitions without changing state.
    pub fn transition(&mut self, next: CyclePhase) -> Result<(), StateTransitionError> {
        if self.current == next || !self.current.can_transition_to(next) {
            return Err(StateTransitionError {
                from: self.current,
                to: next,
            });
        }
        self.current = next;
        self.transitions += 1;
        Ok(())
    }
}

impl Default for StateMachine {
    fn default() -> Self {
        Self::new()
    }
}

/// Observes phase entries and optional progress within a phase.
pub trait PhaseObserver: Send + Sync {
    fn on_phase(&self, phase: CyclePhase);

    /// Report forward progress without changing the state machine. The
    /// default is intentionally a no-op so diagnostic phase recordings keep
    /// representing transitions only.
    fn on_progress(&self, _phase: CyclePhase) {}
}

/// Observer that does nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoopObserver;

impl PhaseObserver for NoopObserver {
    fn on_phase(&self, _phase: CyclePhase) {}
}

/// Observer that records every phase entry in order.
#[derive(Debug, Default)]
pub struct RecordingObserver {
    seen: Mutex<Vec<CyclePhase>>,
}

impl RecordingObserver {
    pub fn new() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
        }
    }

    pub fn phases(&self) -> Vec<CyclePhase> {
        self.seen.lock().unwrap().clone()
    }

    pub fn contains(&self, phase: CyclePhase) -> bool {
        self.phases().contains(&phase)
    }

    pub fn count(&self, phase: CyclePhase) -> usize {
        self.phases().iter().filter(|p| **p == phase).count()
    }
}

impl PhaseObserver for RecordingObserver {
    fn on_phase(&self, phase: CyclePhase) {
        self.seen.lock().unwrap().push(phase);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_cycle_is_legal() {
        let mut m = StateMachine::new();
        for phase in [
            CyclePhase::Discovering,
            CyclePhase::Connecting,
            CyclePhase::Negotiating,
            CyclePhase::Subscribing,
            CyclePhase::Requesting,
            CyclePhase::Collecting,
            CyclePhase::Persisting,
            CyclePhase::Delivering,
            CyclePhase::Disconnecting,
            CyclePhase::Idle,
        ] {
            m.transition(phase).unwrap();
        }
        assert_eq!(m.current(), CyclePhase::Idle);
        assert_eq!(m.transitions(), 10);
    }

    #[test]
    fn every_working_phase_can_backoff_and_shutdown() {
        use CyclePhase::*;
        let working = [
            Discovering,
            Connecting,
            Negotiating,
            Subscribing,
            Requesting,
            Collecting,
            Persisting,
            Delivering,
            Disconnecting,
        ];
        for from in working {
            assert!(from.can_transition_to(Backoff), "{from:?} -> Backoff");
            assert!(
                from.can_transition_to(ShuttingDown),
                "{from:?} -> ShuttingDown"
            );
        }
    }

    #[test]
    fn rejects_illegal_transitions() {
        let mut m = StateMachine::new();
        // Skip ahead is illegal.
        assert_eq!(
            m.transition(CyclePhase::Requesting),
            Err(StateTransitionError {
                from: CyclePhase::Idle,
                to: CyclePhase::Requesting
            })
        );
        assert_eq!(m.current(), CyclePhase::Idle);
        // Self-transition is illegal.
        assert_eq!(
            m.transition(CyclePhase::Idle),
            Err(StateTransitionError {
                from: CyclePhase::Idle,
                to: CyclePhase::Idle
            })
        );
        // ShuttingDown is terminal.
        m.transition(CyclePhase::ShuttingDown).unwrap();
        assert!(m.transition(CyclePhase::Discovering).is_err());
    }

    #[test]
    fn backoff_can_return_to_idle_or_retry_directly() {
        assert!(CyclePhase::Backoff.can_transition_to(CyclePhase::Idle));
        assert!(CyclePhase::Backoff.can_transition_to(CyclePhase::Discovering));
    }

    #[test]
    fn graceful_teardown_from_any_acquisition_phase() {
        // Idle/Backoff go straight to ShuttingDown without a disconnect; only
        // working acquisition phases need a teardown path.
        use CyclePhase::*;
        let phases = [
            Discovering,
            Connecting,
            Negotiating,
            Subscribing,
            Requesting,
            Collecting,
            Persisting,
            Delivering,
        ];
        for from in phases {
            assert!(
                from.can_transition_to(Disconnecting),
                "{from:?} -> Disconnecting"
            );
        }
    }

    #[test]
    fn observer_records_in_order() {
        let o = RecordingObserver::new();
        o.on_phase(CyclePhase::Discovering);
        o.on_phase(CyclePhase::Connecting);
        o.on_phase(CyclePhase::Discovering);
        assert_eq!(
            o.phases(),
            vec![
                CyclePhase::Discovering,
                CyclePhase::Connecting,
                CyclePhase::Discovering
            ]
        );
        assert_eq!(o.count(CyclePhase::Discovering), 2);
    }
}
