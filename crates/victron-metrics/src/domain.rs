//! Integration adapter from the workspace `victron-domain` crate
//! (feature `domain`, not enabled by default).
//!
//! Fills the measurement fields of a caller-supplied [`crate::adapter::SampleView`]
//! from a real `victron_domain::Sample` using the accessors the domain crate
//! actually exposes (`device()`, `observed_at()`, named `Option<&Measurement>`
//! getters, `charger_state()`). [`victron_domain::Measurement`] is
//! non-generic and always finite by construction;
//! [`victron_domain::ChargerState`] includes `Unknown(u8)`, `StartingUp`,
//! `AutoRecondition`, and `ExternalControl` variants, which are mapped onto
//! the bounded label vocabulary in [`crate::names::states`].
//!
//! # Integration contract (health is never fabricated)
//!
//! A domain `Sample` carries measurements and charger state only — it has no
//! BLE link state, no cumulative health counters, and no spool depth. Those
//! live in the service layer. The caller therefore supplies a [`SampleView`]
//! that already contains the real health context; this module overwrites only
//! the device identity, observation time, and measurement/charger-state
//! fields. Health fields in the supplied view are preserved untouched. A
//! supplied `yield_total_kwh` is also preserved so the service can provide its
//! resolved native-or-integrated canonical energy value. Mapping a domain
//! sample can therefore never synthesize health series: a caller
//! without health context passes a view with `None` health fields and the
//! batch simply omits those series (unknown health is never rendered as a
//! known zero).
//!
//! The mapping is deliberately thin: no validation, no scaling, no duplicate
//! domain logic. It converts base-unit domain values into [`crate::adapter::SampleView`].

use crate::adapter::SampleView;
use crate::error::MetricError;
use crate::metric::{system_time_to_ms, MetricBatchBuilder};

/// Maps one domain sample into a ready-to-encode batch.
///
/// `device` is the configured stable device identity (the `device` label).
/// `health` is a caller-supplied [`SampleView`] that already carries the real
/// health context from the service layer (BLE link state, cumulative
/// counters, spool depth). Only the device identity, observation time, and
/// measurement/charger-state fields are taken from the domain `sample`; every
/// health field and an explicitly supplied resolved yield in `health` are
/// preserved untouched. A domain sample therefore
/// can never fabricate health series — pass a view with `None` health fields
/// when there is no health context and those series are omitted.
///
/// # Errors
///
/// Returns a [`MetricError`] when the observation time cannot be converted
/// (clock before the Unix epoch) or the sample's timestamp is not strictly
/// positive Unix milliseconds.
pub fn sample_to_batch<'a>(
    device: &'a str,
    sample: &victron_domain::Sample,
    mut health: SampleView<'a>,
) -> Result<MetricBatchBuilder, MetricError> {
    let observed_at_ms = system_time_to_ms(sample.observed_at())?;
    health.device = device;
    health.observed_at_ms = observed_at_ms;
    health.pv_voltage_volts = sample.pv_voltage_volts().map(measurement_value);
    health.pv_current_amperes = sample.pv_current_amperes().map(measurement_value);
    health.pv_power_watts = sample.pv_power_watts().map(measurement_value);
    health.battery_voltage_volts = sample.battery_voltage_volts().map(measurement_value);
    health.battery_current_amperes = sample.battery_current_amperes().map(measurement_value);
    health.load_power_watts = sample.load_power_watts().map(measurement_value);
    health.load_current_amperes = sample.load_current_amperes().map(measurement_value);
    if health.yield_total_kwh.is_none() {
        health.yield_total_kwh = sample.yield_total_kwh().map(measurement_value);
    }
    health.yield_today_kwh = sample.yield_today_kwh().map(measurement_value);
    health.ble_rssi_dbm = sample.ble_rssi_dbm().map(|m| m.value() as i32);
    health.charger_state = sample.charger_state().map(charger_state_label);
    MetricBatchBuilder::try_from(health)
}

