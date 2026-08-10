//! Metric model ([`MetricName`], [`MetricPoint`]) and the deterministic batch
//! builder ([`MetricBatchBuilder`]).
//!
//! Cardinality and validity rules are enforced here:
//!
//! - metric names: `[a-zA-Z_:][a-zA-Z0-9_:]*`, max 128 bytes, no `__` prefix
//! - label names: `[a-zA-Z_][a-zA-Z0-9_]*`, max 64 bytes, no `__` prefix
//! - label values: max 128 bytes, no control bytes except `\n` (escaped on
//!   output), `\r`/NUL/tab rejected
//! - labels per series: at most [`MAX_LABELS_PER_SERIES`], no duplicates
//! - counters: name must end in `_total`
//! - state label values: `[a-z0-9_]`, max 32 bytes (bounded by construction)
//! - values: must be finite (NaN and ±Inf are rejected at [`MetricPoint`]
//!   construction and, as defense in depth, omitted by the encoder)
//! - timestamps: strictly positive Unix milliseconds (0 and negative values
//!   are rejected at construction and, as defense in depth, omitted by the
//!   encoder)
//!
//! The ergonomic builder helpers (`gauge`, `counter`, `state`) *omit*
//! non-finite values by returning `Ok(false)` — a convenience for collectors
//! that treat NaN as "no data". Direct [`MetricPoint`] construction is strict
//! and rejects non-finite values and non-positive timestamps with an `Err`.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::MetricError;

/// Upper bound on the number of labels per series (including `device`).
pub const MAX_LABELS_PER_SERIES: usize = 8;

/// Metric kind. Does not change the text encoding (the Prometheus text format
/// carries no type information) but documents intent and enables validation:
/// counters are cumulative and their name must end in `_total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    /// A gauge: instantaneous value that can go up and down.
    Gauge,
    /// A counter: cumulative, monotonic value (name must end in `_total`).
    Counter,
}

impl MetricKind {
    /// Returns true for [`MetricKind::Counter`].
    pub fn is_counter(self) -> bool {
        matches!(self, MetricKind::Counter)
    }
}

/// Validated Prometheus metric name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MetricName(String);

impl MetricName {
    /// Validates and wraps a metric name.
    pub fn new(name: &str) -> Result<Self, MetricError> {
        validate_metric_name(name)?;
        Ok(MetricName(name.to_owned()))
    }

    /// The name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MetricName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A single Prometheus sample with an explicit millisecond timestamp.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricPoint {
    name: MetricName,
    /// Label pairs, validated and sorted by name for deterministic output.
    labels: Vec<(String, String)>,
    value: f64,
    timestamp_ms: i64,
    kind: MetricKind,
}

impl MetricPoint {
    /// Constructs and validates a point.
    ///
    /// # Errors
    ///
    /// - [`MetricError::NonFiniteValue`] when `value` is NaN or infinite:
    ///   the wire format must never carry non-finite samples.
    /// - [`MetricError::InvalidTimestamp`] when `timestamp_ms <= 0`: sample
    ///   timestamps are strictly positive Unix milliseconds.
    /// - the label/name/counter errors documented on [`MetricName`] and
    ///   [`Self::new`]'s validation.
    pub fn new(
        name: &str,
        labels: Vec<(String, String)>,
        value: f64,
        timestamp_ms: i64,
        kind: MetricKind,
    ) -> Result<Self, MetricError> {
        if !value.is_finite() {
            return Err(MetricError::NonFiniteValue);
        }
        if timestamp_ms <= 0 {
            return Err(MetricError::InvalidTimestamp(timestamp_ms));
        }
        let name = MetricName::new(name)?;
        if kind == MetricKind::Counter && !name.as_str().ends_with("_total") {
            return Err(MetricError::CounterNameMissingSuffix(name.to_string()));
        }
        let labels = validate_labels(labels)?;
        Ok(MetricPoint {
            name,
            labels,
            value,
            timestamp_ms,
            kind,
        })
    }

    /// Convenience constructor for a gauge point.
    pub fn gauge(
        name: &str,
        labels: Vec<(String, String)>,
        value: f64,
        timestamp_ms: i64,
    ) -> Result<Self, MetricError> {
        Self::new(name, labels, value, timestamp_ms, MetricKind::Gauge)
    }

    /// Convenience constructor for a counter point.
    pub fn counter(
        name: &str,
        labels: Vec<(String, String)>,
        value: f64,
        timestamp_ms: i64,
    ) -> Result<Self, MetricError> {
        Self::new(name, labels, value, timestamp_ms, MetricKind::Counter)
    }

    /// Metric name.
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// Validated label pairs (sorted by label name).
    pub fn labels(&self) -> &[(String, String)] {
        &self.labels
    }

    /// The sample value. Finite by construction (non-finite values are
    /// rejected in [`Self::new`]).
    pub fn value(&self) -> f64 {
        self.value
    }

    /// Explicit millisecond timestamp. Strictly positive by construction.
    pub fn timestamp_ms(&self) -> i64 {
        self.timestamp_ms
    }

