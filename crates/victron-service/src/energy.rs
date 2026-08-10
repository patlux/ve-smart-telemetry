//! Durable energy fallback integration.
//!
//! Canonical energy priority:
//! 1. confirmed Victron lifetime yield (native counter) — used **only** when
//!    the sample reports it with [`Quality::ConfirmedNative`];
//! 2. durable local trapezoidal integration of **confirmed** PV power.
//!
//! Candidate/derived values never become canonical energy: a `Candidate`
//! yield is ignored (the fallback runs), and only `ConfirmedNative` PV power
//! is stored as the integration anchor or integrated.
//!
//! Integration is skipped (never silently bridged) when either power sample
//! is missing/not confirmed, time moves backward (or a duplicate timestamp
//! arrives), the gap exceeds the configured maximum, or no durable previous
//! sample exists (fresh start).
//!
//! All intervals are measured from [`Sample::observed_at`] — the device's
//! observation time — never from a later orchestration clock reading, so
//! delayed processing cannot manufacture energy across a long wall-clock gap.

use std::time::Duration;

use victron_domain::{Quality, Sample};

use crate::ports::storage::EnergyState;

/// How the resolved cumulative energy was produced this cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnergyKind {
    /// Confirmed native Victron lifetime yield was used directly.
    Native,
    /// Local trapezoidal integration extended the durable accumulator.
    Integrated,
    /// No durable previous sample yet; accumulator initialized (no energy
    /// attributed to this cycle).
    Started,
    /// Integration skipped (invalid/unconfirmed power, clock regression,
    /// duplicate timestamp or gap too large).
    Skipped,
}

/// Result of applying the energy policy to one sample.
#[derive(Debug, Clone, PartialEq)]
pub struct EnergyOutcome {
    pub kind: EnergyKind,
    /// Resolved cumulative kWh for metrics (native or integrated).
    pub total_kwh: f64,
    /// Set when a gap was skipped because it exceeded the maximum; drives the
    /// cumulative `energy_gap_skipped_seconds` health counter.
    pub skipped_gap_seconds: Option<Duration>,
    /// Persistable energy state for the next cycle.
    pub next_state: EnergyState,
}

/// Trapezoidal integration policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnergyPolicy {
    pub maximum_gap: Duration,
}

impl EnergyPolicy {
    /// Apply the policy to `sample`, given the durable previous state `prev`.
    ///
    /// The integration interval is `sample.observed_at() - prev.last_sample_at`
    /// (device observation times), never the processing wall clock.
    pub fn apply(&self, prev: Option<EnergyState>, sample: &Sample) -> EnergyOutcome {
        // Preferred: native counter, but only when the quality is
        // ConfirmedNative. A candidate yield is not canonical energy: fall
        // through to the durable fallback. Keep the anchor fresh so the
        // fallback can take over seamlessly if the native counter disappears.
        if let Some(native) = sample.yield_total_kwh() {
            if native.quality() == Quality::ConfirmedNative {
                return EnergyOutcome {
                    kind: EnergyKind::Native,
                    total_kwh: native.value(),
                    skipped_gap_seconds: None,
                    next_state: updated_state(prev.as_ref(), sample),
                };
            }
        }

        let observed_at = sample.observed_at();
        let confirmed_power = confirmed_pv_power(sample);

        let Some(prev) = prev else {
            // No durable previous sample: start the accumulator, attribute no
            // energy to this cycle (plan: "process has no durable previous
            // sample" -> skip integration).
            return EnergyOutcome {
                kind: EnergyKind::Started,
                total_kwh: 0.0,
                skipped_gap_seconds: None,
                next_state: updated_state(None, sample),
            };
        };

        // Backward clock or duplicate timestamp: never integrate and never
        // move the anchor. The acquisition commit is the idempotency backstop
        // (same observed_at is a no-op there too).
        let Some(prev_at) = prev.last_sample_at else {
            // Inconsistent durable state without an anchor: treat this sample
            // as a fresh anchor, attribute no energy.
            return EnergyOutcome {
                kind: EnergyKind::Started,
                total_kwh: prev.total_kwh,
                skipped_gap_seconds: None,
                next_state: updated_state(Some(&prev), sample),
            };
        };
        if observed_at <= prev_at {
            return EnergyOutcome {
                kind: EnergyKind::Skipped,
                total_kwh: prev.total_kwh,
                skipped_gap_seconds: None,
                next_state: prev,
            };
        }

        let (Some(prev_power), Some(cur_power)) = (prev.last_power_watts, confirmed_power) else {
            // Either power sample missing or not confirmed: skip, keep the
            // anchor (an invalid sample must never become the anchor).
            return EnergyOutcome {
                kind: EnergyKind::Skipped,
                total_kwh: prev.total_kwh,
                skipped_gap_seconds: None,
                next_state: prev,
            };
        };

        let gap = observed_at
            .duration_since(prev_at)
            .unwrap_or(Duration::ZERO);
        if gap > self.maximum_gap {
            // Reset the anchor to this sample without adding energy: the
            // outage is reported, never silently bridged.
            return EnergyOutcome {
                kind: EnergyKind::Skipped,
                total_kwh: prev.total_kwh,
                skipped_gap_seconds: Some(gap),
                next_state: updated_state(Some(&prev), sample),
            };
        }

        // trapezoid: (prev + cur) / 2 * seconds / 3_600_000 -> kWh
        let added = (prev_power + cur_power) / 2.0 * gap.as_secs_f64() / 3_600_000.0;
        let mut next = updated_state(Some(&prev), sample);
        next.total_kwh = prev.total_kwh + added;
        EnergyOutcome {
            kind: EnergyKind::Integrated,
            total_kwh: next.total_kwh,
            skipped_gap_seconds: None,
            next_state: next,
        }
    }
}

