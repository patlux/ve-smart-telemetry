//! VE.Smart session adapter over the concrete BlueZ byte transport.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use victron_bluez::{BleError as TransportError, BleErrorClass, BleTransport, NotificationSource};
use victron_protocol::control::{ControlInfo, ControlMessage};
use victron_protocol::{OutboundTarget, Reassembler, Response};
use victron_service::{BleError, BleSession};

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
}

impl VeSmartBleSession {
    pub fn new(config: victron_bluez::TransportConfig) -> Self {
        Self {
            transport: victron_bluez::BluezTransport::new(config),
            opened: false,
            negotiated: false,
            subscribed_instance: None,
            pending: Reassembler::new(),
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

    async fn drain_completed_payloads(&mut self, quiet: Duration) -> Result<(), BleError> {
        self.pending.clear();
        let mut notifications = 0u32;
        let mut completed_payloads = 0u32;
        loop {
            match tokio::time::timeout(quiet, self.transport.next_notification()).await {
                Err(_) => {
                    tracing::debug!(
                        operation = "subscribe-drain",
                        notifications,
                        completed_payloads,
                        quiet_ms = quiet.as_millis() as u64,
                        "subscribe notification queue reached quiet period"
                    );
                    return Ok(());
                }
                Ok(Err(TransportError::Timeout { .. })) => {
                    tracing::debug!(
                        operation = "subscribe-drain",
                        notifications,
                        completed_payloads,
                        quiet_ms = quiet.as_millis() as u64,
                        "subscribe notification queue reached transport quiet period"
                    );
                    return Ok(());
                }
                Ok(Err(error)) => return Err(map_transport_error(error)),
                Ok(Ok(notification)) => {
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
                            if self
                                .pending
                                .push_last_data(&notification.value)
                                .map_err(map_reassembly_error)?
                                .is_some()
                            {
                                completed_payloads = completed_payloads.saturating_add(1);
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
        Ok(())
    }

    async fn subscribe(&mut self, instance: u16, payload: &[u8]) -> Result<(), BleError> {
        if !self.negotiated {
            return Err(BleError::Other("protocol not negotiated".into()));
        }
        self.write_request(payload).await?;
        // Subscribe produces acknowledgements/push notifications on the same
        // queue as getValues. Consume them until a short quiet period so the
        // following request cannot mistake a stale subscribe frame for its
        // response (the proven Python reader waits here for the same reason).
        self.drain_completed_payloads(Duration::from_millis(500))
            .await?;
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
        self.pending.clear();
        self.write_request(payload).await?;
        self.wait_for_values(instance, Instant::now() + timeout)
            .await
    }

    async fn disconnect(&mut self) -> Result<(), BleError> {
        self.transport.close().await;
        self.opened = false;
        self.negotiated = false;
        self.subscribed_instance = None;
        self.pending.clear();
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

fn payload_has_values(payload: &[u8], instance: u16) -> Result<bool, BleError> {
    let responses = Response::parse_stream(payload).map_err(map_protocol_error)?;
    Ok(responses.iter().any(|response| {
        matches!(
            response,
            Response::Value { instance: response_instance, .. }
                if *response_instance == u64::from(instance)
        )
    }))
}

fn map_protocol_error(_error: victron_protocol::ProtocolError) -> BleError {
    BleError::Other("protocol decode failed".into())
}

fn map_reassembly_error(_error: victron_protocol::ReassemblyError) -> BleError {
    BleError::Other("response reassembly failed".into())
}

#[cfg(test)]
mod tests {
    use super::{map_protocol_error, map_transport_error, payload_has_values};

    #[test]
    fn subscribe_ack_is_not_a_get_values_response() {
        // Response(instance=3, request opcode=Subscribe/3, code=Ok).
        assert!(!payload_has_values(&[0x07, 0x03, 0x03, 0x00], 3).unwrap());
    }

    #[test]
    fn value_for_requested_instance_is_correlated() {
        let value = [0x08, 0x03, 0x19, 0xed, 0xbb, 0x42, 0xf3, 0x0a];
        assert!(payload_has_values(&value, 3).unwrap());
        assert!(!payload_has_values(&value, 1).unwrap());
    }

    #[test]
    fn keepalive_for_instance_zero_is_not_correlated() {
        let keepalive = [0x08, 0x00, 0x18, 0x93, 0x42, 0x10, 0x27];
        assert!(!payload_has_values(&keepalive, 3).unwrap());
    }

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

    #[test]
    fn protocol_errors_are_bounded_and_payload_free() {
        let raw = "wire-secret-marker";
        let error = map_protocol_error(victron_protocol::ProtocolError::Malformed(raw));
        assert_eq!(error.to_string(), "protocol decode failed");
        assert!(!error.to_string().contains(raw));
    }
}
