//! One observation of a Victron device at a point in time.
//!
//! A [`Sample`] is the stable seam other crates build against: it carries a
//! validated [`DeviceId`], a [`std::time::SystemTime`] timestamp, named
//! optional [`Measurement`] fields, and optional bounded state enums.
//!
//! Construction goes through [`SampleBuilder`], which validates every value
//! against the documented conservative ranges in [`crate::bounds`] and
//! rejects non-finite values.

use crate::builder::SampleBuilder;
use crate::device::DeviceId;
use crate::measurement::{Measurement, SampleField};
use crate::state::{ChargerState, ConnectionHealth, LoadState};
use std::time::{Duration, SystemTime};

/// Default maximum sample age before a sample is considered stale.
///
/// 300 s matches the maximum tolerated energy-integration gap in the
/// collector plan. Callers with different polling intervals can pass their
/// own bound to [`Sample::is_fresh`] / [`Sample::is_fresh_at`].
pub const DEFAULT_MAX_SAMPLE_AGE: Duration = Duration::from_secs(300);

/// One timestamped observation of a Victron device.
///
/// Fields are private; they are populated only through the validating
/// [`SampleBuilder`] (same crate) and read through accessors.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub(crate) device: DeviceId,
    pub(crate) observed_at: SystemTime,
    pub(crate) pv_voltage_volts: Option<Measurement>,
    pub(crate) pv_current_amperes: Option<Measurement>,
    pub(crate) pv_power_watts: Option<Measurement>,
    pub(crate) battery_voltage_volts: Option<Measurement>,
    pub(crate) battery_current_amperes: Option<Measurement>,
    pub(crate) load_current_amperes: Option<Measurement>,
    pub(crate) load_power_watts: Option<Measurement>,
    pub(crate) yield_total_kwh: Option<Measurement>,
    pub(crate) yield_today_kwh: Option<Measurement>,
    pub(crate) ble_rssi_dbm: Option<Measurement>,
    pub(crate) charger_state: Option<ChargerState>,
    pub(crate) load_state: Option<LoadState>,
    pub(crate) connection_health: Option<ConnectionHealth>,
}

impl Sample {
    /// Start building a sample with an explicit observation time.
    pub fn builder(device: DeviceId, observed_at: SystemTime) -> SampleBuilder {
        SampleBuilder::new(device, observed_at)
    }

    /// Start building a sample stamped with the current time.
    pub fn builder_now(device: DeviceId) -> SampleBuilder {
        SampleBuilder::new(device, SystemTime::now())
    }

    /// The validated device identity.
    pub fn device(&self) -> &DeviceId {
        &self.device
    }

    /// The observation timestamp.
    pub fn observed_at(&self) -> SystemTime {
        self.observed_at
    }

    /// PV voltage in volts, if present.
    pub fn pv_voltage_volts(&self) -> Option<&Measurement> {
        self.pv_voltage_volts.as_ref()
    }

    /// PV current in amperes, if present.
    pub fn pv_current_amperes(&self) -> Option<&Measurement> {
        self.pv_current_amperes.as_ref()
    }

    /// PV power in watts, if present.
    pub fn pv_power_watts(&self) -> Option<&Measurement> {
        self.pv_power_watts.as_ref()
    }

    /// Battery voltage in volts, if present.
    pub fn battery_voltage_volts(&self) -> Option<&Measurement> {
        self.battery_voltage_volts.as_ref()
    }

    /// Battery current in amperes (positive = charging), if present.
    pub fn battery_current_amperes(&self) -> Option<&Measurement> {
        self.battery_current_amperes.as_ref()
    }

    /// Load output current in amperes, if present.
    pub fn load_current_amperes(&self) -> Option<&Measurement> {
        self.load_current_amperes.as_ref()
    }

    /// Load output power in watts, if present.
    pub fn load_power_watts(&self) -> Option<&Measurement> {
        self.load_power_watts.as_ref()
    }

    /// Native lifetime yield counter in kWh, if present.
    pub fn yield_total_kwh(&self) -> Option<&Measurement> {
        self.yield_total_kwh.as_ref()
    }

    /// Yield since local midnight in kWh, if present.
    pub fn yield_today_kwh(&self) -> Option<&Measurement> {
        self.yield_today_kwh.as_ref()
    }

    /// BLE RSSI in dBm, if present.
    pub fn ble_rssi_dbm(&self) -> Option<&Measurement> {
        self.ble_rssi_dbm.as_ref()
    }

    /// Charger state, if present.
    pub fn charger_state(&self) -> Option<ChargerState> {
        self.charger_state
    }

    /// Load output state, if present.
    pub fn load_state(&self) -> Option<LoadState> {
        self.load_state
    }

    /// BLE connection health, if present.
    pub fn connection_health(&self) -> Option<ConnectionHealth> {
        self.connection_health
    }

