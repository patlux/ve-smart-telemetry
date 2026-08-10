//! VE.Smart session adapter over the concrete BlueZ byte transport.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use victron_bluez::{BleError as TransportError, BleErrorClass, BleTransport, NotificationSource};
use victron_protocol::control::{ControlInfo, ControlMessage};
use victron_protocol::{OutboundTarget, Reassembler};
use victron_service::{BleError, BleSession};

use super::ble_flow::{
    map_protocol_error, map_reassembly_error, payload_has_values, ReceiveCredit,
};
const SUBSCRIBE_DRAIN_QUIET: Duration = Duration::from_millis(500);
const SUBSCRIBE_DRAIN_BUDGET: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
struct NotificationCounts {
    control: u32,
    data: u32,
    last_data: u32,
    clear_buffer: u32,
    completed_payloads: u32,
}

impl NotificationCounts {
    fn total(&self) -> u32 {
        self.control
            .saturating_add(self.data)
            .saturating_add(self.last_data)
    }
}

pub struct VeSmartBleSession {
    transport: victron_bluez::BluezTransport,
    opened: bool,
    negotiated: bool,
    subscribed_instance: Option<u16>,
    pending: Reassembler,
    receive_credit: ReceiveCredit,
    fallback_values: Option<Vec<u8>>,
    reusable: bool,
}

impl VeSmartBleSession {
    pub fn new(config: victron_bluez::TransportConfig) -> Self {
        Self {
            transport: victron_bluez::BluezTransport::new(config),
            opened: false,
            negotiated: false,
            subscribed_instance: None,
            pending: Reassembler::new(),
            receive_credit: ReceiveCredit::default(),
            fallback_values: None,
            reusable: false,
        }
    }

    async fn write_request(&mut self, payload: &[u8]) -> Result<(), BleError> {
        for chunk in victron_protocol::split_request(payload, 20).map_err(map_protocol_error)? {
            match chunk.target {
                OutboundTarget::Data => self.transport.write_data(&chunk.bytes).await,
                OutboundTarget::LastData => self.transport.write_last_data(&chunk.bytes).await,
            }
            .map_err(map_transport_error)?;
        }
        Ok(())
    }

    async fn replenish_receive_credit(
        &mut self,
        source: NotificationSource,
    ) -> Result<(), BleError> {
        if let Some(credit) = self.receive_credit.record(source) {
            self.transport
                .write_control(&credit)
                .await
                .map_err(map_transport_error)?;
            tracing::debug!(
                operation = "receive-credit",
                credited_chunks = credit[1],
                "replenished BLE receive credit"
            );
        }
        Ok(())
    }

