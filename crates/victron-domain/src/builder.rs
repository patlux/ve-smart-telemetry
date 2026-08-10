//! Fallible builder for a [`Sample`].

use crate::bounds::{
    Range, BATTERY_CURRENT_A, BATTERY_VOLTAGE_V, BLE_RSSI_DBM, LOAD_CURRENT_A, LOAD_POWER_W,
    PV_CURRENT_A, PV_POWER_W, PV_VOLTAGE_V, YIELD_TODAY_KWH, YIELD_TOTAL_KWH,
};
use crate::device::DeviceId;
use crate::error::DomainError;
use crate::measurement::Measurement;
use crate::quality::Quality;
use crate::sample::Sample;
use crate::state::{ChargerState, ConnectionHealth, LoadState};
use std::time::SystemTime;

/// Fallible builder for a [`Sample`].
///
/// Every measurement setter validates its value against the documented
/// conservative range in [`crate::bounds`] and rejects non-finite values.
/// State and health setters are infallible because the enums are bounded by
/// construction. [`SampleBuilder::build`] is infallible: all fallible
/// validation happens at set time.
#[derive(Debug, Clone)]
pub struct SampleBuilder {
    device: DeviceId,
    observed_at: SystemTime,
    pv_voltage_volts: Option<Measurement>,
    pv_current_amperes: Option<Measurement>,
    pv_power_watts: Option<Measurement>,
    battery_voltage_volts: Option<Measurement>,
    battery_current_amperes: Option<Measurement>,
    load_current_amperes: Option<Measurement>,
    load_power_watts: Option<Measurement>,
    yield_total_kwh: Option<Measurement>,
    yield_today_kwh: Option<Measurement>,
    ble_rssi_dbm: Option<Measurement>,
    charger_state: Option<ChargerState>,
    load_state: Option<LoadState>,
    connection_health: Option<ConnectionHealth>,
}

impl SampleBuilder {
    /// Create a builder for a sample of `device` observed at `observed_at`.
    pub fn new(device: DeviceId, observed_at: SystemTime) -> Self {
        Self {
            device,
            observed_at,
            pv_voltage_volts: None,
            pv_current_amperes: None,
            pv_power_watts: None,
            battery_voltage_volts: None,
            battery_current_amperes: None,
            load_current_amperes: None,
            load_power_watts: None,
            yield_total_kwh: None,
            yield_today_kwh: None,
            ble_rssi_dbm: None,
            charger_state: None,
            load_state: None,
            connection_health: None,
        }
    }

    /// Validate finiteness and physical range for one measurement field.
    fn check(field: &'static str, value: f64, range: Range) -> Result<(), DomainError> {
        if !value.is_finite() {
            return Err(DomainError::NonFinite {
                context: field,
                value,
            });
        }
        if !range.contains(value) {
            return Err(DomainError::OutOfRange {
                field,
                value,
                min: range.min(),
                max: range.max(),
            });
        }
        Ok(())
    }

    /// Set PV voltage in volts.
    ///
    /// # Errors
    ///
    /// [`DomainError::NonFinite`] or [`DomainError::OutOfRange`] when the
    /// value is outside [`crate::bounds::PV_VOLTAGE_V`].
    pub fn pv_voltage_volts(mut self, value: f64, quality: Quality) -> Result<Self, DomainError> {
        Self::check("pv_voltage_volts", value, PV_VOLTAGE_V)?;
        self.pv_voltage_volts = Some(Measurement::new(value, quality)?);
        Ok(self)
    }

    /// Set PV current in amperes.
    ///
    /// # Errors
    ///
    /// [`DomainError::NonFinite`] or [`DomainError::OutOfRange`] when the
    /// value is outside [`crate::bounds::PV_CURRENT_A`].
    pub fn pv_current_amperes(mut self, value: f64, quality: Quality) -> Result<Self, DomainError> {
        Self::check("pv_current_amperes", value, PV_CURRENT_A)?;
        self.pv_current_amperes = Some(Measurement::new(value, quality)?);
        Ok(self)
    }

    /// Set PV power in watts.
    ///
    /// # Errors
    ///
    /// [`DomainError::NonFinite`] or [`DomainError::OutOfRange`] when the
    /// value is outside [`crate::bounds::PV_POWER_W`].
    pub fn pv_power_watts(mut self, value: f64, quality: Quality) -> Result<Self, DomainError> {
        Self::check("pv_power_watts", value, PV_POWER_W)?;
        self.pv_power_watts = Some(Measurement::new(value, quality)?);
        Ok(self)
    }