    /// Elapsed time between `observed_at` and `now`.
    ///
    /// Returns `None` when `now` is before `observed_at` (clock moved
    /// backwards), which callers should treat as "not fresh".
    pub fn age_at(&self, now: SystemTime) -> Option<Duration> {
        now.duration_since(self.observed_at).ok()
    }

    /// Elapsed time between `observed_at` and the current time.
    ///
    /// Returns `None` when the clock moved backwards.
    pub fn age(&self) -> Option<Duration> {
        self.age_at(SystemTime::now())
    }

    /// Whether this sample is at most `max_age` old relative to `now`.
    ///
    /// `false` when the sample is older than `max_age` or the clock moved
    /// backwards relative to `observed_at`.
    pub fn is_fresh_at(&self, now: SystemTime, max_age: Duration) -> bool {
        matches!(self.age_at(now), Some(age) if age <= max_age)
    }

    /// Whether this sample is at most `max_age` old relative to now.
    ///
    /// Convenience wrapper over [`Sample::is_fresh_at`].
    pub fn is_fresh(&self, max_age: Duration) -> bool {
        self.is_fresh_at(SystemTime::now(), max_age)
    }

    /// Number of present measurement fields (states and health excluded).
    pub fn measurement_count(&self) -> usize {
        self.iter().count()
    }

    /// Whether any measurement field is present.
    pub fn has_measurements(&self) -> bool {
        self.measurement_count() > 0
    }

    /// Whether no field at all (measurement, state, or health) is present.
    pub fn is_empty(&self) -> bool {
        self.measurement_count() == 0
            && self.charger_state.is_none()
            && self.load_state.is_none()
            && self.connection_health.is_none()
    }

    /// Whether the sample carries at least one field.
    ///
    /// All values inside are valid by construction; this checks that the
    /// sample is not an empty shell.
    pub fn is_valid(&self) -> bool {
        !self.is_empty()
    }

