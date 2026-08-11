//! Typed read-only diagnostic operations shared by CLI commands.

use std::time::{Duration, Instant};

use victron_bluez::BleTransport;
use victron_protocol::{Request, Response};
use victron_service::{BleError, BleSession};

use super::{NotificationCounts, VeSmartBleSession, SUBSCRIBE_DRAIN_QUIET};
use crate::flow::map_protocol_error;

impl VeSmartBleSession {
    /// Open and negotiate using the fixed read-only control handshake.
    /// Caller-provided control bytes are deliberately not accepted.
    pub async fn open_read_only(&mut self) -> Result<(), BleError> {
        let started_at = Instant::now();
        tracing::debug!(
            operation = "diagnostic-open",
            stage = "transport",
            "opening read-only diagnostic session"
        );
        if let Err(error) = self.discover().await {
            tracing::debug!(
                operation = "diagnostic-open",
                stage = "transport",
                outcome = "failed",
                error_kind = error.kind(),
                timeout_operation = error.operation().unwrap_or("none"),
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "read-only diagnostic transport open failed"
            );
            return Err(error);
        }
        tracing::debug!(
            operation = "diagnostic-open",
            stage = "transport",
            outcome = "opened",
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "read-only diagnostic transport opened"
        );
        self.connect().await?;
        let frames = victron_protocol::control::NEGOTIATION_WRITES
            .iter()
            .map(|frame| frame.to_vec())
            .collect::<Vec<_>>();
        tracing::debug!(
            operation = "diagnostic-open",
            stage = "negotiate",
            frame_count = frames.len(),
            "negotiating read-only diagnostic session"
        );
        match self.negotiate(&frames).await {
            Ok(()) => {
                tracing::debug!(
                    operation = "diagnostic-open",
                    stage = "negotiate",
                    outcome = "ready",
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "read-only diagnostic session ready"
                );
                Ok(())
            }
            Err(error) => {
                tracing::debug!(
                    operation = "diagnostic-open",
                    stage = "negotiate",
                    outcome = "failed",
                    error_kind = error.kind(),
                    timeout_operation = error.operation().unwrap_or("none"),
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "read-only diagnostic negotiation failed"
                );
                Err(error)
            }
        }
    }

    /// Subscribe to one positive VE.Smart instance.
    pub async fn subscribe_read_only(&mut self, instance: u16) -> Result<(), BleError> {
        if instance == 0 {
            return Err(BleError::Other("instance must be positive".into()));
        }
        let started_at = Instant::now();
        let payload = Request::Subscribe { instance }
            .encode()
            .map_err(map_protocol_error)?;
        tracing::debug!(
            operation = "diagnostic-subscribe",
            instance,
            "subscribing read-only diagnostic session"
        );
        match self.subscribe(instance, &payload).await {
            Ok(()) => {
                tracing::debug!(
                    operation = "diagnostic-subscribe",
                    instance,
                    outcome = "subscribed",
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "read-only diagnostic subscription ready"
                );
                Ok(())
            }
            Err(error) => {
                tracing::debug!(
                    operation = "diagnostic-subscribe",
                    instance,
                    outcome = "failed",
                    error_kind = error.kind(),
                    timeout_operation = error.operation().unwrap_or("none"),
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "read-only diagnostic subscription failed"
                );
                Err(error)
            }
        }
    }

