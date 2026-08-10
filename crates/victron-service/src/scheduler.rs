//! Poll scheduling: active/idle interval selection and deterministic backoff.
//!
//! No randomness anywhere. Backoff is a pure exponential
//! `min(cap, base * factor^(attempts-1))` with overflow-safe arithmetic; if
//! jitter is ever wanted it must be derived from the clock, not from a PRNG.
//!
//! Active/idle cadence is driven by the **last successfully committed
//! sample's actual solar activity** (confirmed PV power), not by a UTC hour
//! window: [`SolarActivityPolicy`] switches to the short cadence while the
//! device reports confirmed PV power at or above the configured threshold and
//! to the long cadence otherwise. With no committed sample yet the first
//! cycle uses the active cadence.

use std::time::{Duration, SystemTime};

use victron_domain::{Quality, Sample};

/// Which poll cadence applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntervalKind {
    /// Short cadence (e.g. 15 s while solar power is active).
    Active,
    /// Long cadence (e.g. 60 s while idle).
    Idle,
}

/// Solar activity of the last successfully committed sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolarActivity {
    /// Confirmed PV power at or above the active threshold.
    Active,
    /// Confirmed PV power below the threshold, or no confirmed power.
    Idle,
}

/// Inputs an [`IntervalPolicy`] may consult.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntervalContext {
    pub now: SystemTime,
    pub last_success: Option<SystemTime>,
    /// Solar activity of the last successfully committed sample (`None` when
    /// no sample has been committed yet).
    pub last_solar: Option<SolarActivity>,
    pub consecutive_failures: u32,
}

/// Decides whether the next poll uses the active or idle interval.
pub trait IntervalPolicy: Send + Sync {
    fn kind(&self, ctx: &IntervalContext) -> IntervalKind;
}

/// Always the same cadence (useful for tests and for deployments without a
/// solar-activity policy).
#[derive(Debug, Clone, Copy)]
pub struct ConstantIntervalPolicy {
    pub kind: IntervalKind,
}

impl ConstantIntervalPolicy {
    pub fn active() -> Self {
        Self {
            kind: IntervalKind::Active,
        }
    }

    pub fn idle() -> Self {
        Self {
            kind: IntervalKind::Idle,
        }
    }
}

impl IntervalPolicy for ConstantIntervalPolicy {
    fn kind(&self, _ctx: &IntervalContext) -> IntervalKind {
        self.kind
    }
}

/// Active/idle cadence from the last committed sample's confirmed solar
/// activity.
///
/// Policy: `Active` while the last committed sample reports confirmed PV
/// power at or above `active_threshold_watts`; `Idle` otherwise. With no
/// committed sample yet (`last_solar == None`) the first cycle uses the
/// active cadence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolarActivityPolicy {
    /// Confirmed PV power (watts) at or above this value counts as active.
    pub active_threshold_watts: f64,
}

impl SolarActivityPolicy {
    pub fn new(active_threshold_watts: f64) -> Self {
        debug_assert!(active_threshold_watts.is_finite() && active_threshold_watts >= 0.0);
        Self {
            active_threshold_watts,
        }
    }
}

impl IntervalPolicy for SolarActivityPolicy {
    fn kind(&self, ctx: &IntervalContext) -> IntervalKind {
        match ctx.last_solar {
            None => IntervalKind::Active,
            Some(SolarActivity::Active) => IntervalKind::Active,
            Some(SolarActivity::Idle) => IntervalKind::Idle,
        }
    }
}

/// Classify a committed sample's solar activity from its **confirmed** PV
/// power. Candidate/derived power is not evidence of solar activity.
pub fn solar_activity(sample: &Sample, threshold_watts: f64) -> SolarActivity {
    match sample.pv_power_watts() {
        Some(m) if m.quality() == Quality::ConfirmedNative && m.value() >= threshold_watts => {
            SolarActivity::Active
        }
        _ => SolarActivity::Idle,
    }
}

/// Deterministic exponential backoff.
///
/// `delay(0)` is zero; `delay(n)` for `n >= 1` is
/// `min(cap, base * factor^(n-1))`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExponentialBackoff {
    pub base: Duration,
    pub factor: u32,
    pub cap: Duration,
}

impl BackoffPolicy for ExponentialBackoff {
    fn delay(&self, consecutive_failures: u32) -> Duration {
        if consecutive_failures == 0 {
            return Duration::ZERO;
        }
        // Guard the exponent so huge failure counts cannot overflow.
        let exponent = consecutive_failures.saturating_sub(1).min(32);
        let multiplier = self.factor.saturating_pow(exponent);
        self.base.saturating_mul(multiplier).min(self.cap)
    }
}

