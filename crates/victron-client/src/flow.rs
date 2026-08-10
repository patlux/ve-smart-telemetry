use victron_bluez::NotificationSource;
use victron_protocol::Response;
use victron_service::BleError;

pub const RECEIVE_CREDIT_THRESHOLD: u8 = 0x41;

#[derive(Debug, Default)]
pub struct ReceiveCredit {
    pending_chunks: u8,
}

impl ReceiveCredit {
    pub fn record(&mut self, source: NotificationSource) -> Option<[u8; 2]> {
        if matches!(
            source,
            NotificationSource::Data | NotificationSource::LastData
        ) {
            self.pending_chunks = self.pending_chunks.saturating_add(1);
        }
        if self.pending_chunks >= RECEIVE_CREDIT_THRESHOLD {
            let credited = self.pending_chunks;
            self.pending_chunks = 0;
            Some([0xf9, credited])
        } else {
            None
        }
    }

    pub fn clear(&mut self) {
        self.pending_chunks = 0;
    }
}

pub fn payload_has_values(payload: &[u8], instance: u16) -> Result<bool, BleError> {
    let responses = Response::parse_stream(payload).map_err(map_protocol_error)?;
    Ok(responses.iter().any(|response| {
        matches!(
            response,
            Response::Value { instance: response_instance, .. }
                if *response_instance == u64::from(instance)
        )
    }))
}

pub fn map_protocol_error(_error: victron_protocol::ProtocolError) -> BleError {
    BleError::Other("protocol decode failed".into())
}

pub fn map_reassembly_error(_error: victron_protocol::ReassemblyError) -> BleError {
    BleError::Other("response reassembly failed".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receive_credit_replenishes_after_sixty_five_data_chunks() {
        let mut credit = ReceiveCredit::default();
        assert_eq!(credit.record(NotificationSource::Control), None);
        for _ in 0..RECEIVE_CREDIT_THRESHOLD - 1 {
            assert_eq!(credit.record(NotificationSource::LastData), None);
        }
        assert_eq!(
            credit.record(NotificationSource::Data),
            Some([0xf9, RECEIVE_CREDIT_THRESHOLD])
        );
        assert_eq!(credit.record(NotificationSource::LastData), None);
    }

    #[test]
    fn subscribe_ack_is_not_a_get_values_response() {
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
    fn protocol_errors_are_bounded_and_payload_free() {
        let raw = "wire-secret-marker";
        let error = map_protocol_error(victron_protocol::ProtocolError::Malformed(raw));
        assert_eq!(error.to_string(), "protocol decode failed");
        assert!(!error.to_string().contains(raw));
    }
}
