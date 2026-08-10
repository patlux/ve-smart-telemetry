//! Derived measurement helpers for [`Sample`].
//!
//! These compute values from other measurements of the same sample and tag
//! them [`Quality::Derived`]. They return `None` whenever an input is missing
//! or the result is not physically plausible, so consumers can safely fall
//! back to the optional native fields.

use crate::bounds::{LOAD_POWER_W, PV_CURRENT_A};
use crate::measurement::Measurement;
use crate::quality::Quality;
use crate::sample::Sample;

impl Sample {
    /// Derive PV current as `PV power / PV voltage`, tagged
    /// [`Quality::Derived`].
    ///
    /// Returns `None` unless both inputs are present and valid and the
    /// result is physically plausible:
    ///
    /// - PV voltage must be strictly positive (division by zero is
    ///   meaningless).
    /// - The result must lie within [`crate::bounds::PV_CURRENT_A`].
    ///
    /// Input qualities are not re-checked; the result is always `Derived`.
    /// When a native PV current is available, prefer it over this helper.
    pub fn derived_pv_current(&self) -> Option<Measurement> {
        let power = self.pv_power_watts?;
        let voltage = self.pv_voltage_volts?;
        if voltage.value() <= 0.0 {
            return None;
        }
        let current = power.value() / voltage.value();
        if !PV_CURRENT_A.contains(current) {
            return None;
        }
        Measurement::new(current, Quality::Derived).ok()
    }

    /// Derive load power as `load current × battery voltage`, tagged
    /// [`Quality::Derived`].
    ///
    /// The MPPT load output is fed from the battery, so battery voltage is
    /// the load supply voltage. Returns `None` unless both inputs are
    /// present and valid and the result lies within
    /// [`crate::bounds::LOAD_POWER_W`].
    ///
    /// Input qualities are not re-checked; the result is always `Derived`.
    /// When a native load power is available, prefer it over this helper.
    pub fn derived_load_power(&self) -> Option<Measurement> {
        let current = self.load_current_amperes?;
        let voltage = self.battery_voltage_volts?;
        let power = current.value() * voltage.value();
        if !LOAD_POWER_W.contains(power) {
            return None;
        }
        Measurement::new(power, Quality::Derived).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::DeviceId;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn t0() -> SystemTime {
        UNIX_EPOCH
    }

    fn device() -> DeviceId {
        DeviceId::new("solar-charger").unwrap()
    }

    #[test]
    fn derived_pv_current_valid() {
        let s = Sample::builder(device(), t0())
            .pv_voltage_volts(34.2, Quality::ConfirmedNative)
            .unwrap()
            .pv_power_watts(96.0, Quality::ConfirmedNative)
            .unwrap()
            .build();
        let derived = s.derived_pv_current().expect("derived current");
        assert_eq!(derived.quality(), Quality::Derived);
        let expected = 96.0 / 34.2;
        assert!(
            (derived.value() - expected).abs() < 1e-12,
            "got {}",
            derived.value()
        );
    }

    #[test]
    fn derived_pv_current_missing_or_zero_voltage() {
        // Missing power.
        let s1 = Sample::builder(device(), t0())
            .pv_voltage_volts(34.2, Quality::ConfirmedNative)
            .unwrap()
            .build();
        assert!(s1.derived_pv_current().is_none());
        // Missing voltage.
        let s2 = Sample::builder(device(), t0())
            .pv_power_watts(96.0, Quality::ConfirmedNative)
            .unwrap()
            .build();
        assert!(s2.derived_pv_current().is_none());
        // Zero voltage (division by zero).
        let s3 = Sample::builder(device(), t0())
            .pv_voltage_volts(0.0, Quality::ConfirmedNative)
            .unwrap()
            .pv_power_watts(96.0, Quality::ConfirmedNative)
            .unwrap()
            .build();
        assert!(s3.derived_pv_current().is_none());
    }

    #[test]
    fn derived_pv_current_rejects_impossible_result() {
        // 30 000 W over 0.1 V => 300 000 A, far outside PV_CURRENT_A.
        let s = Sample::builder(device(), t0())
            .pv_voltage_volts(0.1, Quality::ConfirmedNative)
            .unwrap()
            .pv_power_watts(30_000.0, Quality::ConfirmedNative)
            .unwrap()
            .build();
        assert!(s.derived_pv_current().is_none());
    }

    #[test]
    fn derived_load_power_valid() {
        let s = Sample::builder(device(), t0())
            .load_current_amperes(3.5, Quality::Candidate)
            .unwrap()
            .battery_voltage_volts(12.6, Quality::Candidate)
            .unwrap()
            .build();
        let derived = s.derived_load_power().expect("derived load power");
        assert_eq!(derived.quality(), Quality::Derived);
        assert!(
            (derived.value() - 3.5 * 12.6).abs() < 1e-12,
            "got {}",
            derived.value()
        );
    }

    #[test]
    fn derived_load_power_missing_or_impossible() {
        // Missing current.
        let s1 = Sample::builder(device(), t0())
            .battery_voltage_volts(12.6, Quality::Candidate)
            .unwrap()
            .build();
        assert!(s1.derived_load_power().is_none());
        // Missing voltage.
        let s2 = Sample::builder(device(), t0())
            .load_current_amperes(3.5, Quality::Candidate)
            .unwrap()
            .build();
        assert!(s2.derived_load_power().is_none());
        // 100 A * 70 V = 7000 W > LOAD_POWER_W max 6000.
        let s3 = Sample::builder(device(), t0())
            .load_current_amperes(100.0, Quality::Candidate)
            .unwrap()
            .battery_voltage_volts(70.0, Quality::Candidate)
            .unwrap()
            .build();
        assert!(s3.derived_load_power().is_none());
    }
}