    /// Metric kind.
    pub fn kind(&self) -> MetricKind {
        self.kind
    }
}

/// A builder producing one timestamped batch of [`MetricPoint`]s with the
/// `device` label applied to every series.
///
/// A timestamp must be set before any point can be added: call
/// [`MetricBatchBuilder::with_timestamp_ms`] (explicit, strict positive) or
/// [`MetricBatchBuilder::now`] (current wall clock). Until then, the ergonomic
/// helpers return [`MetricError::TimestampNotSet`] and `encode()` renders an
/// empty string.
///
/// Values that are NaN or infinite are omitted: the fallible helpers return
/// `Ok(false)` in that case and add nothing. Label/name/timestamp violations
/// return `Err`.
#[derive(Debug, Clone)]
pub struct MetricBatchBuilder {
    device: String,
    timestamp_ms: Option<i64>,
    points: Vec<MetricPoint>,
}

impl MetricBatchBuilder {
    /// Creates an empty builder. `device` must be non-empty and satisfy
    /// label-value bounds (≤128 bytes, no control bytes).
    ///
    /// The batch timestamp is unset until [`Self::with_timestamp_ms`] or
    /// [`Self::now`] is called; the ergonomic helpers require it.
    pub fn new(device: impl AsRef<str>) -> Result<Self, MetricError> {
        let device = device.as_ref();
        if device.is_empty() {
            return Err(MetricError::EmptyLabelValue);
        }
        validate_label_value(device)?;
        Ok(MetricBatchBuilder {
            device: device.to_owned(),
            timestamp_ms: None,
            points: Vec::new(),
        })
    }

    /// Sets the explicit millisecond timestamp applied by the ergonomic
    /// helpers.
    ///
    /// # Errors
    ///
    /// Returns [`MetricError::InvalidTimestamp`] when `timestamp_ms <= 0`:
    /// production sample timestamps must be strictly positive Unix
    /// milliseconds, so invalid state can never be stored.
    pub fn with_timestamp_ms(mut self, timestamp_ms: i64) -> Result<Self, MetricError> {
        if timestamp_ms <= 0 {
            return Err(MetricError::InvalidTimestamp(timestamp_ms));
        }
        self.timestamp_ms = Some(timestamp_ms);
        Ok(self)
    }

    /// Sets the timestamp to the current wall-clock time in milliseconds.
    pub fn now(mut self) -> Result<Self, MetricError> {
        self.timestamp_ms = Some(system_time_ms()?);
        Ok(self)
    }

    /// The configured device label value.
    pub fn device(&self) -> &str {
        &self.device
    }

    /// The batch timestamp in milliseconds, or `None` when unset.
    pub fn timestamp_ms(&self) -> Option<i64> {
        self.timestamp_ms
    }

    /// Number of points in the batch.
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// True when the batch contains no points.
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Removes all points (keeps device and timestamp).
    pub fn clear(&mut self) {
        self.points.clear();
    }

    /// The points added so far, in insertion order.
    pub fn points(&self) -> &[MetricPoint] {
        &self.points
    }

    /// Appends a pre-validated point. The point keeps its own timestamp and
    /// its value is finite by construction ([`MetricPoint::new`] rejects
    /// non-finite values).
    pub fn push_point(&mut self, point: MetricPoint) {
        debug_assert!(point.value().is_finite());
        debug_assert!(point.timestamp_ms() > 0);
        self.points.push(point);
    }

    /// Adds a gauge with only the `device` label.
    pub fn gauge(&mut self, name: &str, value: f64) -> Result<bool, MetricError> {
        self.add(name, &[], value, MetricKind::Gauge)
    }

    /// Adds a gauge with extra labels on top of `device`.
    pub fn gauge_with(
        &mut self,
        name: &str,
        labels: &[(&str, &str)],
        value: f64,
    ) -> Result<bool, MetricError> {
        self.add(name, labels, value, MetricKind::Gauge)
    }

    /// Adds a counter (name must end in `_total`) with only the `device` label.
    /// The caller supplies the cumulative value.
    pub fn counter(&mut self, name: &str, value: f64) -> Result<bool, MetricError> {
        self.add(name, &[], value, MetricKind::Counter)
    }

    /// Adds a counter with extra labels on top of `device`.
    pub fn counter_with(
        &mut self,
        name: &str,
        labels: &[(&str, &str)],
        value: f64,
    ) -> Result<bool, MetricError> {
        self.add(name, labels, value, MetricKind::Counter)
    }

    /// Adds a bounded state indicator: `name{device="...",state="<state>"} 1`.
    /// `state` must match `[a-z0-9_]`, be non-empty, and be ≤32 bytes.
    pub fn state(&mut self, name: &str, state: &str) -> Result<bool, MetricError> {
        if !Self::is_valid_state_value(state) {
            return Err(MetricError::InvalidStateLabel(state.to_owned()));
        }
        self.add(
            name,
            &[(crate::names::STATE_LABEL, state)],
            1.0,
            MetricKind::Gauge,
        )
    }