    /// Set battery voltage in volts.
    ///
    /// # Errors
    ///
    /// [`DomainError::NonFinite`] or [`DomainError::OutOfRange`] when the
    /// value is outside [`crate::bounds::BATTERY_VOLTAGE_V`].
    pub fn battery_voltage_volts(
        mut self,
        value: f64,
        quality: Quality,
    ) -> Result<Self, DomainError> {
        Self::check("battery_voltage_volts", value, BATTERY_VOLTAGE_V)?;
        self.battery_voltage_volts = Some(Measurement::new(value, quality)?);
        Ok(self)
    }

    /// Set battery current in amperes (positive = charging).
    ///
    /// # Errors
    ///
    /// [`DomainError::NonFinite`] or [`DomainError::OutOfRange`] when the
    /// value is outside [`crate::bounds::BATTERY_CURRENT_A`].
    pub fn battery_current_amperes(
        mut self,
        value: f64,
        quality: Quality,
    ) -> Result<Self, DomainError> {
        Self::check("battery_current_amperes", value, BATTERY_CURRENT_A)?;
        self.battery_current_amperes = Some(Measurement::new(value, quality)?);
        Ok(self)
    }

    /// Set load output current in amperes.
    ///
    /// # Errors
    ///
    /// [`DomainError::NonFinite`] or [`DomainError::OutOfRange`] when the
    /// value is outside [`crate::bounds::LOAD_CURRENT_A`].
    pub fn load_current_amperes(
        mut self,
        value: f64,
        quality: Quality,
    ) -> Result<Self, DomainError> {
        Self::check("load_current_amperes", value, LOAD_CURRENT_A)?;
        self.load_current_amperes = Some(Measurement::new(value, quality)?);
        Ok(self)
    }

    /// Set load output power in watts.
    ///
    /// # Errors
    ///
    /// [`DomainError::NonFinite`] or [`DomainError::OutOfRange`] when the
    /// value is outside [`crate::bounds::LOAD_POWER_W`].
    pub fn load_power_watts(mut self, value: f64, quality: Quality) -> Result<Self, DomainError> {
        Self::check("load_power_watts", value, LOAD_POWER_W)?;
        self.load_power_watts = Some(Measurement::new(value, quality)?);
        Ok(self)
    }

    /// Set the native lifetime yield counter in kWh.
    ///
    /// # Errors
    ///
    /// [`DomainError::NonFinite`] or [`DomainError::OutOfRange`] when the
    /// value is outside [`crate::bounds::YIELD_TOTAL_KWH`].
    pub fn yield_total_kwh(mut self, value: f64, quality: Quality) -> Result<Self, DomainError> {
        Self::check("yield_total_kwh", value, YIELD_TOTAL_KWH)?;
        self.yield_total_kwh = Some(Measurement::new(value, quality)?);
        Ok(self)
    }

    /// Set the yield since local midnight in kWh.
    ///
    /// # Errors
    ///
    /// [`DomainError::NonFinite`] or [`DomainError::OutOfRange`] when the
    /// value is outside [`crate::bounds::YIELD_TODAY_KWH`].
    pub fn yield_today_kwh(mut self, value: f64, quality: Quality) -> Result<Self, DomainError> {
        Self::check("yield_today_kwh", value, YIELD_TODAY_KWH)?;
        self.yield_today_kwh = Some(Measurement::new(value, quality)?);
        Ok(self)
    }

    /// Set BLE RSSI in dBm.
    ///
    /// # Errors
    ///
    /// [`DomainError::NonFinite`] or [`DomainError::OutOfRange`] when the
    /// value is outside [`crate::bounds::BLE_RSSI_DBM`].
    pub fn ble_rssi_dbm(mut self, value: f64, quality: Quality) -> Result<Self, DomainError> {
        Self::check("ble_rssi_dbm", value, BLE_RSSI_DBM)?;
        self.ble_rssi_dbm = Some(Measurement::new(value, quality)?);
        Ok(self)
    }

    /// Set the charger state.
    pub fn charger_state(mut self, state: ChargerState) -> Self {
        self.charger_state = Some(state);
        self
    }

    /// Set the load output state.
    pub fn load_state(mut self, state: LoadState) -> Self {
        self.load_state = Some(state);
        self
    }

    /// Set the BLE connection health.
    pub fn connection_health(mut self, health: ConnectionHealth) -> Self {
        self.connection_health = Some(health);
        self
    }

