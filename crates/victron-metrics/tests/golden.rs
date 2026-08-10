//! Golden tests for the Prometheus text encoding.
//!
//! These pin the exact wire format: sorted series, escaped labels, explicit
//! millisecond timestamps, omission of non-finite values, and the bounded
//! charger-state encoding. They never touch the network.

use victron_metrics::adapter::SampleView;
use victron_metrics::{MetricBatchBuilder, MetricError, MetricKind, MetricPoint};

const TS: i64 = 1_700_000_000_000;

/// The full contract batch produced from a realistic acquisition cycle.
fn full_view() -> SampleView<'static> {
    SampleView {
        device: "solar-charger",
        observed_at_ms: TS,
        pv_voltage_volts: Some(36.42),
        pv_current_amperes: Some(3.75),
        pv_power_watts: Some(136.4),
        battery_voltage_volts: Some(13.05),
        battery_current_amperes: Some(-4.2),
        load_power_watts: Some(12.8),
        load_current_amperes: Some(1.1),
        yield_total_kwh: Some(12_345.678),
        yield_today_kwh: Some(3.21),
        charger_state: Some(victron_metrics::names::states::BULK),
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
    }
}

#[test]
fn golden_full_batch() {
    let batch = MetricBatchBuilder::try_from(full_view()).unwrap();
    let expected = "\
victron_battery_current_amperes{device=\"solar-charger\"} -4.2 1700000000000\n\
victron_battery_voltage_volts{device=\"solar-charger\"} 13.05 1700000000000\n\
victron_ble_connect_failures_total{device=\"solar-charger\"} 2 1700000000000\n\
victron_ble_rssi_dbm{device=\"solar-charger\"} -61 1700000000000\n\
victron_ble_up{device=\"solar-charger\"} 1 1700000000000\n\
victron_charger_state{device=\"solar-charger\",state=\"bulk\"} 1 1700000000000\n\
victron_energy_integration_gap_seconds_total{device=\"solar-charger\"} 0 1700000000000\n\
victron_last_success_unixtime{device=\"solar-charger\"} 1700000000 1700000000000\n\
victron_load_current_amperes{device=\"solar-charger\"} 1.1 1700000000000\n\
victron_load_power_watts{device=\"solar-charger\"} 12.8 1700000000000\n\
victron_protocol_errors_total{device=\"solar-charger\"} 1 1700000000000\n\
victron_pv_current_amperes{device=\"solar-charger\"} 3.75 1700000000000\n\
victron_pv_power_watts{device=\"solar-charger\"} 136.4 1700000000000\n\
victron_pv_voltage_volts{device=\"solar-charger\"} 36.42 1700000000000\n\
victron_sample_age_seconds{device=\"solar-charger\"} 3.5 1700000000000\n\
victron_samples_dropped_total{device=\"solar-charger\"} 0 1700000000000\n\
victron_spool_batches{device=\"solar-charger\"} 4 1700000000000\n\
victron_spool_oldest_age_seconds{device=\"solar-charger\"} 120.5 1700000000000\n\
victron_yield_today_kwh{device=\"solar-charger\"} 3.21 1700000000000\n\
victron_yield_total_kwh{device=\"solar-charger\"} 12345.678 1700000000000\n";
    assert_eq!(batch.encode(), expected);
    // Rendering twice must be byte-identical.
    assert_eq!(batch.encode(), batch.encode());
}

#[test]
fn golden_minimal_batch() {
    let mut b = MetricBatchBuilder::new("solar-charger")
        .unwrap()
        .with_timestamp_ms(TS)
        .unwrap();
    b.gauge(victron_metrics::names::PV_POWER_WATTS, 136.4)
        .unwrap();
    b.state(
        victron_metrics::names::CHARGER_STATE,
        victron_metrics::names::states::BULK,
    )
    .unwrap();
    b.gauge(victron_metrics::names::BLE_UP, 1.0).unwrap();
    assert_eq!(
        b.encode(),
        "victron_ble_up{device=\"solar-charger\"} 1 1700000000000\n\
         victron_charger_state{device=\"solar-charger\",state=\"bulk\"} 1 1700000000000\n\
         victron_pv_power_watts{device=\"solar-charger\"} 136.4 1700000000000\n"
    );
}

#[test]
fn golden_label_escaping() {
    let mut b = MetricBatchBuilder::new("dev \"quoted\"")
        .unwrap()
        .with_timestamp_ms(TS)
        .unwrap();
    // Value with backslash, quote, and newline; name and label stay valid.
    b.gauge_with("m", &[("reason", "a\"b\\c\nd")], 1.0).unwrap();
    assert_eq!(
        b.encode(),
        "m{device=\"dev \\\"quoted\\\"\",reason=\"a\\\"b\\\\c\\nd\"} 1 1700000000000\n"
    );
}