    /// True when `state` is an acceptable bounded state label value:
    /// non-empty, ≤32 bytes, characters in `[a-z0-9_]`.
    pub fn is_valid_state_value(state: &str) -> bool {
        !state.is_empty()
            && state.len() <= 32
            && state
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    }

    /// Adds a point with the builder's timestamp. Non-finite values are
    /// omitted (`Ok(false)`); a missing batch timestamp is an error
    /// ([`MetricError::TimestampNotSet`]).
    fn add(
        &mut self,
        name: &str,
        extra: &[(&str, &str)],
        value: f64,
        kind: MetricKind,
    ) -> Result<bool, MetricError> {
        if !value.is_finite() {
            return Ok(false);
        }
        let timestamp_ms = self.timestamp_ms.ok_or(MetricError::TimestampNotSet)?;
        let mut labels = Vec::with_capacity(extra.len() + 1);
        labels.push((crate::names::DEVICE_LABEL.to_owned(), self.device.clone()));
        for (k, v) in extra {
            labels.push((k.to_string(), v.to_string()));
        }
        let point = MetricPoint::new(name, labels, value, timestamp_ms, kind)?;
        self.points.push(point);
        Ok(true)
    }

    /// Renders the batch as deterministic Prometheus text (series sorted by
    /// name, labels, and timestamp; true duplicates — same series and
    /// timestamp — collapsed to the last occurrence).
    pub fn encode(&self) -> String {
        crate::encode::encode(&self.points)
    }

    /// Appends the deterministic Prometheus text rendering to `out`.
    pub fn encode_into(&self, out: &mut String) {
        crate::encode::encode_into(&self.points, out);
    }
}

/// Milliseconds since the Unix epoch for the current wall clock.
pub(crate) fn system_time_ms() -> Result<i64, MetricError> {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| MetricError::TimestampError(e.to_string()))?;
    Ok(d.as_millis() as i64)
}

/// Converts a `SystemTime` to milliseconds since the Unix epoch. Used by the
/// `domain` integration adapter.
#[cfg_attr(not(feature = "domain"), allow(dead_code))]
pub(crate) fn system_time_to_ms(t: SystemTime) -> Result<i64, MetricError> {
    let d = t
        .duration_since(UNIX_EPOCH)
        .map_err(|e| MetricError::TimestampError(e.to_string()))?;
    Ok(d.as_millis() as i64)
}

fn validate_metric_name(name: &str) -> Result<(), MetricError> {
    if name.is_empty() {
        return Err(MetricError::EmptyName);
    }
    if name.len() > 128 {
        return Err(MetricError::NameTooLong(name.len()));
    }
    let mut chars = name.chars();
    let first = chars.next().expect("non-empty checked above");
    if !(first.is_ascii_alphabetic() || first == '_' || first == ':') {
        return Err(MetricError::InvalidName(name.to_owned()));
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_' || c == ':') {
            return Err(MetricError::InvalidName(name.to_owned()));
        }
    }
    if name.starts_with("__") {
        return Err(MetricError::ReservedName(name.to_owned()));
    }
    Ok(())
}

fn validate_label_name(name: &str) -> Result<(), MetricError> {
    if name.is_empty() {
        return Err(MetricError::EmptyLabelName);
    }
    if name.len() > 64 {
        return Err(MetricError::LabelNameTooLong(name.len()));
    }
    let mut chars = name.chars();
    let first = chars.next().expect("non-empty checked above");
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(MetricError::InvalidLabelName(name.to_owned()));
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return Err(MetricError::InvalidLabelName(name.to_owned()));
        }
    }
    if name.starts_with("__") {
        return Err(MetricError::ReservedLabelName(name.to_owned()));
    }
    Ok(())
}

/// Validates a label value: bounded length, no control bytes except `\n`
/// (which the encoder escapes as `\n`).
pub(crate) fn validate_label_value(value: &str) -> Result<(), MetricError> {
    if value.len() > 128 {
        return Err(MetricError::LabelValueTooLong(value.len()));
    }
    for b in value.bytes() {
        if (b < 0x20 && b != b'\n') || b == 0x7f {
            return Err(MetricError::InvalidLabelValue(value.to_owned()));
        }
    }
    Ok(())
}

fn validate_labels(labels: Vec<(String, String)>) -> Result<Vec<(String, String)>, MetricError> {
    if labels.len() > MAX_LABELS_PER_SERIES {
        return Err(MetricError::TooManyLabels(labels.len()));
    }
    let mut seen = Vec::with_capacity(labels.len());
    for (k, v) in &labels {
        validate_label_name(k)?;
        validate_label_value(v)?;
        if seen.iter().any(|s: &String| s == k) {
            return Err(MetricError::DuplicateLabel(k.clone()));
        }
        seen.push(k.clone());
    }
    let mut labels = labels;
    labels.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(labels)
}

#[cfg(test)]
#[path = "metric_tests.rs"]
mod tests;