    /// Execute one typed read-only request and collect matching responses.
    ///
    /// Collection stops when all requested keys arrived or, after at least one
    /// matching record, when the stream has been quiet for 500 ms.
    pub async fn request_read_only(
        &mut self,
        request: &Request,
        timeout: Duration,
    ) -> Result<Vec<Response>, BleError> {
        if matches!(request, Request::Subscribe { .. }) {
            return Err(BleError::Other(
                "use subscribe_read_only for subscription requests".into(),
            ));
        }
        if let Some(instance) = request_instance(request) {
            if self.subscribed_instance != Some(instance) {
                return Err(BleError::Other("request instance is not subscribed".into()));
            }
        }

        self.pending.clear();
        let started_at = Instant::now();
        let opcode = request.opcode().as_u8();
        let payload = request.encode().map_err(map_protocol_error)?;
        tracing::debug!(
            operation = "diagnostic-request",
            opcode,
            request_bytes = payload.len(),
            timeout_ms = timeout.as_millis() as u64,
            "sending typed read-only diagnostic request"
        );
        if let Err(error) = self.write_request(&payload).await {
            tracing::debug!(
                operation = "diagnostic-request",
                opcode,
                outcome = "write-failed",
                error_kind = error.kind(),
                timeout_operation = error.operation().unwrap_or("none"),
                elapsed_ms = started_at.elapsed().as_millis() as u64,
                "typed read-only diagnostic request write failed"
            );
            return Err(error);
        }

        let deadline = Instant::now() + timeout;
        let mut quiet_deadline = None;
        let mut counts = NotificationCounts::default();
        let mut collected = Vec::new();
        loop {
            let wait_until = quiet_deadline
                .map(|quiet: Instant| quiet.min(deadline))
                .unwrap_or(deadline);
            let payload = match self.next_payload(wait_until, &mut counts).await {
                Ok(payload) => payload,
                Err(BleError::Timeout { .. }) if !collected.is_empty() => {
                    tracing::debug!(
                        operation = "diagnostic-request",
                        opcode,
                        outcome = "quiet-complete",
                        response_records = collected.len(),
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        "typed read-only diagnostic request completed after quiet period"
                    );
                    return Ok(collected);
                }
                Err(error) => {
                    tracing::debug!(
                        operation = "diagnostic-request",
                        opcode,
                        outcome = "failed",
                        error_kind = error.kind(),
                        timeout_operation = error.operation().unwrap_or("none"),
                        response_records = collected.len(),
                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                        "typed read-only diagnostic request failed"
                    );
                    return Err(error);
                }
            };
            let responses = Response::parse_stream(&payload).map_err(map_protocol_error)?;
            collected.extend(
                responses
                    .into_iter()
                    .filter(|response| response_matches_request(response, request)),
            );
            if request_is_complete(request, &collected) {
                tracing::debug!(
                    operation = "diagnostic-request",
                    opcode,
                    outcome = "complete",
                    response_records = collected.len(),
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    "typed read-only diagnostic request completed"
                );
                return Ok(collected);
            }
            if !collected.is_empty() {
                quiet_deadline = Some(Instant::now() + SUBSCRIBE_DRAIN_QUIET);
            }
        }
    }

    /// Read the last RSSI cached by BlueZ, when available.
    pub async fn rssi_read_only(&mut self) -> Result<Option<i16>, BleError> {
        self.transport
            .rssi()
            .await
            .map_err(super::map_transport_error)
    }

    /// Close a diagnostic session deterministically.
    pub async fn close_read_only(&mut self) {
        let _ = self.disconnect().await;
    }
}

fn request_instance(request: &Request) -> Option<u16> {
    match request {
        Request::GetDevices => None,
        Request::Subscribe { instance }
        | Request::GetValues { instance, .. }
        | Request::GetPathList { instance }
        | Request::GetPathValues { instance, .. } => Some(*instance),
    }
}

