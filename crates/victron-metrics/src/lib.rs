//! `victron-metrics`: Prometheus text encoding and VictoriaMetrics import
//! client for the Victron BLE collector.
//!
//! Part of the Rust multi-crate collector planned in
//! `analysis/rust-multicrate-collector-plan.md`. This crate is
//! hardware-independent and does **not** touch BLE, storage, or the network
//! domain model. It owns two responsibilities:
//!
//! 1. Deterministic Prometheus text encoding (stable names, escaped and
//!    low-cardinality labels, explicit millisecond timestamps, omission of
//!    non-finite values).
//! 2. A small async HTTP/1.1 client that POSTs batches to the VictoriaMetrics
//!    import endpoint and classifies the outcome as success, retryable, or
//!    permanent failure.
//!
//! # Footprint (Raspberry Pi Zero W / ARMv6)
//!
//! The endpoint is an internal plaintext HTTP URL
//! (`http://100.64.0.2:8429/api/v1/import/prometheus`), so TLS and the
//! entire reqwest/hyper/rustls/native-tls dependency tree are deliberately
//! avoided. The HTTP client is implemented directly on `tokio::net::TcpStream`
//! (HTTP/1.1, `Connection: close`, bounded error bodies). The only runtime
//! dependency is `tokio` with `net`/`time`/`io-util` features. The crate
//! never binds a listener and opens outbound connections only.
//!
//! # Example
//!
//! ```
//! use victron_metrics::MetricBatchBuilder;
//!
//! let mut batch = MetricBatchBuilder::new("solar-charger")
//!     .unwrap()
//!     .with_timestamp_ms(1_700_000_000_123)
//!     .unwrap();
//!
//! batch.gauge(victron_metrics::names::PV_POWER_WATTS, 136.4).unwrap();
//! batch.state(victron_metrics::names::CHARGER_STATE, victron_metrics::names::states::BULK).unwrap();
//! batch.gauge(victron_metrics::names::BLE_UP, 1.0).unwrap();
//!
//! let text = batch.encode();
//! assert!(text.contains("victron_pv_power_watts{device=\"solar-charger\"} 136.4 1700000000123\n"));
//! assert!(text.contains("victron_charger_state{device=\"solar-charger\",state=\"bulk\"} 1 1700000000123\n"));
//! ```
//!
//! # Integration seams
//!
//! - [`MetricBatchBuilder`] / [`MetricPoint`]: the small public API any
//!   collector code can use directly. Points must have finite values and
//!   strictly positive millisecond timestamps; the ergonomic builder helpers
//!   additionally omit non-finite values (`Ok(false)`).
//! - [`adapter::SampleView`]: a plain-data shim (`device`, base-unit values,
//!   charger state, health fields) with a `TryFrom` conversion into a
//!   [`MetricBatchBuilder`]. Health fields are `Option`: `Some` (including a
//!   known zero) emits the series, `None` (unknown) omits it, so unknown
//!   health is never rendered as a known zero. Fully testable without the
//!   domain crate.
//! - `domain` feature → the `domain` module: fills the measurement fields of
//!   a caller-supplied [`adapter::SampleView`] from `victron_domain::Sample`
//!   through the real domain accessors; the caller's view already carries the
//!   real health context, so mapping a domain sample never fabricates health
//!   series (see `src/domain.rs`).
//!
//! # Metric contract
//!
//! Names, units, and label rules are pinned in [`names`] and documented in
//! README.md. Low cardinality is enforced structurally: label names/values are
//! bounded in length, label count per series is capped, `__`-prefixed names
//! are rejected, and the `device` label is always set by the builder.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod adapter;
#[cfg(feature = "domain")]
pub mod domain;
pub mod encode;
pub mod error;
pub mod metric;
pub mod names;
mod response;
pub mod victoria_metrics;

pub use crate::adapter::SampleView;
pub use crate::encode::{encode, encode_into, escape_label_value, format_value};
pub use crate::error::MetricError;
pub use crate::metric::{MetricBatchBuilder, MetricKind, MetricName, MetricPoint};
pub use crate::victoria_metrics::{
    ClientConfigError, FailureCategory, ImportFailure, Outcome, VictoriaMetricsClient,
};