#[test]
fn golden_non_finite_values_are_omitted() {
    let mut b = MetricBatchBuilder::new("d")
        .unwrap()
        .with_timestamp_ms(TS)
        .unwrap();
    b.gauge("a", f64::NAN).unwrap();
    b.gauge("b", f64::INFINITY).unwrap();
    b.counter("c_total", f64::NEG_INFINITY).unwrap();
    b.gauge("ok", 7.0).unwrap();
    assert_eq!(b.encode(), "ok{device=\"d\"} 7 1700000000000\n");
}

#[test]
fn golden_state_value_is_always_one() {
    let mut b = MetricBatchBuilder::new("d")
        .unwrap()
        .with_timestamp_ms(TS)
        .unwrap();
    b.state(
        victron_metrics::names::CHARGER_STATE,
        victron_metrics::names::states::ABSORPTION,
    )
    .unwrap();
    assert_eq!(
        b.encode(),
        "victron_charger_state{device=\"d\",state=\"absorption\"} 1 1700000000000\n"
    );
}

#[test]
fn golden_deterministic_regardless_of_insertion_order() {
    let mut a = MetricBatchBuilder::new("d")
        .unwrap()
        .with_timestamp_ms(TS)
        .unwrap();
    a.gauge("z", 1.0).unwrap();
    a.gauge("a", 2.0).unwrap();
    a.gauge("m", 3.0).unwrap();

    let mut b = MetricBatchBuilder::new("d")
        .unwrap()
        .with_timestamp_ms(TS)
        .unwrap();
    b.gauge("m", 3.0).unwrap();
    b.gauge("z", 1.0).unwrap();
    b.gauge("a", 2.0).unwrap();

    assert_eq!(a.encode(), b.encode());
    assert_eq!(
        a.encode(),
        "a{device=\"d\"} 2 1700000000000\nm{device=\"d\"} 3 1700000000000\nz{device=\"d\"} 1 1700000000000\n"
    );
}

#[test]
fn golden_duplicate_series_last_wins() {
    let mut b = MetricBatchBuilder::new("d")
        .unwrap()
        .with_timestamp_ms(TS)
        .unwrap();
    b.gauge("m", 1.0).unwrap();
    b.gauge("m", 9.0).unwrap();
    assert_eq!(b.encode(), "m{device=\"d\"} 9 1700000000000\n");
}

#[test]
fn golden_direct_points_keep_own_timestamps_and_kinds() {
    let mut b = MetricBatchBuilder::new("d").unwrap();
    let p = MetricPoint::counter(
        "events_total",
        vec![("device".into(), "d".into())],
        12.0,
        TS + 5,
    )
    .unwrap();
    assert_eq!(p.kind(), MetricKind::Counter);
    b.push_point(p);
    assert_eq!(b.encode(), "events_total{device=\"d\"} 12 1700000000005\n");
}

#[test]
fn golden_invalid_inputs_are_rejected() {
    let mut b = MetricBatchBuilder::new("d")
        .unwrap()
        .with_timestamp_ms(TS)
        .unwrap();
    // Reserved metric name.
    assert_eq!(
        b.gauge("__prometheus_private", 1.0),
        Err(MetricError::ReservedName("__prometheus_private".into()))
    );
    // Invalid label name.
    assert_eq!(
        b.gauge_with("m", &[("bad-name", "v")], 1.0),
        Err(MetricError::InvalidLabelName("bad-name".into()))
    );
    // Control byte in label value (unescapable).
    assert_eq!(
        b.gauge_with("m", &[("k", "a\tb")], 1.0),
        Err(MetricError::InvalidLabelValue("a\tb".into()))
    );
    // Duplicate device label supplied by caller.
    assert_eq!(
        b.gauge_with("m", &[("device", "other")], 1.0),
        Err(MetricError::DuplicateLabel("device".into()))
    );
    // Counter without _total suffix.
    assert_eq!(
        b.counter("not_a_counter", 1.0),
        Err(MetricError::CounterNameMissingSuffix(
            "not_a_counter".into()
        ))
    );
    assert!(b.is_empty());
}

#[test]
fn golden_batch_is_empty_until_points_are_added() {
    let b = MetricBatchBuilder::new("d")
        .unwrap()
        .with_timestamp_ms(TS)
        .unwrap();
    assert!(b.is_empty());
    assert_eq!(b.encode(), "");
}
