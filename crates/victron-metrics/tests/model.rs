//! Model-level unit tests (public API only).
//!
//! These mirror the in-module tests that historically lived in `metric.rs`
//! and cover name/label validation, counter typing, omission of non-finite
//! values, state bounds, and timestamp handling.

use victron_metrics::{MetricBatchBuilder, MetricError, MetricKind, MetricName, MetricPoint};

fn ts() -> i64 {
    1_700_000_000_000
}

#[test]
fn valid_metric_names() {
    for name in ["victron_pv_power_watts", "a", "a_b:c", "_x", "x_0"] {
        assert!(MetricName::new(name).is_ok(), "{name} should be valid");
    }
}

#[test]
fn invalid_metric_names() {
    for name in ["", "9abc", "a-b", "a b", "__reserved", "a b!"] {
        assert!(MetricName::new(name).is_err(), "{name} should be invalid");
    }
}

#[test]
fn label_validation() {
    assert_eq!(
        MetricPoint::gauge(
            "m",
            vec![("a".into(), "v".into()), ("a".into(), "w".into())],
            1.0,
            ts()
        ),
        Err(MetricError::DuplicateLabel("a".into()))
    );
    assert!(matches!(
        MetricPoint::gauge("m", vec![("__x".into(), "v".into())], 1.0, ts()),
        Err(MetricError::ReservedLabelName(_))
    ));
    assert!(matches!(
        MetricPoint::gauge("m", vec![("ok".into(), "bad\0value".into())], 1.0, ts()),
        Err(MetricError::InvalidLabelValue(_))
    ));
    // Too many labels.
    let labels: Vec<(String, String)> = (0..9).map(|i| (format!("k{i}"), "v".into())).collect();
    assert!(matches!(
        MetricPoint::gauge("m", labels, 1.0, ts()),
        Err(MetricError::TooManyLabels(9))
    ));
}

#[test]
fn labels_are_sorted() {
    let p = MetricPoint::gauge(
        "m",
        vec![
            ("z".into(), "1".into()),
            ("a".into(), "2".into()),
            ("m".into(), "3".into()),
        ],
        1.0,
        ts(),
    )
    .unwrap();
    let names: Vec<&str> = p.labels().iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(names, ["a", "m", "z"]);
}

#[test]
fn counter_requires_total_suffix() {
    assert!(matches!(
        MetricBatchBuilder::new("d")
            .unwrap()
            .with_timestamp_ms(ts())
            .unwrap()
            .counter("victron_ble_failures", 3.0),
        Err(MetricError::CounterNameMissingSuffix(_))
    ));
    assert!(MetricBatchBuilder::new("d")
        .unwrap()
        .with_timestamp_ms(ts())
        .unwrap()
        .counter("victron_ble_failures_total", 3.0)
        .is_ok());
}

#[test]
fn non_finite_values_are_omitted() {
    let mut b = MetricBatchBuilder::new("d")
        .unwrap()
        .with_timestamp_ms(ts())
        .unwrap();
    assert!(!b.gauge("m", f64::NAN).unwrap());
    assert!(!b.gauge("m", f64::INFINITY).unwrap());
    assert!(!b.gauge("m", f64::NEG_INFINITY).unwrap());
    assert_eq!(b.len(), 0);
    assert!(b.gauge("m", 1.5).unwrap());
    assert_eq!(b.len(), 1);
}

#[test]
fn state_value_bounds() {
    assert!(MetricBatchBuilder::is_valid_state_value("bulk"));
    assert!(MetricBatchBuilder::is_valid_state_value("absorption"));
    assert!(!MetricBatchBuilder::is_valid_state_value(""));
    assert!(!MetricBatchBuilder::is_valid_state_value("Bulk")); // uppercase
    assert!(!MetricBatchBuilder::is_valid_state_value("has space"));
    assert!(!MetricBatchBuilder::is_valid_state_value(&"x".repeat(33)));
    assert!(!MetricBatchBuilder::is_valid_state_value("state:bulk")); // ':' rejected
}

#[test]
fn device_label_is_always_first() {
    let mut b = MetricBatchBuilder::new("solar-charger")
        .unwrap()
        .with_timestamp_ms(ts())
        .unwrap();
    b.gauge_with("m", &[("x", "1")], 2.0).unwrap();
    let p = &b.points()[0];
    assert_eq!(
        p.labels()[0],
        ("device".to_owned(), "solar-charger".to_owned())
    );
}

#[test]
fn device_validation() {
    assert!(MetricBatchBuilder::new("").is_err());
    assert!(MetricBatchBuilder::new("valid device ✓").is_ok());
    assert!(MetricBatchBuilder::new("bad\0device").is_err());
}

#[test]
fn timestamps_are_explicit() {
    let mut b = MetricBatchBuilder::new("d")
        .unwrap()
        .with_timestamp_ms(1_700_000_000_001)
        .unwrap();
    b.gauge("m", 1.0).unwrap();
    assert_eq!(b.points()[0].timestamp_ms(), 1_700_000_000_001);

    // now() sets a real timestamp.
    let b = MetricBatchBuilder::new("d").unwrap().now().unwrap();
    assert!(b.timestamp_ms().unwrap() > 1_700_000_000_000);

    // A builder without a timestamp reports None.
    assert_eq!(MetricBatchBuilder::new("d").unwrap().timestamp_ms(), None);
}

#[test]
fn kind_is_recorded() {
    let mut b = MetricBatchBuilder::new("d")
        .unwrap()
        .with_timestamp_ms(ts())
        .unwrap();
    b.gauge("g", 1.0).unwrap();
    b.counter("c_total", 1.0).unwrap();
    assert_eq!(b.points()[0].kind(), MetricKind::Gauge);
    assert_eq!(b.points()[1].kind(), MetricKind::Counter);
}
