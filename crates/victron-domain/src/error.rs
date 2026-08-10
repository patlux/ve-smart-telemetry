//! Typed error type for the domain crate.

use std::error::Error;
use std::fmt;

/// Errors produced by domain validation.
///
/// All variants carry enough context to be reported in logs without exposing
/// secrets, raw wire payloads, or unbounded labels.
#[derive(Debug, Clone, PartialEq)]
pub enum DomainError {
    /// A [`crate::DeviceId`] input failed its charset or length validation.
    InvalidDeviceId {
        /// The rejected input.
        value: String,
        /// Human-readable validation reason.
        reason: &'static str,
    },
    /// A non-finite value (`NaN`, `+inf`, `-inf`) was rejected.
    NonFinite {
        /// What the value belonged to, e.g. `"pv_voltage_volts"`.
        context: &'static str,
        /// The non-finite value.
        value: f64,
    },
    /// A finite value outside the conservative physical range was rejected.
    OutOfRange {
        /// Field name, e.g. `"pv_voltage_volts"`.
        field: &'static str,
        /// The rejected value.
        value: f64,
        /// Inclusive lower bound of the documented range.
        min: f64,
        /// Inclusive upper bound of the documented range.
        max: f64,
    },
}

impl fmt::Display for DomainError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DomainError::InvalidDeviceId { value, reason } => {
                write!(f, "invalid device id {:?}: {}", value, reason)
            }
            DomainError::NonFinite { context, value } => {
                write!(f, "{} value must be finite, got {}", context, value)
            }
            DomainError::OutOfRange {
                field,
                value,
                min,
                max,
            } => {
                write!(
                    f,
                    "{} value {} outside conservative physical range [{}, {}]",
                    field, value, min, max
                )
            }
        }
    }
}

impl Error for DomainError {}

#[cfg(test)]
mod tests {
    use super::DomainError;

    #[test]
    fn display_messages_are_useful() {
        let err = DomainError::InvalidDeviceId {
            value: "".to_string(),
            reason: "must not be empty",
        };
        assert!(err.to_string().contains("invalid device id"));

        let err = DomainError::NonFinite {
            context: "pv_power_watts",
            value: f64::NAN,
        };
        assert!(err.to_string().contains("pv_power_watts"));
        assert!(err.to_string().contains("finite"));

        let err = DomainError::OutOfRange {
            field: "pv_voltage_volts",
            value: 300.0,
            min: 0.0,
            max: 250.0,
        };
        assert!(err.to_string().contains("pv_voltage_volts"));
        assert!(err.to_string().contains("[0, 250]"));
    }

    #[test]
    fn error_is_std_error() {
        let err = DomainError::NonFinite {
            context: "measurement",
            value: f64::INFINITY,
        };
        let _: &dyn std::error::Error = &err;
    }
}