fn response_matches_request(response: &Response, request: &Request) -> bool {
    match (request, response) {
        (Request::GetDevices, Response::DeviceList { .. }) => true,
        (
            Request::GetValues {
                instance,
                registers,
            },
            Response::Value {
                instance: response_instance,
                register,
                ..
            }
            | Response::ValueResponse {
                instance: response_instance,
                register,
                ..
            },
        ) => *response_instance == u64::from(*instance) && registers.contains(register),
        (
            Request::GetPathList { instance },
            Response::PathList {
                instance: response_instance,
                ..
            }
            | Response::NewPath {
                instance: response_instance,
                ..
            },
        ) => *response_instance == u64::from(*instance),
        (
            Request::GetPathValues {
                instance,
                path_indexes,
            },
            Response::PathValue {
                instance: response_instance,
                path_index,
                ..
            }
            | Response::PathResponse {
                instance: response_instance,
                path_index,
                ..
            },
        ) => *response_instance == u64::from(*instance) && path_indexes.contains(path_index),
        (
            _,
            Response::Response {
                instance: response_instance,
                opcode,
                ..
            },
        ) => {
            request_instance(request).is_none_or(|value| *response_instance == u64::from(value))
                && *opcode == u64::from(request.opcode().as_u8())
        }
        _ => false,
    }
}

fn request_is_complete(request: &Request, responses: &[Response]) -> bool {
    if responses
        .iter()
        .any(|response| matches!(response, Response::Response { code, .. } if !code.is_ok()))
    {
        return true;
    }
    match request {
        Request::GetDevices => responses
            .iter()
            .any(|response| matches!(response, Response::DeviceList { .. })),
        Request::GetValues { registers, .. } => registers.iter().all(|wanted| {
            responses.iter().any(|response| {
                matches!(
                    response,
                    Response::Value { register, .. }
                        | Response::ValueResponse { register, .. }
                        if register == wanted
                )
            })
        }),
        // A compressed PathList is complete immediately. A sequence of
        // NewPath records is collected until the bounded quiet period.
        Request::GetPathList { .. } => responses
            .iter()
            .any(|response| matches!(response, Response::PathList { .. })),
        Request::GetPathValues { path_indexes, .. } => path_indexes.iter().all(|wanted| {
            responses.iter().any(|response| {
                matches!(
                    response,
                    Response::PathValue { path_index, .. }
                        | Response::PathResponse { path_index, .. }
                        if path_index == wanted
                )
            })
        }),
        Request::Subscribe { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::map_reassembly_error;
    use victron_protocol::ResponseCode;

    #[test]
    fn get_values_requires_every_requested_register() {
        let request = Request::GetValues {
            instance: 3,
            registers: vec![0xedbb, 0xedbc],
        };
        let first = Response::Value {
            instance: 3,
            register: 0xedbb,
            data: vec![1, 2],
        };
        assert!(!request_is_complete(&request, std::slice::from_ref(&first)));
        let second = Response::Value {
            instance: 3,
            register: 0xedbc,
            data: vec![3, 4, 5, 6],
        };
        assert!(request_is_complete(&request, &[first, second]));
    }

    #[test]
    fn rejection_completes_without_waiting_for_missing_values() {
        let request = Request::GetPathList { instance: 3 };
        let rejected = Response::Response {
            instance: 3,
            opcode: request.opcode().as_u8() as u64,
            code: ResponseCode::Rejected,
        };
        assert!(response_matches_request(&rejected, &request));
        assert!(request_is_complete(&request, &[rejected]));
    }

    #[test]
    fn new_path_records_are_correlated_but_need_the_quiet_period() {
        let request = Request::GetPathList { instance: 3 };
        let path = Response::NewPath {
            instance: 3,
            path_index: 7,
            path: "/History/Daily/0/Yield".into(),
        };
        assert!(response_matches_request(&path, &request));
        assert!(!request_is_complete(&request, &[path]));
    }

    #[test]
    fn another_instance_is_never_correlated() {
        let request = Request::GetValues {
            instance: 3,
            registers: vec![0xedbb],
        };
        let other = Response::Value {
            instance: 1,
            register: 0xedbb,
            data: vec![1, 2],
        };
        assert!(!response_matches_request(&other, &request));
    }

    #[test]
    fn protocol_error_mapping_remains_payload_free() {
        let error = map_reassembly_error(victron_protocol::ReassemblyError {
            capacity: 1,
            needed: 2,
        });
        assert_eq!(error.to_string(), "response reassembly failed");
    }
}