    /// Consume the builder and produce the validated [`Sample`].
    ///
    /// Infallible: every fallible check ran at set time.
    pub fn build(self) -> Sample {
        Sample {
            device: self.device,
            observed_at: self.observed_at,
            pv_voltage_volts: self.pv_voltage_volts,
            pv_current_amperes: self.pv_current_amperes,
            pv_power_watts: self.pv_power_watts,
            battery_voltage_volts: self.battery_voltage_volts,
            battery_current_amperes: self.battery_current_amperes,
            load_current_amperes: self.load_current_amperes,
            load_power_watts: self.load_power_watts,
            yield_total_kwh: self.yield_total_kwh,
            yield_today_kwh: self.yield_today_kwh,
            ble_rssi_dbm: self.ble_rssi_dbm,
            charger_state: self.charger_state,
            load_state: self.load_state,
            connection_health: self.connection_health,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bounds::{
        BATTERY_CURRENT_A, BATTERY_VOLTAGE_V, BLE_RSSI_DBM, LOAD_CURRENT_A, LOAD_POWER_W,
        PV_CURRENT_A, PV_POWER_W, PV_VOLTAGE_V, YIELD_TODAY_KWH, YIELD_TOTAL_KWH,
    };
    use std::time::UNIX_EPOCH;

    fn t0() -> SystemTime {
        UNIX_EPOCH
    }

    fn device() -> DeviceId {
        DeviceId::new("solar-charger").unwrap()
    }

    #[test]
    fn rejects_out_of_range_per_field() {
        let device = device();
        assert!(Sample::builder(device.clone(), t0())
            .pv_voltage_volts(-1.0, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .pv_voltage_volts(250.01, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .pv_current_amperes(-0.1, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .pv_current_amperes(100.1, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .pv_power_watts(-0.1, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .pv_power_watts(30_001.0, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .battery_voltage_volts(-0.1, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .battery_voltage_volts(70.01, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .battery_current_amperes(-200.1, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .battery_current_amperes(200.1, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .load_current_amperes(-0.1, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .load_current_amperes(100.1, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .load_power_watts(-0.1, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .load_power_watts(6_000.1, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .yield_total_kwh(-0.1, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .yield_total_kwh(10_000_001.0, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .yield_today_kwh(-0.1, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .yield_today_kwh(2_000.1, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device.clone(), t0())
            .ble_rssi_dbm(-128.1, Quality::Candidate)
            .is_err());
        assert!(Sample::builder(device, t0())
            .ble_rssi_dbm(0.1, Quality::Candidate)
            .is_err());
    }

    #[test]
    fn rejects_non_finite() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            match Sample::builder(device(), t0()).pv_power_watts(bad, Quality::Candidate) {
                Err(DomainError::NonFinite { context, value }) => {
                    assert_eq!(context, "pv_power_watts");
                    assert!(!value.is_finite());
                }
                other => panic!("expected NonFinite for {bad}, got {other:?}"),
            }
        }
    }

    #[test]
    fn out_of_range_error_names_field() {
        let err = Sample::builder(device(), t0())
            .battery_voltage_volts(99.0, Quality::Candidate)
            .unwrap_err();
        match err {
            DomainError::OutOfRange {
                field,
                value,
                min,
                max,
            } => {
                assert_eq!(field, "battery_voltage_volts");
                assert_eq!(value, 99.0);
                assert_eq!(min, 0.0);
                assert_eq!(max, 70.0);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn boundary_values_are_accepted() {
        let s = Sample::builder(device(), t0())
            .pv_voltage_volts(PV_VOLTAGE_V.max(), Quality::Candidate)
            .unwrap()
            .pv_current_amperes(PV_CURRENT_A.max(), Quality::Candidate)
            .unwrap()
            .pv_power_watts(PV_POWER_W.max(), Quality::Candidate)
            .unwrap()
            .battery_voltage_volts(BATTERY_VOLTAGE_V.max(), Quality::Candidate)
            .unwrap()
            .battery_current_amperes(BATTERY_CURRENT_A.min(), Quality::Candidate)
            .unwrap()
            .load_current_amperes(LOAD_CURRENT_A.max(), Quality::Candidate)
            .unwrap()
            .load_power_watts(LOAD_POWER_W.max(), Quality::Candidate)
            .unwrap()
            .yield_total_kwh(YIELD_TOTAL_KWH.max(), Quality::Candidate)
            .unwrap()
            .yield_today_kwh(YIELD_TODAY_KWH.max(), Quality::Candidate)
            .unwrap()
            .ble_rssi_dbm(BLE_RSSI_DBM.min(), Quality::Candidate)
            .unwrap()
            .build();
        assert_eq!(s.measurement_count(), 10);
    }

    #[test]
    fn setting_later_fields_does_not_clobber_earlier() {
        let s = Sample::builder(device(), t0())
            .pv_voltage_volts(34.2, Quality::ConfirmedNative)
            .unwrap()
            .yield_total_kwh(5.0, Quality::ConfirmedNative)
            .unwrap()
            .build();
        assert_eq!(s.pv_voltage_volts().unwrap().value(), 34.2);
        assert_eq!(s.yield_total_kwh().unwrap().value(), 5.0);
    }

    #[test]
    fn builder_is_cloneable() {
        let b = Sample::builder(device(), t0())
            .pv_voltage_volts(34.2, Quality::ConfirmedNative)
            .unwrap();
        let b2 = b.clone();
        let s1 = b.build();
        let s2 = b2.build();
        assert_eq!(s1, s2);
    }
}
