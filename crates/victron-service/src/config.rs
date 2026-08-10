//! Runtime configuration for the service layer.
//!
//! This is a plain validated struct (no serde). The binaries own TOML parsing
//! and map their (validated) config onto this type. There is deliberately no
//! PIN/PUK/secret field anywhere in this crate: pairing material belongs to
//! BlueZ and never to application configuration.

use std::time::Duration;

use victron_domain::DeviceId;

use crate::model::DeviceIdentity;
use crate::scheduler::ExponentialBackoff;

/// Tunables for the collector orchestration.
///
/// `default()` mirrors the values from the deployment plan
/// (`analysis/rust-multicrate-collector-plan.md`).
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceConfig {
    /// Stable device label used for metrics and logs.
    pub device_name: String,
    /// VE.Smart instance to subscribe to; `0` is reserved for the keep-alive
    /// pseudo-instance and is rejected by validation.
    pub instance: u16,
    /// Poll interval while the active interval policy applies.
    pub active_interval: Duration,
    /// Poll interval while the idle interval policy applies.
    pub idle_interval: Duration,
    /// Confirmed PV power (watts) at or above this value counts as solar
    /// activity and selects the active poll cadence.
    pub solar_active_threshold_watts: f64,
    /// Protocol response deadline handed to the BLE session for one request.
    pub response_timeout: Duration,
    /// Hard outer deadline for every BLE phase (belt and suspenders).
    pub phase_timeout: Duration,
    /// Longest allowed gap between samples for local energy integration.
    pub maximum_energy_gap: Duration,
    /// How long a claimed spool batch stays owned before it may be reclaimed.
    pub spool_claim_ttl: Duration,
    /// Maximum delivery attempts per batch before it is dropped (bounded).
    pub spool_max_attempts: u32,
    /// Base backoff duration after a failed cycle.
    pub backoff_base: Duration,
    /// Backoff growth factor (>= 1; 1 means linear).
    pub backoff_factor: u32,
    /// Upper bound for any single backoff delay.
    pub backoff_cap: Duration,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            device_name: "solar-charger".into(),
            instance: 3,
            active_interval: Duration::from_secs(15),
            idle_interval: Duration::from_secs(60),
            solar_active_threshold_watts: 5.0,
            response_timeout: Duration::from_secs(8),
            phase_timeout: Duration::from_secs(12),
            maximum_energy_gap: Duration::from_secs(300),
            spool_claim_ttl: Duration::from_secs(120),
            spool_max_attempts: 5,
            backoff_base: Duration::from_secs(5),
            backoff_factor: 2,
            backoff_cap: Duration::from_secs(300),
        }
    }
}

impl ServiceConfig {
    pub fn device(&self) -> DeviceIdentity {
        // `validate()` guarantees the name is a valid domain `DeviceId`
        // (its charset is a subset of the domain's).
        let device = DeviceId::new(&self.device_name)
            .expect("device_name is validated by ServiceConfig::validate");
        DeviceIdentity::new(device, self.instance)
    }

    /// Deterministic backoff policy derived from this config.
    pub fn backoff(&self) -> ExponentialBackoff {
        ExponentialBackoff {
            base: self.backoff_base,
            factor: self.backoff_factor,
            cap: self.backoff_cap,
        }
    }

    /// Validate all bounds. Returns the first problem found.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.device_name.trim().is_empty() {
            return Err(ConfigError::EmptyDeviceName);
        }
        if self
            .device_name
            .chars()
            .any(|c| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        {
            return Err(ConfigError::InvalidDeviceName(self.device_name.clone()));
        }
        if self.instance == 0 {
            return Err(ConfigError::InstanceMustBeNonZero);
        }
        if self.active_interval.is_zero() {
            return Err(ConfigError::IntervalZero("active_interval"));
        }
        if self.idle_interval.is_zero() {
            return Err(ConfigError::IntervalZero("idle_interval"));
        }
        if !self.solar_active_threshold_watts.is_finite() || self.solar_active_threshold_watts < 0.0
        {
            return Err(ConfigError::SolarThresholdInvalid(
                self.solar_active_threshold_watts,
            ));
        }
        if self.response_timeout.is_zero() {
            return Err(ConfigError::TimeoutZero("response_timeout"));
        }
        if self.phase_timeout.is_zero() {
            return Err(ConfigError::TimeoutZero("phase_timeout"));
        }
        if self.phase_timeout < self.response_timeout {
            return Err(ConfigError::PhaseTimeoutBelowResponseTimeout);
        }
        if self.maximum_energy_gap < Duration::from_secs(1) {
            return Err(ConfigError::MaximumEnergyGapTooSmall);
        }
        if self.spool_claim_ttl.is_zero() {
            return Err(ConfigError::SpoolClaimTtlZero);
        }
        if self.spool_max_attempts == 0 {
            return Err(ConfigError::SpoolMaxAttemptsZero);
        }
        if self.backoff_base.is_zero() {
            return Err(ConfigError::BackoffBaseZero);
        }
        if self.backoff_factor == 0 {
            return Err(ConfigError::BackoffFactorZero);
        }
        if self.backoff_cap < self.backoff_base {
            return Err(ConfigError::BackoffCapBelowBase);
        }
        Ok(())
    }
}

