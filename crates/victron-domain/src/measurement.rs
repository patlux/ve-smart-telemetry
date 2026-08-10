//! A validated measurement value plus its provenance quality.
//!
//! A [`Measurement`] only ever holds a finite `f64`; construction rejects
//! `NaN` and infinities. Physical plausibility is enforced per field by the
//! [`crate::SampleBuilder`] using the documented ranges in [`crate::bounds`].

use crate::error::DomainError;
use crate::quality::Quality;
use std::fmt;

/// A finite `f64` value together with its [`Quality`].
///
/// This is the smallest unit exposed to consumers: it always has a value
/// ([`Measurement::value`]) and a quality ([`Measurement::quality`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measurement {
    value: f64,
    quality: Quality,
}

impl Measurement {
    /// Build a measurement from a finite value.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NonFinite`] when `value` is `NaN`, `+inf`, or
    /// `-inf`. Physical range checks are intentionally not applied here:
    /// ranges are field-specific and live in [`crate::bounds`].
    pub fn new(value: f64, quality: Quality) -> Result<Self, DomainError> {
        if !value.is_finite() {
            return Err(DomainError::NonFinite {
                context: "measurement",
                value,
            });
        }
        Ok(Self { value, quality })
    }

    /// The numeric value.
    pub fn value(self) -> f64 {
        self.value
    }

    /// The provenance/trust quality.
    pub fn quality(self) -> Quality {
        self.quality
    }

    /// Whether the value is finite.
    ///
    /// Always `true` by construction; provided for explicit checks in code
    /// that receives measurements across an API boundary.
    pub fn is_finite(self) -> bool {
        self.value.is_finite()
    }
}

impl fmt::Display for Measurement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}({})", self.value, self.quality_label())
    }
}

impl Measurement {
    fn quality_label(&self) -> &'static str {
        match self.quality {
            Quality::ConfirmedNative => "confirmed",
            Quality::Candidate => "candidate",
            Quality::Derived => "derived",
            Quality::LocallyIntegrated => "integrated",
        }
    }
}

/// Identifies one named optional measurement field of a [`crate::Sample`].
///
/// Use with [`crate::Sample::iter`] to enumerate the present measurements of
/// a sample without touching every accessor. The names are semantic and
/// wire-independent; metric/DB names are a separate concern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SampleField {
    /// PV voltage in volts.
    PvVoltageVolts,
    /// PV current in amperes.
    PvCurrentAmperes,
    /// PV power in watts.
    PvPowerWatts,
    /// Battery voltage in volts.
    BatteryVoltageVolts,
    /// Battery current in amperes (positive = charging).
    BatteryCurrentAmperes,
    /// Load output current in amperes.
    LoadCurrentAmperes,
    /// Load output power in watts.
    LoadPowerWatts,
    /// Native lifetime yield counter in kWh.
    YieldTotalKwh,
    /// Yield since local midnight in kWh.
    YieldTodayKwh,
    /// BLE RSSI in dBm.
    BleRssiDbm,
}

impl SampleField {
    /// Canonical short name, e.g. `"pv_voltage_volts"`.
    pub const fn name(self) -> &'static str {
        match self {
            SampleField::PvVoltageVolts => "pv_voltage_volts",
            SampleField::PvCurrentAmperes => "pv_current_amperes",
            SampleField::PvPowerWatts => "pv_power_watts",
            SampleField::BatteryVoltageVolts => "battery_voltage_volts",
            SampleField::BatteryCurrentAmperes => "battery_current_amperes",
            SampleField::LoadCurrentAmperes => "load_current_amperes",
            SampleField::LoadPowerWatts => "load_power_watts",
            SampleField::YieldTotalKwh => "yield_total_kwh",
            SampleField::YieldTodayKwh => "yield_today_kwh",
            SampleField::BleRssiDbm => "ble_rssi_dbm",
        }
    }
}

impl fmt::Display for SampleField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DomainError;

    #[test]
    fn new_accepts_finite() {
        let m = Measurement::new(12.5, Quality::Candidate).unwrap();
        assert_eq!(m.value(), 12.5);
        assert_eq!(m.quality(), Quality::Candidate);
        assert!(m.is_finite());
    }

    #[test]
    fn new_rejects_non_finite() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            match Measurement::new(bad, Quality::Derived) {
                Err(DomainError::NonFinite { context, value }) => {
                    assert_eq!(context, "measurement");
                    assert!(value.is_nan() || value.is_infinite());
                    assert!(!value.is_finite());
                }
                other => panic!("expected NonFinite for {bad}, got {other:?}"),
            }
        }
    }

    #[test]
    fn measurement_is_copy() {
        let m = Measurement::new(1.0, Quality::ConfirmedNative).unwrap();
        let m2 = m; // copy
        assert_eq!(m, m2);
        assert_eq!(m.value(), m2.value());
    }

    #[test]
    fn measurement_display() {
        let m = Measurement::new(96.0, Quality::ConfirmedNative).unwrap();
        assert_eq!(m.to_string(), "96(confirmed)");
    }

    #[test]
    fn sample_field_names() {
        assert_eq!(SampleField::PvVoltageVolts.name(), "pv_voltage_volts");
        assert_eq!(SampleField::YieldTotalKwh.name(), "yield_total_kwh");
        assert_eq!(SampleField::BleRssiDbm.name(), "ble_rssi_dbm");
        assert_eq!(SampleField::LoadPowerWatts.to_string(), "load_power_watts");
    }

    #[test]
    fn sample_field_is_copy() {
        let f = SampleField::PvPowerWatts;
        let f2 = f;
        assert_eq!(f, f2);
    }
}
