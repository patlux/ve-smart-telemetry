use super::*;

const TS: i64 = 1_700_000_000_000;

fn ts() -> i64 {
    TS
}

/// Constructs a point bypassing the validating constructor. Only used to
/// prove the encoder's defensive filtering (a public API path to such a
/// point does not exist).
fn raw_point(name: &str, value: f64, timestamp_ms: i64) -> MetricPoint {
    MetricPoint {
        name: MetricName::new(name).unwrap(),
        labels: Vec::new(),
        value,
        timestamp_ms,
        kind: MetricKind::Gauge,
    }
}

#[test]
fn construction_rejects_non_finite_values() {
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            MetricPoint::new("m", Vec::new(), bad, ts(), MetricKind::Gauge),
            Err(MetricError::NonFiniteValue)
        );
        assert_eq!(
            MetricPoint::gauge("m", Vec::new(), bad, ts()),
            Err(MetricError::NonFiniteValue)
        );
        assert_eq!(
            MetricPoint::counter("c_total", Vec::new(), bad, ts()),
            Err(MetricError::NonFiniteValue)
        );
    }
}

#[test]
fn construction_rejects_non_positive_timestamps() {
    for bad in [0i64, -1, -1_700_000_000_000] {
        assert_eq!(
            MetricPoint::new("m", Vec::new(), 1.0, bad, MetricKind::Gauge),
            Err(MetricError::InvalidTimestamp(bad))
        );
    }
}

#[test]
fn builder_requires_a_positive_timestamp() {
    // Helpers refuse to run before a timestamp is set.
    let mut b = MetricBatchBuilder::new("d").unwrap();
    assert_eq!(b.timestamp_ms(), None);
    assert_eq!(b.gauge("m", 1.0), Err(MetricError::TimestampNotSet));
    assert_eq!(
        b.state("victron_charger_state", "bulk"),
        Err(MetricError::TimestampNotSet)
    );
    assert!(b.is_empty());

    // with_timestamp_ms validates strictly positive.
    assert_eq!(
        MetricBatchBuilder::new("d")
            .unwrap()
            .with_timestamp_ms(0)
            .unwrap_err(),
        MetricError::InvalidTimestamp(0)
    );
    assert_eq!(
        MetricBatchBuilder::new("d")
            .unwrap()
            .with_timestamp_ms(-1)
            .unwrap_err(),
        MetricError::InvalidTimestamp(-1)
    );
    assert!(MetricBatchBuilder::new("d")
        .unwrap()
        .with_timestamp_ms(1)
        .is_ok());
}

#[test]
fn push_point_accepts_pre_validated_points() {
    let mut b = MetricBatchBuilder::new("d").unwrap();
    let p = MetricPoint::gauge("m", Vec::new(), 1.5, ts()).unwrap();
    b.push_point(p);
    assert_eq!(b.len(), 1);
    assert_eq!(b.encode(), "m{} 1.5 1700000000000\n");
}

#[test]
fn encoder_defensively_omits_invalid_samples() {
    // Defense in depth: even a point that bypasses the constructor (only
    // possible through private fields) is never encoded.
    let nan = raw_point("m", f64::NAN, ts());
    assert_eq!(crate::encode::encode(&[nan]), "");

    let inf = raw_point("m", f64::INFINITY, ts());
    assert_eq!(crate::encode::encode(&[inf]), "");

    let zero_ts = raw_point("m", 1.0, 0);
    assert_eq!(crate::encode::encode(&[zero_ts]), "");

    let negative_ts = raw_point("m", 1.0, -5);
    assert_eq!(crate::encode::encode(&[negative_ts]), "");

    // Valid points still pass through.
    let ok = raw_point("m", 1.0, ts());
    assert_eq!(crate::encode::encode(&[ok]), "m{} 1 1700000000000\n");
}