/// Configuration validation failure.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConfigError {
    #[error("device name must not be empty")]
    EmptyDeviceName,
    #[error("device name {0:?} contains characters outside [a-zA-Z0-9_-]")]
    InvalidDeviceName(String),
    #[error("instance 0 is the keep-alive pseudo-instance and is not supported")]
    InstanceMustBeNonZero,
    #[error("{0} must be greater than zero")]
    IntervalZero(&'static str),
    #[error("solar_active_threshold_watts must be finite and >= 0, got {0}")]
    SolarThresholdInvalid(f64),
    #[error("{0} must be greater than zero")]
    TimeoutZero(&'static str),
    #[error("phase_timeout must be >= response_timeout")]
    PhaseTimeoutBelowResponseTimeout,
    #[error("maximum_energy_gap must be at least 1 second")]
    MaximumEnergyGapTooSmall,
    #[error("spool_claim_ttl must be greater than zero")]
    SpoolClaimTtlZero,
    #[error("spool_max_attempts must be at least 1")]
    SpoolMaxAttemptsZero,
    #[error("backoff_base must be greater than zero")]
    BackoffBaseZero,
    #[error("backoff_factor must be at least 1")]
    BackoffFactorZero,
    #[error("backoff_cap must be >= backoff_base")]
    BackoffCapBelowBase,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::field_reassign_with_default)]
    use super::*;
    use crate::scheduler::BackoffPolicy;

    #[test]
    fn defaults_are_valid() {
        assert_eq!(ServiceConfig::default().validate(), Ok(()));
    }

    #[test]
    fn rejects_empty_and_invalid_names() {
        let mut c = ServiceConfig::default();
        c.device_name = "  ".into();
        assert!(matches!(c.validate(), Err(ConfigError::EmptyDeviceName)));
        c.device_name = "solar-charger mit space".into();
        assert!(matches!(
            c.validate(),
            Err(ConfigError::InvalidDeviceName(_))
        ));
        c.device_name = "solar-charger".into();
        assert_eq!(c.validate(), Ok(()));
    }

    #[test]
    fn rejects_instance_zero() {
        let mut c = ServiceConfig::default();
        c.instance = 0;
        assert!(matches!(
            c.validate(),
            Err(ConfigError::InstanceMustBeNonZero)
        ));
    }

    #[test]
    fn rejects_zero_intervals_and_timeouts() {
        let mut c = ServiceConfig::default();
        c.active_interval = Duration::ZERO;
        assert!(matches!(
            c.validate(),
            Err(ConfigError::IntervalZero("active_interval"))
        ));
        c = ServiceConfig::default();
        c.response_timeout = Duration::ZERO;
        assert!(matches!(
            c.validate(),
            Err(ConfigError::TimeoutZero("response_timeout"))
        ));
    }

    #[test]
    fn rejects_invalid_solar_threshold() {
        let mut c = ServiceConfig::default();
        c.solar_active_threshold_watts = f64::NAN;
        assert!(matches!(
            c.validate(),
            Err(ConfigError::SolarThresholdInvalid(_))
        ));
        c = ServiceConfig::default();
        c.solar_active_threshold_watts = -1.0;
        assert!(matches!(
            c.validate(),
            Err(ConfigError::SolarThresholdInvalid(_))
        ));
        c = ServiceConfig::default();
        c.solar_active_threshold_watts = 0.0;
        assert_eq!(c.validate(), Ok(()));
    }

    #[test]
    fn rejects_phase_timeout_below_response_timeout() {
        let mut c = ServiceConfig::default();
        c.phase_timeout = Duration::from_secs(4);
        c.response_timeout = Duration::from_secs(8);
        assert!(matches!(
            c.validate(),
            Err(ConfigError::PhaseTimeoutBelowResponseTimeout)
        ));
    }

    #[test]
    fn rejects_bad_backoff_and_spool_bounds() {
        let mut c = ServiceConfig::default();
        c.backoff_factor = 0;
        assert!(matches!(c.validate(), Err(ConfigError::BackoffFactorZero)));
        c = ServiceConfig::default();
        c.backoff_cap = Duration::from_millis(1);
        assert!(matches!(
            c.validate(),
            Err(ConfigError::BackoffCapBelowBase)
        ));
        c = ServiceConfig::default();
        c.spool_max_attempts = 0;
        assert!(matches!(
            c.validate(),
            Err(ConfigError::SpoolMaxAttemptsZero)
        ));
        c = ServiceConfig::default();
        c.spool_claim_ttl = Duration::ZERO;
        assert!(matches!(c.validate(), Err(ConfigError::SpoolClaimTtlZero)));
    }

    #[test]
    fn backoff_policy_reflects_config() {
        let c = ServiceConfig::default();
        let b = c.backoff();
        assert_eq!(b.delay(0), Duration::ZERO);
        assert_eq!(b.delay(1), c.backoff_base);
        assert_eq!(b.delay(2), c.backoff_base.saturating_mul(2));
    }
}