    async fn next_payload(
        &mut self,
        deadline: Instant,
        counts: &mut NotificationCounts,
    ) -> Result<Vec<u8>, BleError> {
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or_else(|| {
                    tracing::debug!(
                        operation = "wait-for-payload",
                        "response deadline expired before a complete payload arrived"
                    );
                    BleError::Timeout {
                        operation: "response-deadline",
                    }
                })?;
            let notification = tokio::time::timeout(remaining, self.transport.next_notification())
                .await
                .map_err(|_| {
                    tracing::debug!(
                        operation = "notification-wait",
                        remaining_ms = remaining.as_millis() as u64,
                        "response deadline expired while waiting for a BLE notification"
                    );
                    BleError::Timeout {
                        operation: "response-deadline",
                    }
                })?
                .map_err(map_transport_error)?;
            self.replenish_receive_credit(notification.source).await?;
            match notification.source {
                NotificationSource::Control => {
                    counts.control = counts.control.saturating_add(1);
                    match ControlMessage::parse(&notification.value) {
                        Ok(ControlMessage::Error { .. }) => return Err(BleError::Contention),
                        Ok(ControlMessage::ClearBuffer) => {
                            counts.clear_buffer = counts.clear_buffer.saturating_add(1);
                            self.pending.clear();
                        }
                        _ => {}
                    }
                }
                NotificationSource::Data => {
                    counts.data = counts.data.saturating_add(1);
                    self.pending
                        .push_data(&notification.value)
                        .map_err(map_reassembly_error)?;
                }
                NotificationSource::LastData => {
                    counts.last_data = counts.last_data.saturating_add(1);
                    if let Some(payload) = self
                        .pending
                        .push_last_data(&notification.value)
                        .map_err(map_reassembly_error)?
                    {
                        counts.completed_payloads = counts.completed_payloads.saturating_add(1);
                        return Ok(payload);
                    }
                }
            }
        }
    }

    async fn drain_completed_payloads(&mut self, instance: u16) -> Result<(), BleError> {
        self.pending.clear();
        self.fallback_values = None;
        let started_at = Instant::now();
        let deadline = started_at + SUBSCRIBE_DRAIN_BUDGET;
        let mut notifications = 0u32;
        let mut completed_payloads = 0u32;
        let mut correlated_payloads = 0u32;
        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                tracing::debug!(
                    operation = "subscribe-drain",
                    outcome = "budget",
                    notifications,
                    completed_payloads,
                    correlated_payloads,
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "subscribe notification drain reached total budget"
                );
                return Ok(());
            };
            let wait = remaining.min(SUBSCRIBE_DRAIN_QUIET);
            match tokio::time::timeout(wait, self.transport.next_notification()).await {
                Err(_) => {
                    tracing::debug!(
                        operation = "subscribe-drain",
                        outcome = "quiet",
                        notifications,
                        completed_payloads,
                        correlated_payloads,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        "subscribe notification queue reached quiet period"
                    );
                    return Ok(());
                }
                Ok(Err(TransportError::Timeout { .. })) => {
                    tracing::debug!(
                        operation = "subscribe-drain",
                        outcome = "transport-quiet",
                        notifications,
                        completed_payloads,
                        correlated_payloads,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        "subscribe notification queue reached transport quiet period"
                    );
                    return Ok(());
                }
                Ok(Err(error)) => return Err(map_transport_error(error)),
                Ok(Ok(notification)) => {
                    self.replenish_receive_credit(notification.source).await?;
                    notifications = notifications.saturating_add(1);
                    match notification.source {
                        NotificationSource::Control => {
                            if matches!(
                                ControlMessage::parse(&notification.value),
                                Ok(ControlMessage::ClearBuffer)
                            ) {
                                self.pending.clear();
                            }
                        }
                        NotificationSource::Data => self
                            .pending
                            .push_data(&notification.value)
                            .map_err(map_reassembly_error)?,
                        NotificationSource::LastData => {
                            if let Some(payload) = self
                                .pending
                                .push_last_data(&notification.value)
                                .map_err(map_reassembly_error)?
                            {
                                completed_payloads = completed_payloads.saturating_add(1);
                                if payload_has_values(&payload, instance).unwrap_or(false) {
                                    correlated_payloads = correlated_payloads.saturating_add(1);
                                    self.fallback_values = Some(payload);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    async fn wait_for_values(
        &mut self,
        instance: u16,
        deadline: Instant,
    ) -> Result<Vec<u8>, BleError> {
        let started_at = Instant::now();
        let mut counts = NotificationCounts::default();
        let mut unrelated_payloads = 0u32;
        loop {
            let payload = match self.next_payload(deadline, &mut counts).await {
                Ok(payload) => payload,
                Err(error) => {
                    tracing::debug!(
                        operation = "get-values-response",
                        instance,
                        notifications = counts.total(),
                        control_notifications = counts.control,
                        data_notifications = counts.data,
                        last_data_notifications = counts.last_data,
                        clear_buffer_notifications = counts.clear_buffer,
                        completed_payloads = counts.completed_payloads,
                        unrelated_payloads,
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        error = %error,
                        "failed while waiting for correlated value response"
                    );
                    return Err(error);
                }
            };
            let correlated = match payload_has_values(&payload, instance) {
                Ok(correlated) => correlated,
                Err(error) => {
                    tracing::debug!(
                        operation = "get-values-response",
                        instance,
                        notifications = counts.total(),
                        completed_payloads = counts.completed_payloads,
                        unrelated_payloads,
                        payload_bytes = payload.len(),
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        error = %error,
                        "received malformed completed BLE payload"
                    );
                    return Err(error);
                }
            };
            if correlated {
                tracing::debug!(
                    operation = "get-values-response",
                    instance,
                    notifications = counts.total(),
                    control_notifications = counts.control,
                    data_notifications = counts.data,
                    last_data_notifications = counts.last_data,
                    clear_buffer_notifications = counts.clear_buffer,
                    completed_payloads = counts.completed_payloads,
                    unrelated_payloads,
                    response_bytes = payload.len(),
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "received correlated value response"
                );
                return Ok(payload);
            }
            unrelated_payloads = unrelated_payloads.saturating_add(1);
            tracing::trace!(
                operation = "get-values-response",
                instance,
                completed_payloads = counts.completed_payloads,
                unrelated_payloads,
                payload_bytes = payload.len(),
                "ignored uncorrelated completed BLE payload"
            );
            // Subscribe acknowledgements, keepalives, and stale push frames
            // are valid traffic but are not the response to this getValues.
        }
    }
}

#[async_trait(?Send)]
impl BleSession for VeSmartBleSession {
    async fn discover(&mut self) -> Result<(), BleError> {
        // `BluezTransport::open` deliberately performs discovery + connect +
        // GATT setup transactionally. The service's following `connect`
        // phase is therefore an idempotent state assertion.
        self.transport.open().await.map_err(map_transport_error)?;
        self.opened = true;
        Ok(())
    }

    async fn connect(&mut self) -> Result<(), BleError> {
        if self.opened {
            Ok(())
        } else {
            Err(BleError::Disconnected)
        }
    }

    async fn negotiate(&mut self, frames: &[Vec<u8>]) -> Result<(), BleError> {
        if self.negotiated {
            tracing::debug!(
                operation = "session-reuse",
                "reusing negotiated BLE session"
            );
            return Ok(());
        }
        let control = self
            .transport
            .read_control()
            .await
            .map_err(map_transport_error)?;
        ControlInfo::parse(&control)
            .ok_or_else(|| BleError::Other("invalid control info".into()))?;
        for frame in frames {
            self.transport
                .write_control(frame)
                .await
                .map_err(map_transport_error)?;
        }
        self.negotiated = true;
        self.reusable = false;
        Ok(())
    }

    async fn subscribe(&mut self, instance: u16, payload: &[u8]) -> Result<(), BleError> {
        if !self.negotiated {
            return Err(BleError::Other("protocol not negotiated".into()));
        }
        if self.subscribed_instance == Some(instance) {
            tracing::debug!(
                operation = "session-reuse",
                instance,
                "reusing active VE.Smart subscription"
            );
            // Notifications can accumulate while the runner sleeps. Drain to
            // a short quiet period (replenishing receive credit on the way)
            // so the following explicit getValues request is correlated
            // against fresh traffic rather than an idle-period backlog.
            self.drain_completed_payloads(instance).await?;
            return Ok(());
        }
        self.write_request(payload).await?;
        // Subscribe produces acknowledgements/push notifications on the same
        // queue as getValues. Consume them until a short quiet period so the
        // following request cannot mistake a stale subscribe frame for its
        // response (the proven Python reader waits here for the same reason).
        self.drain_completed_payloads(instance).await?;
        self.subscribed_instance = Some(instance);
        Ok(())
    }

    async fn request_values(
        &mut self,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, BleError> {
        let instance = self
            .subscribed_instance
            .ok_or_else(|| BleError::Other("device is not subscribed".into()))?;
        self.reusable = false;
        for attempt in 1..=2u8 {
            self.pending.clear();
            self.write_request(payload).await?;
            match self
                .wait_for_values(instance, Instant::now() + timeout)
                .await
            {
                Ok(values) => {
                    self.reusable = true;
                    return Ok(values);
                }
                Err(error) => {
                    if let Some(values) = self.fallback_values.take() {
                        tracing::debug!(
                            operation = "get-values-fallback",
                            instance,
                            response_bytes = values.len(),
                            error_kind = error.kind(),
                            "using correlated subscription values after explicit request failure"
                        );
                        self.reusable = true;
                        return Ok(values);
                    }
                    if attempt == 1 && matches!(error, BleError::Timeout { .. }) {
                        tracing::debug!(
                            operation = "get-values-retry",
                            instance,
                            attempt,
                            error_kind = error.kind(),
                            "retrying read-only getValues after response timeout"
                        );
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    }
                    return Err(error);
                }
            }
        }
        unreachable!("bounded getValues attempts always return")
    }

    async fn finish_cycle(&mut self) -> Result<(), BleError> {
        if self.reusable {
            tracing::debug!(
                operation = "session-reuse",
                "retaining healthy BLE session for the next cycle"
            );
            Ok(())
        } else {
            self.disconnect().await
        }
    }

    async fn disconnect(&mut self) -> Result<(), BleError> {
        self.transport.close().await;
        self.opened = false;
        self.negotiated = false;
        self.subscribed_instance = None;
        self.pending.clear();
        self.receive_credit.clear();
        self.fallback_values = None;
        self.reusable = false;
        Ok(())
    }
}

fn map_transport_error(error: TransportError) -> BleError {
    match error.class() {
        BleErrorClass::Timeout => match error {
            TransportError::Timeout { operation } => BleError::Timeout { operation },
            _ => BleError::Timeout {
                operation: "transport",
            },
        },
        BleErrorClass::Auth => BleError::Authentication,
        BleErrorClass::Contention => BleError::Contention,
        BleErrorClass::NotFound => BleError::NotFound,
        BleErrorClass::Dbus => BleError::Transport(error.to_string()),
        BleErrorClass::Other => match error {
            TransportError::NotificationStopped => BleError::Disconnected,
            _ => BleError::Other(error.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::map_transport_error;

    #[test]
    fn transport_timeout_operation_survives_service_mapping() {
        let error = map_transport_error(victron_bluez::BleError::Timeout {
            operation: "notification",
        });
        assert_eq!(
            error,
            victron_service::BleError::Timeout {
                operation: "notification"
            }
        );
        assert_eq!(error.to_string(), "timeout: notification");
    }
}
