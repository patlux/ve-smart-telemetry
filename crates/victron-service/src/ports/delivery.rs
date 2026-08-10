//! Metrics rendering + VictoriaMetrics delivery ports.
//!
//! The concrete implementation lives in `victron-metrics` (Prometheus text
//! generation + HTTP import client). The service treats payloads as opaque
//! bytes and coordinates ownership.
//!
//! # Truthful render context
//!
//! The renderer receives an explicit [`RenderContext`] carrying the canonical
//! domain [`Sample`], the resolved native/integrated yield, BLE link state and
//! RSSI *as actually known* (never synthesized), the projected current
//! successful-sample timestamp, the sample age, the health counters, the
//! projected spool health, and the cumulative skipped energy-gap seconds.
//! Unknown health is represented as `None` and must be omitted by the
//! renderer — never rendered as a known zero.

use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use victron_domain::{DeviceId, Sample};

use crate::energy::EnergyKind;
use crate::health::HealthSnapshot;

/// Everything the renderer needs to produce one truthful batch.
#[derive(Debug, Clone)]
pub struct RenderContext<'a> {
    /// Canonical validated device identity (the `device` label).
    pub device: &'a DeviceId,
    /// The canonical domain sample being committed.
    pub sample: &'a Sample,
    /// Resolved cumulative kWh (confirmed native or integrated).
    pub resolved_yield_kwh: f64,
    /// How the resolved yield was produced.
    pub energy_kind: EnergyKind,
    /// BLE link up, as actually known (`None` = unknown, omit the series).
    pub ble_up: Option<bool>,
    /// BLE RSSI in dBm, as actually known (`None` = unknown, omit).
    pub ble_rssi_dbm: Option<i32>,
    /// Projected timestamp of the current successful acquisition: the sample
    /// being committed is a success, so this is its `observed_at`.
    pub last_success: Option<SystemTime>,
    /// Age of this sample at render time (`None` when the clock moved
    /// backwards relative to `observed_at`).
    pub sample_age: Option<Duration>,
    /// Cumulative health counters (pre-commit view; the current success is
    /// reflected via `last_success`/`sample_age`).
    pub health: &'a HealthSnapshot,
    /// Projected spool depth after the newly enqueued batch.
    pub spool_depth: usize,
    /// Projected age of the oldest spooled batch after the enqueue.
    pub spool_oldest_age: Option<Duration>,
    /// Cumulative seconds skipped by local energy integration (gaps that were
    /// never silently bridged), including this cycle's gap.
    pub energy_gap_skipped_seconds: u64,
}

/// Render one acquisition cycle into a Prometheus text batch with explicit
/// timestamps.
pub trait BatchRenderer: Send + Sync {
    fn render(&self, ctx: &RenderContext<'_>) -> Result<Vec<u8>, RenderError>;
}

/// Delivery failure. `retryable()` drives spool retry vs. bounded drop and
/// matches the `victron-metrics` classification: network, timeout, malformed
/// response, HTTP 408/429 and 5xx are retryable; other 4xx are permanent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeliveryError {
    #[error("delivery transport failed: {0}")]
    Transport(String),
    #[error("delivery timed out")]
    Timeout,
    #[error("delivery rejected with HTTP {status}")]
    Http { status: u16 },
    #[error("malformed import response: {0}")]
    MalformedResponse(String),
    #[error("delivery permanently rejected: {0}")]
    Rejected(String),
    #[error("not wired: {0}")]
    NotWired(&'static str),
}

impl DeliveryError {
    /// Stable, response-body-free classification for structured logs.
    pub fn kind(&self) -> &'static str {
        match self {
            DeliveryError::Transport(_) => "transport",
            DeliveryError::Timeout => "timeout",
            DeliveryError::Http { .. } => "http",
            DeliveryError::MalformedResponse(_) => "malformed_response",
            DeliveryError::Rejected(_) => "rejected",
            DeliveryError::NotWired(_) => "not_wired",
        }
    }

    /// HTTP status when the peer returned one.
    pub fn status(&self) -> Option<u16> {
        match self {
            DeliveryError::Http { status } => Some(*status),
            _ => None,
        }
    }

    /// Retry classification aligned with `victron-metrics::Outcome`:
    /// network/timeout/malformed-response/408/429/5xx retry; other 4xx and
    /// configuration errors are permanent.
    pub fn retryable(&self) -> bool {
        match self {
            DeliveryError::Transport(_) | DeliveryError::Timeout => true,
            DeliveryError::Http { status } => matches!(status, 408 | 429 | 500..=599),
            DeliveryError::MalformedResponse(_) => true,
            DeliveryError::Rejected(_) | DeliveryError::NotWired(_) => false,
        }
    }
}

/// HTTP import client for one rendered batch.
#[async_trait]
pub trait MetricsDelivery: Send {
    /// POST one batch to the VictoriaMetrics import endpoint. `Ok` means
    /// durably accepted; the caller then completes the spool claim.
    async fn deliver(&mut self, payload: &[u8]) -> Result<(), DeliveryError>;
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    #[error("render failed: {0}")]
    Metric(String),
    #[error("not wired: {0}")]
    NotWired(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_classification_matches_metrics_contract() {
        // Table-driven: network, timeout, malformed response, HTTP 408/429
        // and 5xx retry; other 4xx and config errors are permanent.
        let cases: &[(DeliveryError, bool)] = &[
            (DeliveryError::Transport("connect refused".into()), true),
            (DeliveryError::Timeout, true),
            (
                DeliveryError::MalformedResponse("bad status line".into()),
                true,
            ),
            (DeliveryError::Http { status: 408 }, true),
            (DeliveryError::Http { status: 429 }, true),
            (DeliveryError::Http { status: 500 }, true),
            (DeliveryError::Http { status: 502 }, true),
            (DeliveryError::Http { status: 503 }, true),
            (DeliveryError::Http { status: 599 }, true),
            (DeliveryError::Http { status: 400 }, false),
            (DeliveryError::Http { status: 401 }, false),
            (DeliveryError::Http { status: 403 }, false),
            (DeliveryError::Http { status: 404 }, false),
            (DeliveryError::Http { status: 422 }, false),
            (DeliveryError::Http { status: 499 }, false),
            (DeliveryError::Rejected("bad payload".into()), false),
            (DeliveryError::NotWired("pending"), false),
        ];
        for (err, expected) in cases {
            assert_eq!(err.retryable(), *expected, "{err:?}");
        }
    }

    #[test]
    fn render_context_is_cloneable_for_tests() {
        let device = DeviceId::new("solar-charger").unwrap();
        let sample = Sample::builder_now(device.clone()).build();
        let health = HealthSnapshot::default();
        let ctx = RenderContext {
            device: &device,
            sample: &sample,
            resolved_yield_kwh: 1.0,
            energy_kind: EnergyKind::Native,
            ble_up: Some(true),
            ble_rssi_dbm: Some(-61),
            last_success: Some(sample.observed_at()),
            sample_age: Some(Duration::from_secs(3)),
            health: &health,
            spool_depth: 1,
            spool_oldest_age: Some(Duration::ZERO),
            energy_gap_skipped_seconds: 0,
        };
        let _ = ctx.clone();
    }
}
