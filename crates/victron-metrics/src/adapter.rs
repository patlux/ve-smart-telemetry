//! Plain-data adapter from acquisition results to a [`MetricBatchBuilder`].
//!
//! [`SampleView`] is a small, dependency-free shim (base units, bounded
//! charger state, health counters) that maps onto the metric contract in
//! [`crate::names`]. It exists so the metrics crate never has to depend on
//! the full domain model; the workspace integration converts its
//! `victron_domain::Sample` into this view (see the `domain` feature in
//! `crate::domain`) or fills it directly.
//!
//! Semantics:
//!
//! - `None` electrical values are omitted (no series emitted)
//! - `charger_state: None` omits the state metric; `Some` emits
//!   `name{device,state} 1`
//! - health fields are emitted when known (`Some`), including a known zero;
//!   `None` (unknown) omits the series, so unknown health is never rendered
//!   as a known zero and dashboards never see fabricated health series
//! - non-finite values never reach the wire

use crate::error::MetricError;
use crate::metric::MetricBatchBuilder;
use crate::names;

/// One acquisition cycle in base units, ready for encoding.
///
/// All values are `f64` in base SI units where applicable; timestamps are
/// explicit milliseconds since the Unix epoch.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SampleView<'a> {
    /// Configured stable device identity (the `device` label value).
    pub device: &'a str,
    /// Sample observation time, milliseconds since the Unix epoch.
    pub observed_at_ms: i64,
    /// PV voltage in volts.
    pub pv_voltage_volts: Option<f64>,
    /// PV current in amperes.
    pub pv_current_amperes: Option<f64>,
    /// PV power in watts.
    pub pv_power_watts: Option<f64>,
    /// Battery voltage in volts.
    pub battery_voltage_volts: Option<f64>,
    /// Battery current in amperes.
    pub battery_current_amperes: Option<f64>,
    /// Load power in watts.
    pub load_power_watts: Option<f64>,
    /// Load current in amperes.
    pub load_current_amperes: Option<f64>,
    /// Cumulative lifetime yield in kWh.
    pub yield_total_kwh: Option<f64>,
    /// Today's yield in kWh (diagnostic only).
    pub yield_today_kwh: Option<f64>,
    /// Bounded charger state label value (one of [`crate::names::states`]).
    pub charger_state: Option<&'static str>,
    /// BLE link up. `None` when unknown (series omitted); `Some(false)` is a
    /// known-down link.
    pub ble_up: Option<bool>,
    /// BLE RSSI in dBm.
    pub ble_rssi_dbm: Option<i32>,
    /// Unix time in seconds of the last successful acquisition.
    pub last_success_unixtime: Option<i64>,
    /// Age of the last sample in seconds.
    pub sample_age_seconds: Option<f64>,
    /// Cumulative BLE connect failures. `None` when unknown (series omitted).
    pub ble_connect_failures: Option<u64>,
    /// Cumulative protocol errors. `None` when unknown (series omitted).
    pub protocol_errors: Option<u64>,
    /// Cumulative dropped samples. `None` when unknown (series omitted).
    pub samples_dropped: Option<u64>,
    /// Cumulative seconds skipped by local energy integration (gaps that were
    /// never silently bridged). `None` when unknown (series omitted).
    pub energy_integration_gap_seconds: Option<u64>,
    /// Current spool depth. `None` when unknown (series omitted).
    pub spool_batches: Option<u64>,
    /// Age of the oldest spooled batch in seconds (`None` when the spool is
    /// empty; the series is then omitted).
    pub spool_oldest_age_seconds: Option<f64>,
}

