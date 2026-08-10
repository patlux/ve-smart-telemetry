//! Error type for metric construction and batch building.

use std::fmt;

use crate::metric::MAX_LABELS_PER_SERIES;

/// Errors produced by metric construction and batch building.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricError {
    /// Metric name is empty.
    EmptyName,
    /// Metric name exceeds 128 bytes.
    NameTooLong(usize),
    /// Metric name contains invalid characters.
    InvalidName(String),
    /// Metric name uses the reserved `__` prefix.
    ReservedName(String),
    /// A counter's name must end in `_total`.
    CounterNameMissingSuffix(String),
    /// Label name is empty.
    EmptyLabelName,
    /// Label name exceeds 64 bytes.
    LabelNameTooLong(usize),
    /// Label name contains invalid characters.
    InvalidLabelName(String),
    /// Label name uses the reserved `__` prefix.
    ReservedLabelName(String),
    /// A label value is empty (only the `device` label disallows this).
    EmptyLabelValue,
    /// Label value exceeds 128 bytes.
    LabelValueTooLong(usize),
    /// Label value contains an unescapable control byte.
    InvalidLabelValue(String),
    /// A state label value violates the bounded `[a-z0-9_]` rule.
    InvalidStateLabel(String),
    /// More than [`MAX_LABELS_PER_SERIES`] labels on one series.
    TooManyLabels(usize),
    /// Duplicate label name on one series.
    DuplicateLabel(String),
    /// A metric value must be finite; NaN and infinities are never encoded.
    NonFiniteValue,
    /// A timestamp must be strictly positive Unix milliseconds.
    InvalidTimestamp(i64),
    /// A batch helper was called before a timestamp was set.
    TimestampNotSet,
    /// Could not convert a timestamp (clock before the Unix epoch).
    TimestampError(String),
}

impl fmt::Display for MetricError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MetricError::EmptyName => write!(f, "metric name is empty"),
            MetricError::NameTooLong(n) => write!(f, "metric name exceeds 128 bytes (got {n})"),
            MetricError::InvalidName(name) => write!(f, "invalid metric name {name:?}"),
            MetricError::ReservedName(name) => {
                write!(f, "metric name {name:?} uses reserved __ prefix")
            }
            MetricError::CounterNameMissingSuffix(name) => {
                write!(f, "counter {name:?} must end in _total")
            }
            MetricError::EmptyLabelName => write!(f, "label name is empty"),
            MetricError::LabelNameTooLong(n) => write!(f, "label name exceeds 64 bytes (got {n})"),
            MetricError::InvalidLabelName(name) => write!(f, "invalid label name {name:?}"),
            MetricError::ReservedLabelName(name) => {
                write!(f, "label name {name:?} uses reserved __ prefix")
            }
            MetricError::LabelValueTooLong(n) => {
                write!(f, "label value exceeds 128 bytes (got {n})")
            }
            MetricError::EmptyLabelValue => write!(f, "label value is empty"),
            MetricError::InvalidLabelValue(value) => {
                write!(f, "label value contains a control byte: {value:?}")
            }
            MetricError::InvalidStateLabel(value) => write!(
                f,
                "state label value {value:?} violates bounded [a-z0-9_] rule (max 32 bytes)"
            ),
            MetricError::TooManyLabels(n) => write!(
                f,
                "more than {MAX_LABELS_PER_SERIES} labels on one series (got {n})"
            ),
            MetricError::DuplicateLabel(name) => {
                write!(f, "duplicate label {name:?} on one series")
            }
            MetricError::NonFiniteValue => {
                write!(
                    f,
                    "metric value must be finite (NaN and infinities are never encoded)"
                )
            }
            MetricError::InvalidTimestamp(ts) => write!(
                f,
                "timestamp must be strictly positive Unix milliseconds (got {ts})"
            ),
            MetricError::TimestampNotSet => write!(
                f,
                "batch timestamp not set: call with_timestamp_ms or now before adding points"
            ),
            MetricError::TimestampError(e) => write!(f, "timestamp conversion failed: {e}"),
        }
    }
}

impl std::error::Error for MetricError {}
