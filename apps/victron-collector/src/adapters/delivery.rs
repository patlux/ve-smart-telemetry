//! Prometheus rendering and outbound VictoriaMetrics delivery adapters.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use victron_metrics::{FailureCategory, Outcome, SampleView, VictoriaMetricsClient};
use victron_service::{BatchRenderer, DeliveryError, MetricsDelivery, RenderContext, RenderError};

#[derive(Debug, Clone, Default)]
pub struct PrometheusRenderer;

impl BatchRenderer for PrometheusRenderer {
    fn render(&self, ctx: &RenderContext<'_>) -> Result<Vec<u8>, RenderError> {
        let health = &ctx.health;
        let view = SampleView {
            device: ctx.device.as_str(),
            observed_at_ms: 0,
            // The service resolved native-vs-integrated energy already. Do
            // not let the raw sample's candidate/native yield override it.
            yield_total_kwh: Some(ctx.resolved_yield_kwh),
            ble_up: ctx.ble_up,
            ble_rssi_dbm: ctx.ble_rssi_dbm,
            last_success_unixtime: ctx.last_success.map(system_time_seconds).transpose()?,
            sample_age_seconds: ctx.sample_age.map(|age| age.as_secs_f64()),
            ble_connect_failures: Some(health.ble_connect_failures_total),
            protocol_errors: Some(health.protocol_errors_total),
            samples_dropped: Some(health.samples_dropped_total),
            energy_integration_gap_seconds: Some(ctx.energy_gap_skipped_seconds),
            spool_batches: Some(ctx.spool_depth as u64),
            spool_oldest_age_seconds: ctx.spool_oldest_age.map(|age| age.as_secs_f64()),
            ..SampleView::default()
        };
        let batch = victron_metrics::domain::sample_to_batch(ctx.device.as_str(), ctx.sample, view)
            .map_err(metric_error)?;

        if batch.is_empty() {
            return Err(RenderError::Metric(
                "sample rendered no metrics".to_string(),
            ));
        }
        Ok(batch.encode().into_bytes())
    }
}

#[derive(Debug, Clone)]
pub struct VictoriaMetricsDelivery {
    client: VictoriaMetricsClient,
}

impl VictoriaMetricsDelivery {
    pub fn new(url: &str, request_timeout: Duration) -> Result<Self, DeliveryError> {
        let client = VictoriaMetricsClient::new(url)
            .map_err(|error| DeliveryError::Rejected(error.to_string()))?
            .with_timeouts(request_timeout, request_timeout);
        Ok(Self { client })
    }
}

#[async_trait]
impl MetricsDelivery for VictoriaMetricsDelivery {
    async fn deliver(&mut self, payload: &[u8]) -> Result<(), DeliveryError> {
        let body = std::str::from_utf8(payload)
            .map_err(|_| DeliveryError::Rejected("metrics payload is not UTF-8".into()))?;
        match self.client.import(body).await {
            Outcome::Success => Ok(()),
            Outcome::Retryable(failure) | Outcome::Permanent(failure) => Err(map_failure(failure)),
        }
    }
}

fn map_failure(failure: victron_metrics::ImportFailure) -> DeliveryError {
    match (failure.category, failure.status) {
        (FailureCategory::Network, _) => DeliveryError::Transport(failure.message),
        (FailureCategory::Timeout, _) => DeliveryError::Timeout,
        (FailureCategory::MalformedResponse, _) => {
            DeliveryError::MalformedResponse(failure.message)
        }
        (FailureCategory::Http, Some(status)) => DeliveryError::Http { status },
        (FailureCategory::Http, None) => DeliveryError::Rejected(failure.message),
    }
}

fn metric_error(error: victron_metrics::MetricError) -> RenderError {
    RenderError::Metric(error.to_string())
}

fn system_time_seconds(time: SystemTime) -> Result<i64, RenderError> {
    let duration = time
        .duration_since(UNIX_EPOCH)
        .map_err(|error| RenderError::Metric(error.to_string()))?;
    i64::try_from(duration.as_secs())
        .map_err(|_| RenderError::Metric("timestamp exceeds i64 seconds".into()))
}