/// Converts a [`SampleView`] into a timestamped batch. The batch uses
/// `observed_at_ms` as its explicit timestamp.
impl TryFrom<SampleView<'_>> for MetricBatchBuilder {
    type Error = MetricError;

    fn try_from(view: SampleView<'_>) -> Result<Self, MetricError> {
        let mut b = MetricBatchBuilder::new(view.device)?.with_timestamp_ms(view.observed_at_ms)?;

        if let Some(v) = view.pv_voltage_volts {
            b.gauge(names::PV_VOLTAGE_VOLTS, v)?;
        }
        if let Some(v) = view.pv_current_amperes {
            b.gauge(names::PV_CURRENT_AMPERES, v)?;
        }
        if let Some(v) = view.pv_power_watts {
            b.gauge(names::PV_POWER_WATTS, v)?;
        }
        if let Some(v) = view.battery_voltage_volts {
            b.gauge(names::BATTERY_VOLTAGE_VOLTS, v)?;
        }
        if let Some(v) = view.battery_current_amperes {
            b.gauge(names::BATTERY_CURRENT_AMPERES, v)?;
        }
        if let Some(v) = view.load_power_watts {
            b.gauge(names::LOAD_POWER_WATTS, v)?;
        }
        if let Some(v) = view.load_current_amperes {
            b.gauge(names::LOAD_CURRENT_AMPERES, v)?;
        }
        if let Some(v) = view.yield_total_kwh {
            b.gauge(names::YIELD_TOTAL_KWH, v)?;
        }
        if let Some(v) = view.yield_today_kwh {
            b.gauge(names::YIELD_TODAY_KWH, v)?;
        }
        if let Some(s) = view.charger_state {
            b.state(names::CHARGER_STATE, s)?;
        }
        if let Some(up) = view.ble_up {
            b.gauge(names::BLE_UP, if up { 1.0 } else { 0.0 })?;
        }
        if let Some(v) = view.ble_rssi_dbm {
            b.gauge(names::BLE_RSSI_DBM, f64::from(v))?;
        }
        if let Some(v) = view.last_success_unixtime {
            b.gauge(names::LAST_SUCCESS_UNIXTIME, v as f64)?;
        }
        if let Some(v) = view.sample_age_seconds {
            b.gauge(names::SAMPLE_AGE_SECONDS, v)?;
        }
        if let Some(v) = view.ble_connect_failures {
            b.counter(names::BLE_CONNECT_FAILURES_TOTAL, v as f64)?;
        }
        if let Some(v) = view.protocol_errors {
            b.counter(names::PROTOCOL_ERRORS_TOTAL, v as f64)?;
        }
        if let Some(v) = view.samples_dropped {
            b.counter(names::SAMPLES_DROPPED_TOTAL, v as f64)?;
        }
        if let Some(v) = view.energy_integration_gap_seconds {
            b.counter(names::ENERGY_INTEGRATION_GAP_SECONDS_TOTAL, v as f64)?;
        }
        if let Some(v) = view.spool_batches {
            b.gauge(names::SPOOL_BATCHES, v as f64)?;
        }
        if let Some(v) = view.spool_oldest_age_seconds {
            b.gauge(names::SPOOL_OLDEST_AGE_SECONDS, v)?;
        }
        Ok(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> SampleView<'static> {
        SampleView {
            device: "solar-charger",
            observed_at_ms: 1_700_000_000_000,
            pv_voltage_volts: Some(36.42),
            pv_current_amperes: Some(3.75),
            pv_power_watts: Some(136.4),
            battery_voltage_volts: Some(13.05),
            battery_current_amperes: Some(-4.2),
            yield_total_kwh: Some(12_345.678),
            charger_state: Some(crate::names::states::BULK),
            ble_up: Some(true),
            ble_rssi_dbm: Some(-61),
            last_success_unixtime: Some(1_700_000_000),
            sample_age_seconds: Some(3.5),
            ble_connect_failures: Some(2),
            protocol_errors: Some(1),
            samples_dropped: Some(0),
            energy_integration_gap_seconds: Some(0),
            spool_batches: Some(4),
            spool_oldest_age_seconds: Some(120.5),
            ..SampleView::default()
        }
    }

    #[test]
    fn view_converts_to_expected_series() {
        let batch = MetricBatchBuilder::try_from(view()).unwrap();
        let names: Vec<&str> = batch.points().iter().map(|p| p.name()).collect();
        assert!(names.contains(&names::PV_POWER_WATTS));
        assert!(names.contains(&names::CHARGER_STATE));
        assert!(names.contains(&names::BLE_CONNECT_FAILURES_TOTAL));
        assert!(names.contains(&names::SPOOL_BATCHES));
        assert!(names.contains(&names::SPOOL_OLDEST_AGE_SECONDS));
        assert!(names.contains(&names::ENERGY_INTEGRATION_GAP_SECONDS_TOTAL));
        assert_eq!(batch.points().len(), 17);

        let text = batch.encode();
        // charger state encoded as bounded state label with value 1
        assert!(text.contains(
            "victron_charger_state{device=\"solar-charger\",state=\"bulk\"} 1 1700000000000\n"
        ));
        // counters present even at zero
        assert!(text
            .contains("victron_samples_dropped_total{device=\"solar-charger\"} 0 1700000000000\n"));
        // energy-gap counter present even at zero
        assert!(text.contains(
            "victron_energy_integration_gap_seconds_total{device=\"solar-charger\"} 0 1700000000000\n"
        ));
        // spool oldest age gauge present
        assert!(text.contains(
            "victron_spool_oldest_age_seconds{device=\"solar-charger\"} 120.5 1700000000000\n"
        ));
    }

    #[test]
    fn none_fields_are_omitted() {
        let mut v = view();
        v.pv_power_watts = None;
        v.yield_total_kwh = None;
        v.charger_state = None;
        v.ble_up = Some(false); // known-down link is a real value
        let batch = MetricBatchBuilder::try_from(v).unwrap();
        let text = batch.encode();
        assert!(!text.contains("victron_pv_power_watts"));
        assert!(!text.contains("victron_yield_total_kwh"));
        assert!(!text.contains("victron_charger_state"));
        assert!(text.contains("victron_ble_up{device=\"solar-charger\"} 0 1700000000000\n"));
    }

    #[test]
    fn unknown_health_is_omitted_not_fabricated_as_zero() {
        // `None` health fields mean "unknown": the series must be omitted,
        // never rendered as a known zero. This is what makes unknown health
        // distinguishable from a real zero.
        let mut v = view();
        v.ble_up = None;
        v.ble_connect_failures = None;
        v.protocol_errors = None;
        v.samples_dropped = None;
        v.energy_integration_gap_seconds = None;
        v.spool_batches = None;
        v.spool_oldest_age_seconds = None;
        let batch = MetricBatchBuilder::try_from(v).unwrap();
        let text = batch.encode();
        assert!(!text.contains("victron_ble_up"));
        assert!(!text.contains("victron_ble_connect_failures_total"));
        assert!(!text.contains("victron_protocol_errors_total"));
        assert!(!text.contains("victron_samples_dropped_total"));
        assert!(!text.contains("victron_energy_integration_gap_seconds_total"));
        assert!(!text.contains("victron_spool_batches"));
        assert!(!text.contains("victron_spool_oldest_age_seconds"));
        // Measurements are still emitted.
        assert!(
            text.contains("victron_pv_power_watts{device=\"solar-charger\"} 136.4 1700000000000\n")
        );
    }

    #[test]
    fn energy_gap_counter_is_cumulative_seconds_not_events() {
        // A nontrivial gap of 600 seconds must be emitted as the value 600:
        // the counter accumulates skipped seconds, not gap events.
        let mut v = view();
        v.energy_integration_gap_seconds = Some(600);
        let batch = MetricBatchBuilder::try_from(v).unwrap();
        let text = batch.encode();
        assert!(text.contains(
            "victron_energy_integration_gap_seconds_total{device=\"solar-charger\"} 600 1700000000000\n"
        ));
    }

    #[test]
    fn non_finite_view_values_never_reach_wire() {
        let mut v = view();
        v.pv_power_watts = Some(f64::NAN);
        v.sample_age_seconds = Some(f64::INFINITY);
        let batch = MetricBatchBuilder::try_from(v).unwrap();
        let text = batch.encode();
        assert!(!text.contains("victron_pv_power_watts"));
        assert!(!text.contains("victron_sample_age_seconds"));
    }

    #[test]
    fn invalid_device_is_rejected() {
        let mut v = view();
        v.device = "bad\0device";
        assert!(MetricBatchBuilder::try_from(v).is_err());
    }

    #[test]
    fn non_positive_observed_at_is_rejected() {
        for bad in [0i64, -1, -1_700_000_000_000] {
            let mut v = view();
            v.observed_at_ms = bad;
            assert!(
                MetricBatchBuilder::try_from(v).is_err(),
                "observed_at_ms {bad} must be rejected"
            );
        }
    }

    #[test]
    fn empty_spool_omits_oldest_age_series() {
        let mut v = view();
        v.spool_oldest_age_seconds = None;
        let batch = MetricBatchBuilder::try_from(v).unwrap();
        let text = batch.encode();
        assert!(!text.contains("victron_spool_oldest_age_seconds"));
        assert!(text.contains("victron_spool_batches{device=\"solar-charger\"} 4 1700000000000\n"));
    }

    #[test]
    fn invalid_state_value_is_rejected() {
        let mut v = view();
        v.charger_state = Some("Bulk"); // uppercase violates bounded charset
        assert!(MetricBatchBuilder::try_from(v).is_err());
    }
}
