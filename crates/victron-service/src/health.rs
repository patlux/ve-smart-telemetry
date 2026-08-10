//! Health counters exposed to the metrics renderer.
//!
//! The runner owns one [`HealthCounters`]; the renderer receives a
//! [`HealthSnapshot`] per cycle. Counter names are service-internal; the app
//! maps them onto the Prometheus contract
//! (`victron_ble_connect_failures_total`, `victron_protocol_errors_total`,
//! ...) when wiring the sibling `victron-metrics` crate.
//!
//! The energy-gap counter is **cumulative seconds** skipped by local energy
//! integration (gaps that were never silently bridged), not an event count:
//! a 600 s gap adds 600, a 10 s gap adds 10.

use std::time::{Duration, SystemTime};

/// Point-in-time view of the health counters.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HealthSnapshot {
    pub cycles_total: u64,
    pub cycles_succeeded: u64,
    pub ble_discover_failures_total: u64,
    pub ble_connect_failures_total: u64,
    pub ble_session_failures_total: u64,
    pub protocol_errors_total: u64,
    pub samples_dropped_total: u64,
    pub deliveries_succeeded_total: u64,
    pub deliveries_failed_total: u64,
    pub spool_dropped_total: u64,
    /// Cumulative seconds skipped by local energy integration (gaps).
    pub energy_gap_skipped_seconds: u64,
    pub consecutive_failures: u32,
    pub last_success: Option<SystemTime>,
}

/// Mutable counters. Single-threaded by design (Tokio current-thread); no
/// atomics needed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HealthCounters {
    snapshot: HealthSnapshot,
}

impl HealthCounters {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&self) -> HealthSnapshot {
        self.snapshot.clone()
    }

    pub fn last_success(&self) -> Option<SystemTime> {
        self.snapshot.last_success
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.snapshot.consecutive_failures
    }

    /// Cumulative skipped energy-gap seconds.
    pub fn energy_gap_skipped_seconds(&self) -> u64 {
        self.snapshot.energy_gap_skipped_seconds
    }

    /// Record a successful acquisition timestamp. Monotonic: a duplicate or
    /// backward sample never moves the recorded success backwards.
    pub fn set_last_success(&mut self, at: SystemTime) {
        let current = self.snapshot.last_success;
        if current.is_none_or(|c| at > c) {
            self.snapshot.last_success = Some(at);
        }
    }

    pub fn record_cycle(&mut self, ok: bool) {
        self.snapshot.cycles_total += 1;
        if ok {
            self.snapshot.cycles_succeeded += 1;
            self.snapshot.consecutive_failures = 0;
        } else {
            self.snapshot.consecutive_failures =
                self.snapshot.consecutive_failures.saturating_add(1);
        }
    }

    pub fn record_ble_discover_failure(&mut self) {
        self.snapshot.ble_discover_failures_total += 1;
    }

    pub fn record_ble_connect_failure(&mut self) {
        self.snapshot.ble_connect_failures_total += 1;
    }

    pub fn record_ble_session_failure(&mut self) {
        self.snapshot.ble_session_failures_total += 1;
    }

    pub fn record_protocol_error(&mut self) {
        self.snapshot.protocol_errors_total += 1;
    }

    pub fn record_sample_dropped(&mut self) {
        self.snapshot.samples_dropped_total += 1;
    }

    pub fn record_delivery(&mut self, ok: bool) {
        if ok {
            self.snapshot.deliveries_succeeded_total += 1;
        } else {
            self.snapshot.deliveries_failed_total += 1;
        }
    }

    pub fn record_spool_dropped(&mut self) {
        self.snapshot.spool_dropped_total += 1;
    }

    /// Add a skipped energy gap (in seconds) to the cumulative counter.
    pub fn record_energy_gap(&mut self, gap: Duration) {
        self.snapshot.energy_gap_skipped_seconds = self
            .snapshot
            .energy_gap_skipped_seconds
            .saturating_add(gap.as_secs());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_counting_resets_failures_on_success() {
        let mut h = HealthCounters::new();
        h.record_cycle(false);
        h.record_cycle(false);
        assert_eq!(h.consecutive_failures(), 2);
        assert_eq!(h.snapshot().cycles_total, 2);
        h.record_cycle(true);
        assert_eq!(h.consecutive_failures(), 0);
        assert_eq!(h.snapshot().cycles_succeeded, 1);
    }

    #[test]
    fn counters_increment_independently() {
        let mut h = HealthCounters::new();
        h.record_ble_connect_failure();
        h.record_protocol_error();
        h.record_delivery(false);
        h.record_delivery(true);
        let s = h.snapshot();
        assert_eq!(s.ble_connect_failures_total, 1);
        assert_eq!(s.protocol_errors_total, 1);
        assert_eq!(s.deliveries_failed_total, 1);
        assert_eq!(s.deliveries_succeeded_total, 1);
    }

    #[test]
    fn energy_gap_counter_accumulates_seconds_not_events() {
        let mut h = HealthCounters::new();
        h.record_energy_gap(Duration::from_secs(600));
        h.record_energy_gap(Duration::from_secs(10));
        assert_eq!(h.energy_gap_skipped_seconds(), 610);
        assert_eq!(h.snapshot().energy_gap_skipped_seconds, 610);
    }

    #[test]
    fn last_success_is_monotonic() {
        let mut h = HealthCounters::new();
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let t1 = t0 + Duration::from_secs(10);
        h.set_last_success(t1);
        // A duplicate/backward sample must not move the recorded success back.
        h.set_last_success(t0);
        assert_eq!(h.last_success(), Some(t1));
        h.set_last_success(t1 + Duration::from_secs(1));
        assert_eq!(h.last_success(), Some(t1 + Duration::from_secs(1)));
    }
}
