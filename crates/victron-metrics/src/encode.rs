//! Deterministic Prometheus text-format encoding.
//!
//! Output rules:
//!
//! - one sample per line: `name{label="value",...} value timestamp_ms\n`
//! - series are emitted in stable order, sorted by (name, encoded label set,
//!   timestamp)
//! - a true duplicate — the same series (name and labels) **and** the same
//!   timestamp — collapses to its **last** occurrence (last value wins);
//!   distinct timestamps for the same series are all emitted, so no sample is
//!   ever lost to collapsing
//! - label values are escaped per the Prometheus text format: `\` → `\\`,
//!   `"` → `\"`, newline → `\n` (other control bytes are rejected upstream)
//! - values use Rust's shortest round-trip `f64` formatting
//! - explicit millisecond timestamps
//!
//! # Never encoded
//!
//! As defense in depth beyond the validating [`crate::MetricPoint`]
//! constructors, samples with a non-finite value or a non-positive timestamp
//! are **skipped**: the wire format must never carry `NaN`/`±Inf` values or
//! invalid timestamps, even if a point somehow bypassed validation.

use crate::metric::MetricPoint;

/// Renders points as Prometheus text.
///
/// Samples with a non-finite value or a non-positive timestamp are skipped
/// (see the module docs).
pub fn encode(points: &[MetricPoint]) -> String {
    let mut out = String::new();
    encode_into(points, &mut out);
    out
}

/// Appends the Prometheus text rendering of `points` to `out`.
///
/// Samples with a non-finite value or a non-positive timestamp are skipped
/// (see the module docs).
pub fn encode_into(points: &[MetricPoint], out: &mut String) {
    let valid: Vec<&MetricPoint> = points
        .iter()
        .filter(|p| p.value().is_finite() && p.timestamp_ms() > 0)
        .collect();
    let mut items: Vec<&MetricPoint> = valid;
    // sort_by is stable, so equal keys keep insertion order; we then pick
    // the last occurrence of each exact (series, timestamp) identity
    // (last value wins for true duplicates only).
    items.sort_by(|a, b| {
        (a.name(), a.labels(), a.timestamp_ms()).cmp(&(b.name(), b.labels(), b.timestamp_ms()))
    });

    let mut i = 0;
    while i < items.len() {
        let mut j = i;
        while j + 1 < items.len()
            && items[j + 1].name() == items[i].name()
            && items[j + 1].labels() == items[i].labels()
            && items[j + 1].timestamp_ms() == items[i].timestamp_ms()
        {
            j += 1;
        }
        write_line(items[j], out);
        i = j + 1;
    }
}

fn write_line(p: &MetricPoint, out: &mut String) {
    out.push_str(p.name());
    out.push('{');
    for (i, (k, v)) in p.labels().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(k);
        out.push_str("=\"");
        escape_label_value_into(v, out);
        out.push('"');
    }
    out.push_str("} ");
    out.push_str(&format_value(p.value()));
    out.push(' ');
    out.push_str(&p.timestamp_ms().to_string());
    out.push('\n');
}

/// Escapes a label value per the Prometheus text format (`\`, `"`, newline).
/// Returns a new `String`.
pub fn escape_label_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    escape_label_value_into(value, &mut out);
    out
}

fn escape_label_value_into(value: &str, out: &mut String) {
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
}