/// The anchor always moves to the sample's observation time; only confirmed
/// PV power is ever stored as the integration anchor.
fn updated_state(prev: Option<&EnergyState>, sample: &Sample) -> EnergyState {
    EnergyState {
        total_kwh: prev.map(|p| p.total_kwh).unwrap_or(0.0),
        last_power_watts: confirmed_pv_power(sample),
        last_sample_at: Some(sample.observed_at()),
    }
}

/// Confirmed PV power only: candidate/derived values are never canonical
/// energy and never become the integration anchor.
fn confirmed_pv_power(sample: &Sample) -> Option<f64> {
    sample
        .pv_power_watts()
        .filter(|m| m.quality() == Quality::ConfirmedNative)
        .map(|m| m.value())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;
    use victron_domain::{ChargerState, DeviceId};

    fn secs(ts: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(ts)
    }

    fn device() -> DeviceId {
        DeviceId::new("solar-charger").unwrap()
    }

    fn sample_with(
        observed_at: SystemTime,
        pv_power: Option<(f64, Quality)>,
        yield_kwh: Option<(f64, Quality)>,
    ) -> Sample {
        let mut b = Sample::builder(device(), observed_at);
        if let Some((v, q)) = pv_power {
            b = b.pv_power_watts(v, q).unwrap();
        }
        if let Some((v, q)) = yield_kwh {
            b = b.yield_total_kwh(v, q).unwrap();
        }
        b.build()
    }

    fn prev_at(total: f64, power: Option<f64>, at: SystemTime) -> EnergyState {
        EnergyState {
            total_kwh: total,
            last_power_watts: power,
            last_sample_at: Some(at),
        }
    }

    #[test]
    fn native_counter_wins_over_integration() {
        let p = EnergyPolicy {
            maximum_gap: Duration::from_secs(300),
        };
        let prev = Some(prev_at(1.0, Some(200.0), secs(0)));
        let out = p.apply(
            prev,
            &sample_with(
                secs(15),
                Some((210.0, Quality::ConfirmedNative)),
                Some((42.5, Quality::ConfirmedNative)),
            ),
        );
        assert_eq!(out.kind, EnergyKind::Native);
        assert_eq!(out.total_kwh, 42.5);
        assert_eq!(out.skipped_gap_seconds, None);
        // Fallback continuity state still updated from the sample.
        assert_eq!(out.next_state.last_power_watts, Some(210.0));
        assert_eq!(out.next_state.last_sample_at, Some(secs(15)));
    }

    #[test]
    fn candidate_yield_is_not_canonical_energy() {
        // A candidate yield must NOT be used as canonical energy: the durable
        // fallback integrates confirmed PV power instead.
        let p = EnergyPolicy {
            maximum_gap: Duration::from_secs(300),
        };
        let prev = Some(prev_at(0.0, Some(200.0), secs(0)));
        let out = p.apply(
            prev,
            &sample_with(
                secs(15),
                Some((210.0, Quality::ConfirmedNative)),
                Some((42.5, Quality::Candidate)),
            ),
        );
        assert_eq!(out.kind, EnergyKind::Integrated);
        let expected = 205.0 * 15.0 / 3_600_000.0;
        assert!((out.total_kwh - expected).abs() < 1e-12);
    }

    #[test]
    fn candidate_pv_power_is_not_integrated() {
        // Candidate PV power must not become canonical energy: the anchor is
        // not moved and no energy is added.
        let p = EnergyPolicy {
            maximum_gap: Duration::from_secs(300),
        };
        let prev = prev_at(1.0, Some(200.0), secs(0));
        let out = p.apply(
            Some(prev.clone()),
            &sample_with(secs(15), Some((210.0, Quality::Candidate)), None),
        );
        assert_eq!(out.kind, EnergyKind::Skipped);
        assert_eq!(out.total_kwh, 1.0);
        assert_eq!(out.next_state, prev, "anchor must not move");
    }

    #[test]
    fn integrates_trapezoid_between_regular_samples() {
        let p = EnergyPolicy {
            maximum_gap: Duration::from_secs(300),
        };
        let prev = Some(prev_at(0.0, Some(200.0), secs(0)));
        // (200 + 210) / 2 * 15s / 3_600_000 = 205 * 15 / 3_600_000 = 0.00085416...
        let out = p.apply(
            prev,
            &sample_with(secs(15), Some((210.0, Quality::ConfirmedNative)), None),
        );
        assert_eq!(out.kind, EnergyKind::Integrated);
        assert!((out.total_kwh - 205.0 * 15.0 / 3_600_000.0).abs() < 1e-12);
        assert_eq!(out.skipped_gap_seconds, None);
    }

    #[test]
    fn delayed_processing_uses_observed_at_not_wall_clock() {
        // The sample was observed at t=15 but is only processed at wall clock
        // t=600. The integration interval must be the 15 s between observed
        // timestamps, NOT the 600 s processing delay (which would exceed the
        // 300 s maximum gap and skip).
        let p = EnergyPolicy {
            maximum_gap: Duration::from_secs(300),
        };
        let prev = Some(prev_at(0.0, Some(200.0), secs(0)));
        let out = p.apply(
            prev,
            &sample_with(secs(15), Some((210.0, Quality::ConfirmedNative)), None),
        );
        assert_eq!(out.kind, EnergyKind::Integrated);
        let expected = 205.0 * 15.0 / 3_600_000.0;
        assert!((out.total_kwh - expected).abs() < 1e-12);
    }

    #[test]
    fn starts_accumulator_when_no_previous_sample() {
        let p = EnergyPolicy {
            maximum_gap: Duration::from_secs(300),
        };
        let out = p.apply(
            None,
            &sample_with(secs(0), Some((150.0, Quality::ConfirmedNative)), None),
        );
        assert_eq!(out.kind, EnergyKind::Started);
        assert_eq!(out.total_kwh, 0.0);
        // Next cycle can integrate from this state.
        let out2 = p.apply(
            Some(out.next_state),
            &sample_with(secs(15), Some((160.0, Quality::ConfirmedNative)), None),
        );
        assert_eq!(out2.kind, EnergyKind::Integrated);
    }

    #[test]
    fn skips_when_power_missing_or_unconfirmed() {
        let p = EnergyPolicy {
            maximum_gap: Duration::from_secs(300),
        };
        let prev = prev_at(1.0, Some(200.0), secs(0));
        let out = p.apply(Some(prev.clone()), &sample_with(secs(15), None, None));
        assert_eq!(out.kind, EnergyKind::Skipped);
        assert_eq!(out.total_kwh, 1.0);
        assert_eq!(out.next_state, prev, "anchor must not move");
    }

    #[test]
    fn skips_when_clock_moves_backward() {
        let p = EnergyPolicy {
            maximum_gap: Duration::from_secs(300),
        };
        let prev = prev_at(1.0, Some(200.0), secs(100));
        let out = p.apply(
            Some(prev.clone()),
            &sample_with(secs(90), Some((210.0, Quality::ConfirmedNative)), None),
        );
        assert_eq!(out.kind, EnergyKind::Skipped);
        assert_eq!(out.total_kwh, 1.0);
        assert_eq!(out.next_state, prev, "anchor must not move");
    }

    #[test]
    fn duplicate_timestamp_is_skipped_without_moving_anchor() {
        let p = EnergyPolicy {
            maximum_gap: Duration::from_secs(300),
        };
        let prev = prev_at(1.0, Some(200.0), secs(100));
        // Same observed_at as the anchor: a reprocessed sample.
        let out = p.apply(
            Some(prev.clone()),
            &sample_with(secs(100), Some((210.0, Quality::ConfirmedNative)), None),
        );
        assert_eq!(out.kind, EnergyKind::Skipped);
        assert_eq!(out.total_kwh, 1.0);
        assert_eq!(out.next_state, prev, "anchor must not move");
    }

    #[test]
    fn skips_long_gaps_and_reports_them() {
        let p = EnergyPolicy {
            maximum_gap: Duration::from_secs(300),
        };
        let prev = Some(prev_at(1.0, Some(200.0), secs(0)));
        let out = p.apply(
            prev,
            &sample_with(secs(600), Some((210.0, Quality::ConfirmedNative)), None),
        );
        assert_eq!(out.kind, EnergyKind::Skipped);
        assert_eq!(out.total_kwh, 1.0);
        assert_eq!(out.skipped_gap_seconds, Some(Duration::from_secs(600)));
        // The anchor resets to the new sample so the next cycle can integrate.
        assert_eq!(out.next_state.last_sample_at, Some(secs(600)));
        assert_eq!(out.next_state.last_power_watts, Some(210.0));
    }

    #[test]
    fn restart_keeps_accumulator_without_double_counting() {
        let p = EnergyPolicy {
            maximum_gap: Duration::from_secs(300),
        };
        // Simulate a persisted state surviving a process restart.
        let persisted = prev_at(0.5, Some(100.0), secs(0));
        let out = p.apply(
            Some(persisted),
            &sample_with(secs(10), Some((110.0, Quality::ConfirmedNative)), None),
        );
        let expected = 0.5 + (100.0 + 110.0) / 2.0 * 10.0 / 3_600_000.0;
        assert_eq!(out.kind, EnergyKind::Integrated);
        assert!((out.total_kwh - expected).abs() < 1e-12);
    }

    #[test]
    fn charger_state_and_other_fields_are_preserved() {
        // The policy must not lose domain fields: a sample carrying charger
        // state and other measurements still integrates on confirmed power.
        let p = EnergyPolicy {
            maximum_gap: Duration::from_secs(300),
        };
        let prev = Some(prev_at(0.0, Some(200.0), secs(0)));
        let sample = Sample::builder(device(), secs(15))
            .pv_power_watts(210.0, Quality::ConfirmedNative)
            .unwrap()
            .pv_voltage_volts(48.1, Quality::ConfirmedNative)
            .unwrap()
            .charger_state(ChargerState::Bulk)
            .build();
        let out = p.apply(prev, &sample);
        assert_eq!(out.kind, EnergyKind::Integrated);
        assert_eq!(sample.charger_state(), Some(ChargerState::Bulk));
        assert_eq!(sample.pv_voltage_volts().unwrap().value(), 48.1);
    }
}