/// Retry/backoff policy: how long to wait before the next attempt.
pub trait BackoffPolicy: Send + Sync {
    /// Delay to apply after `consecutive_failures` failed cycles.
    /// `factor` 1 keeps the delay at `base` for every attempt.
    fn delay(&self, consecutive_failures: u32) -> Duration;
}

#[cfg(test)]
mod tests {
    use super::*;
    use victron_domain::DeviceId;

    fn device() -> DeviceId {
        DeviceId::new("solar-charger").unwrap()
    }

    fn sample_with_power(power: Option<(f64, Quality)>) -> Sample {
        let mut b = Sample::builder(device(), SystemTime::UNIX_EPOCH);
        if let Some((v, q)) = power {
            b = b.pv_power_watts(v, q).unwrap();
        }
        b.build()
    }

    #[test]
    fn backoff_is_deterministic_and_capped() {
        let b = ExponentialBackoff {
            base: Duration::from_secs(5),
            factor: 2,
            cap: Duration::from_secs(300),
        };
        assert_eq!(b.delay(0), Duration::ZERO);
        assert_eq!(b.delay(1), Duration::from_secs(5));
        assert_eq!(b.delay(2), Duration::from_secs(10));
        assert_eq!(b.delay(3), Duration::from_secs(20));
        // 5 * 2^6 = 320 -> capped at 300.
        assert_eq!(b.delay(7), Duration::from_secs(300));
        // Huge counts stay capped, no overflow.
        assert_eq!(b.delay(u32::MAX), Duration::from_secs(300));
        // Repeatable.
        assert_eq!(b.delay(3), b.delay(3));
    }

    #[test]
    fn factor_one_keeps_base_delay() {
        let b = ExponentialBackoff {
            base: Duration::from_secs(1),
            factor: 1,
            cap: Duration::from_secs(10),
        };
        assert_eq!(b.delay(1), Duration::from_secs(1));
        assert_eq!(b.delay(5), Duration::from_secs(1));
        assert_eq!(b.delay(99), Duration::from_secs(1));
    }

    #[test]
    fn constant_policies_report_kind() {
        let ctx = IntervalContext {
            now: SystemTime::UNIX_EPOCH,
            last_success: None,
            last_solar: None,
            consecutive_failures: 0,
        };
        assert_eq!(
            ConstantIntervalPolicy::active().kind(&ctx),
            IntervalKind::Active
        );
        assert_eq!(
            ConstantIntervalPolicy::idle().kind(&IntervalContext {
                consecutive_failures: 3,
                ..ctx
            }),
            IntervalKind::Idle
        );
    }

    #[test]
    fn solar_policy_switches_on_committed_activity() {
        let p = SolarActivityPolicy::new(5.0);
        let ctx = |solar: Option<SolarActivity>| IntervalContext {
            now: SystemTime::UNIX_EPOCH,
            last_success: None,
            last_solar: solar,
            consecutive_failures: 0,
        };
        // No committed sample yet: first-cycle active cadence.
        assert_eq!(p.kind(&ctx(None)), IntervalKind::Active);
        assert_eq!(
            p.kind(&ctx(Some(SolarActivity::Active))),
            IntervalKind::Active
        );
        assert_eq!(p.kind(&ctx(Some(SolarActivity::Idle))), IntervalKind::Idle);
    }

    #[test]
    fn solar_activity_requires_confirmed_power_at_or_above_threshold() {
        let threshold = 5.0;
        assert_eq!(
            solar_activity(
                &sample_with_power(Some((150.0, Quality::ConfirmedNative))),
                threshold
            ),
            SolarActivity::Active
        );
        assert_eq!(
            solar_activity(
                &sample_with_power(Some((5.0, Quality::ConfirmedNative))),
                threshold
            ),
            SolarActivity::Active,
            "threshold is inclusive"
        );
        assert_eq!(
            solar_activity(
                &sample_with_power(Some((4.9, Quality::ConfirmedNative))),
                threshold
            ),
            SolarActivity::Idle
        );
        // Candidate power is not evidence of solar activity.
        assert_eq!(
            solar_activity(
                &sample_with_power(Some((150.0, Quality::Candidate))),
                threshold
            ),
            SolarActivity::Idle
        );
        // No power at all: idle.
        assert_eq!(
            solar_activity(&sample_with_power(None), threshold),
            SolarActivity::Idle
        );
    }
}