/// Formats an `f64` for the wire: Rust's shortest round-trip representation
/// (e.g. `1.0` → `"1"`, `0.1` → `"0.1"`, `-0.0` → `"-0"`), which the
/// Prometheus/VictoriaMetrics parsers accept.
pub fn format_value(value: f64) -> String {
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MetricBatchBuilder, MetricKind, MetricPoint};

    fn ts() -> i64 {
        1_700_000_000_000
    }

    #[test]
    fn escapes_backslash_quote_newline() {
        assert_eq!(escape_label_value("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }

    #[test]
    fn format_value_is_shortest_round_trip() {
        assert_eq!(format_value(1.0), "1");
        assert_eq!(format_value(0.1), "0.1");
        assert_eq!(format_value(-0.0), "-0");
        assert_eq!(format_value(136.4), "136.4");
        // Rust's Display prints full digits up to 1e21.
        assert_eq!(format_value(1e21), "1000000000000000000000");
    }

    #[test]
    fn encode_sorts_series_deterministically() {
        let mut b = MetricBatchBuilder::new("d")
            .unwrap()
            .with_timestamp_ms(ts())
            .unwrap();
        b.gauge("z_metric", 1.0).unwrap();
        b.gauge("a_metric", 2.0).unwrap();
        b.gauge("m_metric", 3.0).unwrap();
        let out = b.encode();
        let mut lines: Vec<&str> = out.lines().collect();
        lines.sort();
        assert_eq!(
            lines,
            out.lines().collect::<Vec<_>>(),
            "output must be sorted"
        );
    }

    #[test]
    fn duplicate_series_last_wins() {
        let mut b = MetricBatchBuilder::new("d")
            .unwrap()
            .with_timestamp_ms(ts())
            .unwrap();
        b.gauge("m", 1.0).unwrap();
        b.gauge("m", 2.0).unwrap(); // same name + labels + ts
        let out = b.encode();
        assert_eq!(out, "m{device=\"d\"} 2 1700000000000\n");
    }

    #[test]
    fn same_series_different_timestamps_both_emitted() {
        // Two valid points for the same series at different timestamps are
        // distinct samples: both must be emitted, in deterministic order.
        let mut b = MetricBatchBuilder::new("d").unwrap();
        let p1 = MetricPoint::gauge("m", vec![("device".into(), "d".into())], 1.0, ts()).unwrap();
        let p2 =
            MetricPoint::gauge("m", vec![("device".into(), "d".into())], 2.0, ts() + 1000).unwrap();
        b.push_point(p1);
        b.push_point(p2);
        assert_eq!(
            b.encode(),
            "m{device=\"d\"} 1 1700000000000\nm{device=\"d\"} 2 1700000001000\n"
        );
    }

    #[test]
    fn same_series_different_timestamps_order_is_deterministic() {
        // Insertion order must not change the output: the series is sorted by
        // (name, labels, timestamp), so the earlier timestamp always comes
        // first.
        let mut b = MetricBatchBuilder::new("d").unwrap();
        let late =
            MetricPoint::gauge("m", vec![("device".into(), "d".into())], 2.0, ts() + 1000).unwrap();
        let early =
            MetricPoint::gauge("m", vec![("device".into(), "d".into())], 1.0, ts()).unwrap();
        b.push_point(late);
        b.push_point(early);
        assert_eq!(
            b.encode(),
            "m{device=\"d\"} 1 1700000000000\nm{device=\"d\"} 2 1700000001000\n"
        );
    }

    #[test]
    fn duplicate_collapse_is_scoped_to_exact_timestamp() {
        // Same series, same timestamp twice (last wins) plus a third point at
        // a different timestamp: the different-timestamp sample survives.
        let mut b = MetricBatchBuilder::new("d").unwrap();
        let a = MetricPoint::gauge("m", vec![("device".into(), "d".into())], 1.0, ts()).unwrap();
        let b2 = MetricPoint::gauge("m", vec![("device".into(), "d".into())], 9.0, ts()).unwrap();
        let c =
            MetricPoint::gauge("m", vec![("device".into(), "d".into())], 3.0, ts() + 2000).unwrap();
        b.push_point(a);
        b.push_point(b2);
        b.push_point(c);
        assert_eq!(
            b.encode(),
            "m{device=\"d\"} 9 1700000000000\nm{device=\"d\"} 3 1700000002000\n"
        );
    }

    #[test]
    fn structural_series_identity_prevents_ambiguous_label_collisions() {
        let points = [
            MetricPoint::gauge("m", vec![("a".into(), "x,b=y".into())], 1.0, ts()).unwrap(),
            MetricPoint::gauge(
                "m",
                vec![("a".into(), "x".into()), ("b".into(), "y".into())],
                2.0,
                ts(),
            )
            .unwrap(),
        ];
        assert_eq!(
            encode(&points),
            "m{a=\"x\",b=\"y\"} 2 1700000000000\nm{a=\"x,b=y\"} 1 1700000000000\n"
        );
    }

    #[test]
    fn direct_point_with_own_timestamp_is_respected() {
        let p = MetricPoint::new("m", vec![], 5.0, 42, MetricKind::Gauge).unwrap();
        let out = encode(std::slice::from_ref(&p));
        assert_eq!(out, "m{} 5 42\n");
    }
}