    /// Iterate over the present measurement fields in a stable order:
    /// PV voltage, PV current, PV power, battery voltage, battery current,
    /// load current, load power, yield total, yield today, BLE RSSI.
    pub fn iter(&self) -> impl Iterator<Item = (SampleField, &Measurement)> + '_ {
        [
            (SampleField::PvVoltageVolts, self.pv_voltage_volts.as_ref()),
            (
                SampleField::PvCurrentAmperes,
                self.pv_current_amperes.as_ref(),
            ),
            (SampleField::PvPowerWatts, self.pv_power_watts.as_ref()),
            (
                SampleField::BatteryVoltageVolts,
                self.battery_voltage_volts.as_ref(),
            ),
            (
                SampleField::BatteryCurrentAmperes,
                self.battery_current_amperes.as_ref(),
            ),
            (
                SampleField::LoadCurrentAmperes,
                self.load_current_amperes.as_ref(),
            ),
            (SampleField::LoadPowerWatts, self.load_power_watts.as_ref()),
            (SampleField::YieldTotalKwh, self.yield_total_kwh.as_ref()),
            (SampleField::YieldTodayKwh, self.yield_today_kwh.as_ref()),
            (SampleField::BleRssiDbm, self.ble_rssi_dbm.as_ref()),
        ]
        .into_iter()
        .filter_map(|(field, measurement)| measurement.map(|m| (field, m)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quality::Quality;

    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    fn device() -> DeviceId {
        DeviceId::new("solar-charger").unwrap()
    }

    fn full_sample() -> Sample {
        Sample::builder(device(), t0() + Duration::from_secs(1000))
            .pv_voltage_volts(34.2, Quality::ConfirmedNative)
            .unwrap()
            .pv_current_amperes(2.8, Quality::Candidate)
            .unwrap()
            .pv_power_watts(96.0, Quality::ConfirmedNative)
            .unwrap()
            .battery_voltage_volts(12.6, Quality::Candidate)
            .unwrap()
            .battery_current_amperes(-1.2, Quality::Candidate)
            .unwrap()
            .load_current_amperes(3.5, Quality::Candidate)
            .unwrap()
            .load_power_watts(44.1, Quality::Candidate)
            .unwrap()
            .yield_total_kwh(1234.56, Quality::ConfirmedNative)
            .unwrap()
            .yield_today_kwh(1.23, Quality::ConfirmedNative)
            .unwrap()
            .ble_rssi_dbm(-61.0, Quality::ConfirmedNative)
            .unwrap()
            .charger_state(ChargerState::Bulk)
            .load_state(LoadState::On)
            .connection_health(ConnectionHealth::Up)
            .build()
    }

    #[test]
    fn full_sample_round_trip() {
        let s = full_sample();
        assert_eq!(s.device(), &device());
        assert_eq!(s.observed_at(), t0() + Duration::from_secs(1000));
        assert_eq!(s.pv_voltage_volts().unwrap().value(), 34.2);
        assert_eq!(
            s.pv_voltage_volts().unwrap().quality(),
            Quality::ConfirmedNative
        );
        assert_eq!(s.pv_current_amperes().unwrap().value(), 2.8);
        assert_eq!(s.pv_power_watts().unwrap().value(), 96.0);
        assert_eq!(s.battery_voltage_volts().unwrap().value(), 12.6);
        assert_eq!(s.battery_current_amperes().unwrap().value(), -1.2);
        assert_eq!(s.load_current_amperes().unwrap().value(), 3.5);
        assert_eq!(s.load_power_watts().unwrap().value(), 44.1);
        assert_eq!(s.yield_total_kwh().unwrap().value(), 1234.56);
        assert_eq!(s.yield_today_kwh().unwrap().value(), 1.23);
        assert_eq!(s.ble_rssi_dbm().unwrap().value(), -61.0);
        assert_eq!(s.charger_state(), Some(ChargerState::Bulk));
        assert_eq!(s.load_state(), Some(LoadState::On));
        assert_eq!(s.connection_health(), Some(ConnectionHealth::Up));
        assert_eq!(s.measurement_count(), 10);
        assert!(s.has_measurements());
        assert!(!s.is_empty());
        assert!(s.is_valid());
    }

    #[test]
    fn empty_sample_state() {
        let s = Sample::builder(device(), t0()).build();
        assert!(s.is_empty());
        assert!(!s.is_valid());
        assert!(!s.has_measurements());
        assert_eq!(s.measurement_count(), 0);
        assert_eq!(s.iter().count(), 0);
    }

    #[test]
    fn enum_only_sample_is_valid() {
        let s = Sample::builder(device(), t0())
            .charger_state(ChargerState::Float)
            .connection_health(ConnectionHealth::Down)
            .build();
        assert_eq!(s.measurement_count(), 0);
        assert!(!s.is_empty());
        assert!(s.is_valid());
    }

    #[test]
    fn missing_fields_are_none() {
        let s = Sample::builder(device(), t0())
            .pv_voltage_volts(12.0, Quality::ConfirmedNative)
            .unwrap()
            .build();
        assert_eq!(s.measurement_count(), 1);
        assert!(s.pv_current_amperes().is_none());
        assert!(s.pv_power_watts().is_none());
        assert!(s.yield_total_kwh().is_none());
        assert!(s.charger_state().is_none());
    }

    #[test]
    fn iter_yields_present_fields_in_order() {
        let s = Sample::builder(device(), t0())
            .pv_power_watts(96.0, Quality::ConfirmedNative)
            .unwrap()
            .battery_voltage_volts(12.6, Quality::Candidate)
            .unwrap()
            .build();
        let fields: Vec<(SampleField, &Measurement)> = s.iter().collect();
        assert_eq!(
            fields,
            vec![
                (SampleField::PvPowerWatts, s.pv_power_watts().unwrap()),
                (
                    SampleField::BatteryVoltageVolts,
                    s.battery_voltage_volts().unwrap()
                ),
            ]
        );
    }

    #[test]
    fn freshness() {
        let observed = t0();
        let s = Sample::builder(device(), observed).build();
        // Same instant: age 0, fresh.
        assert_eq!(s.age_at(observed), Some(Duration::ZERO));
        assert!(s.is_fresh_at(observed, Duration::from_secs(300)));
        // 250 s later: fresh with 300 s bound.
        assert_eq!(
            s.age_at(observed + Duration::from_secs(250)),
            Some(Duration::from_secs(250))
        );
        assert!(s.is_fresh_at(
            observed + Duration::from_secs(250),
            Duration::from_secs(300)
        ));
        // 301 s later: stale.
        assert!(!s.is_fresh_at(
            observed + Duration::from_secs(301),
            Duration::from_secs(300)
        ));
        // Clock moved backwards: no age, not fresh.
        assert_eq!(s.age_at(observed - Duration::from_secs(60)), None);
        assert!(!s.is_fresh_at(observed - Duration::from_secs(60), Duration::from_secs(300)));
        // Boundary is inclusive.
        assert!(s.is_fresh_at(
            observed + Duration::from_secs(300),
            Duration::from_secs(300)
        ));
    }

    #[test]
    fn clone_and_partial_eq() {
        let a = full_sample();
        let b = a.clone();
        assert_eq!(a, b);
        let c = Sample::builder(device(), t0())
            .pv_voltage_volts(1.0, Quality::Candidate)
            .unwrap()
            .build();
        assert_ne!(a, c);
    }

    #[test]
    fn builder_now_stamps_current_time() {
        let before = SystemTime::now();
        let s = Sample::builder_now(device()).build();
        let after = SystemTime::now();
        assert!(before <= s.observed_at() && s.observed_at() <= after);
    }

    #[test]
    fn default_max_sample_age_is_300s() {
        assert_eq!(DEFAULT_MAX_SAMPLE_AGE, Duration::from_secs(300));
    }
}