/// Borrows the numeric value out of a domain `Measurement` (always finite).
fn measurement_value(m: &victron_domain::Measurement) -> f64 {
    m.value()
}

/// Maps the domain charger state enum onto the bounded label vocabulary in
/// [`crate::names::states`].
fn charger_state_label(state: victron_domain::ChargerState) -> &'static str {
    use crate::names::states;
    match state {
        victron_domain::ChargerState::Off => states::OFF,
        victron_domain::ChargerState::Fault => states::FAULT,
        victron_domain::ChargerState::Bulk => states::BULK,
        victron_domain::ChargerState::Absorption => states::ABSORPTION,
        victron_domain::ChargerState::Float => states::FLOAT,
        victron_domain::ChargerState::Storage => states::STORAGE,
        victron_domain::ChargerState::Equalize => states::EQUALIZE,
        victron_domain::ChargerState::StartingUp => states::STARTING_UP,
        victron_domain::ChargerState::AutoRecondition => states::AUTO_RECONDITION,
        victron_domain::ChargerState::ExternalControl => states::EXTERNAL_CONTROL,
        victron_domain::ChargerState::Unknown(_) => states::UNKNOWN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use victron_domain::{ChargerState, DeviceId, Quality, Sample};

    fn sample(charger_state: Option<ChargerState>) -> Sample {
        let device = DeviceId::new("solar-charger").unwrap();
        let mut b = Sample::builder_now(device)
            .pv_voltage_volts(36.42, Quality::ConfirmedNative)
            .unwrap()
            .pv_current_amperes(3.75, Quality::ConfirmedNative)
            .unwrap()
            .pv_power_watts(136.4, Quality::ConfirmedNative)
            .unwrap()
            .battery_voltage_volts(13.05, Quality::Candidate)
            .unwrap()
            .yield_total_kwh(12_345.678, Quality::ConfirmedNative)
            .unwrap()
            .ble_rssi_dbm(-61.0, Quality::ConfirmedNative)
            .unwrap();
        if let Some(s) = charger_state {
            b = b.charger_state(s);
        }
        b.build()
    }

    /// A caller-supplied health view with real (non-default) health context.
    fn health_view() -> SampleView<'static> {
        SampleView {
            device: "ignored",
            observed_at_ms: 0,
            ble_up: Some(true),
            ble_connect_failures: Some(3),
            protocol_errors: Some(1),
            samples_dropped: Some(0),
            energy_integration_gap_seconds: Some(600),
            spool_batches: Some(2),
            spool_oldest_age_seconds: Some(30.0),
            ..SampleView::default()
        }
    }

    #[test]
    fn maps_domain_sample_into_expected_series() {
        let batch = sample_to_batch(
            "solar-charger",
            &sample(Some(ChargerState::Bulk)),
            health_view(),
        )
        .unwrap();
        let text = batch.encode();
        assert!(text.contains("victron_pv_power_watts{device=\"solar-charger\"} 136.4 1"));
        assert!(text.contains("victron_charger_state{device=\"solar-charger\",state=\"bulk\"} 1 1"));
        assert!(text.contains("victron_yield_total_kwh{device=\"solar-charger\"} 12345.678 1"));
        assert!(text.contains("victron_ble_rssi_dbm{device=\"solar-charger\"} -61 1"));
        // Health context supplied by the caller is preserved verbatim.
        assert!(text.contains("victron_ble_up{device=\"solar-charger\"} 1 1"));
        assert!(text.contains("victron_ble_connect_failures_total{device=\"solar-charger\"} 3 1"));
        assert!(text.contains(
            "victron_energy_integration_gap_seconds_total{device=\"solar-charger\"} 600 1"
        ));
        assert!(text.contains("victron_spool_batches{device=\"solar-charger\"} 2 1"));
    }

    #[test]
    fn domain_adapter_does_not_synthesize_health() {
        // A caller with no health context passes a view whose health fields
        // are all `None`. The batch must omit every health series instead of
        // fabricating `ble_up=0` and zero counters.
        let batch = sample_to_batch(
            "solar-charger",
            &sample(Some(ChargerState::Bulk)),
            SampleView::default(),
        )
        .unwrap();
        let text = batch.encode();
        assert!(!text.contains("victron_ble_up"));
        assert!(!text.contains("victron_ble_connect_failures_total"));
        assert!(!text.contains("victron_protocol_errors_total"));
        assert!(!text.contains("victron_samples_dropped_total"));
        assert!(!text.contains("victron_energy_integration_gap_seconds_total"));
        assert!(!text.contains("victron_spool_batches"));
        assert!(!text.contains("victron_spool_oldest_age_seconds"));
        // Measurements and charger state are still mapped.
        assert!(text.contains("victron_pv_power_watts{device=\"solar-charger\"} 136.4 1"));
        assert!(text.contains("victron_charger_state{device=\"solar-charger\",state=\"bulk\"} 1 1"));
    }

    #[test]
    fn domain_adapter_preserves_caller_health_context() {
        // Health values supplied by the caller pass through unchanged; the
        // domain sample contributes no health data.
        let batch = sample_to_batch(
            "solar-charger",
            &sample(Some(ChargerState::Bulk)),
            health_view(),
        )
        .unwrap();
        let text = batch.encode();
        assert!(text.contains("victron_ble_up{device=\"solar-charger\"} 1 1"));
        assert!(text.contains("victron_ble_connect_failures_total{device=\"solar-charger\"} 3 1"));
        assert!(text.contains("victron_protocol_errors_total{device=\"solar-charger\"} 1 1"));
        assert!(text.contains("victron_samples_dropped_total{device=\"solar-charger\"} 0 1"));
        assert!(text.contains(
            "victron_energy_integration_gap_seconds_total{device=\"solar-charger\"} 600 1"
        ));
        assert!(text.contains("victron_spool_batches{device=\"solar-charger\"} 2 1"));
        assert!(text.contains("victron_spool_oldest_age_seconds{device=\"solar-charger\"} 30 1"));
    }

    #[test]
    fn supplied_resolved_yield_overrides_sample_yield() {
        let health = SampleView {
            yield_total_kwh: Some(7.5),
            ..SampleView::default()
        };
        let batch =
            sample_to_batch("solar-charger", &sample(Some(ChargerState::Bulk)), health).unwrap();
        let text = batch.encode();
        assert!(text.contains("victron_yield_total_kwh{device=\"solar-charger\"} 7.5 1"));
        assert!(!text.contains("12345.678"));
    }

    #[test]
    fn maps_new_and_unknown_charger_states() {
        for (state, label) in [
            (ChargerState::StartingUp, "starting_up"),
            (ChargerState::AutoRecondition, "auto_recondition"),
            (ChargerState::ExternalControl, "external_control"),
            (ChargerState::Unknown(9), "unknown"),
            (ChargerState::Unknown(255), "unknown"),
        ] {
            let batch =
                sample_to_batch("solar-charger", &sample(Some(state)), SampleView::default())
                    .unwrap();
            let text = batch.encode();
            assert!(
                text.contains(&format!(
                    "victron_charger_state{{device=\"solar-charger\",state=\"{label}\"}} 1 1"
                )),
                "state {state:?} should map to {label:?}"
            );
            // No health series may be synthesized for any charger state.
            assert!(!text.contains("victron_ble_up"));
            assert!(!text.contains("victron_ble_connect_failures_total"));
        }
    }

    #[test]
    fn omits_missing_states_and_measurements() {
        let batch = sample_to_batch("solar-charger", &sample(None), SampleView::default()).unwrap();
        let text = batch.encode();
        assert!(!text.contains("victron_charger_state"));
        assert!(!text.contains("victron_load_power_watts"));
    }
}
