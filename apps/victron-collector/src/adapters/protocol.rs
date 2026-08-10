//! Concrete read-only VE.Smart protocol and domain adapter.

use std::time::SystemTime;

use victron_domain::{
    ChargerState, ConnectionHealth, DeviceId, LoadState, Quality, Sample, SampleBuilder,
};
use victron_protocol::{Confidence, Request, Response, Scaled, VregValue};
use victron_service::{AcquirePlan, ProtocolAdapter, ProtocolError, RawValue};

/// Conservative dashboard register set. `0xed8e` stays excluded until its
/// lifetime-yield scaling is confirmed.
pub const MVP_VREGS: &[u16] = &[
    0xedbb, // confirmed PV voltage
    0xedbd, // candidate PV current
    0xedbc, // confirmed PV power
    0xed8d, // candidate battery voltage
    0xed8c, // candidate battery current
    0x0201, // candidate charger state
    0xeda8, // candidate load state
    0xedad, // candidate load current
    0xedaa, // candidate load power
];

#[derive(Debug, Clone)]
pub struct VeSmartProtocol {
    device: DeviceId,
}

impl VeSmartProtocol {
    pub fn new(device: DeviceId) -> Self {
        Self { device }
    }
}

impl ProtocolAdapter for VeSmartProtocol {
    fn vregs(&self) -> &[u16] {
        MVP_VREGS
    }

    fn acquire_plan(&self, instance: u16, vregs: &[u16]) -> Result<AcquirePlan, ProtocolError> {
        if instance == 0 {
            return Err(ProtocolError::NotSubscribed);
        }
        Ok(AcquirePlan {
            negotiation_frames: victron_protocol::control::NEGOTIATION_WRITES
                .iter()
                .map(|frame| frame.to_vec())
                .collect(),
            subscribe_payload: Request::Subscribe { instance }
                .encode()
                .map_err(map_wire_error)?,
            values_payload: Request::GetValues {
                instance,
                registers: vregs.to_vec(),
            }
            .encode()
            .map_err(map_wire_error)?,
        })
    }

    fn parse_response(&self, instance: u16, bytes: &[u8]) -> Result<Vec<RawValue>, ProtocolError> {
        if bytes.is_empty() {
            return Err(ProtocolError::EmptyResponse);
        }
        let responses = Response::parse_stream(bytes).map_err(map_wire_error)?;
        let mut values = Vec::new();
        for response in responses {
            match response {
                Response::Value {
                    instance: response_instance,
                    register,
                    data,
                } if response_instance == u64::from(instance) => {
                    values.push(RawValue {
                        vreg: register,
                        raw: data,
                    });
                }
                Response::Response { code, .. }
                | Response::ValueResponse { code, .. }
                | Response::PathResponse { code, .. }
                    if !code.is_ok() =>
                {
                    return Err(ProtocolError::ContentionResponse {
                        code: i64::from(response_code(code)),
                    });
                }
                _ => {}
            }
        }
        if values.is_empty() {
            Err(ProtocolError::EmptyResponse)
        } else {
            Ok(values)
        }
    }

    fn translate(&self, _instance: u16, values: &[RawValue]) -> Result<Sample, ProtocolError> {
        let mut builder = Sample::builder(self.device.clone(), SystemTime::now())
            .connection_health(ConnectionHealth::Up);
        for raw in values {
            builder = apply_value(builder, raw)?;
        }
        Ok(builder.build())
    }
}

fn apply_value(builder: SampleBuilder, raw: &RawValue) -> Result<SampleBuilder, ProtocolError> {
    let decoded = VregValue::new(raw.vreg, raw.raw.clone()).decode();
    let quality = match decoded.confidence {
        Confidence::Confirmed => Quality::ConfirmedNative,
        Confidence::Candidate => Quality::Candidate,
    };
    let Some(value) = decoded.value else {
        return Ok(builder);
    };
    let result = match (raw.vreg, value) {
        (0xedbb, Scaled::Number(value)) => builder.pv_voltage_volts(value, quality),
        (0xedbd, Scaled::Number(value)) => builder.pv_current_amperes(value, quality),
        (0xedbc, Scaled::Integer(value)) => builder.pv_power_watts(value as f64, quality),
        (0xed8d, Scaled::Number(value)) => builder.battery_voltage_volts(value, quality),
        (0xed8c, Scaled::Number(value)) => builder.battery_current_amperes(value, quality),
        (0x0201, Scaled::State { code, .. }) => {
            return Ok(builder.charger_state(ChargerState::from_code(code)));
        }
        (0xeda8, Scaled::State { code, .. }) => {
            return Ok(builder.load_state(LoadState::from_code(code)));
        }
        (0xedad, Scaled::Number(value)) => builder.load_current_amperes(value, quality),
        (0xedaa, Scaled::Integer(value)) => builder.load_power_watts(value as f64, quality),
        _ => return Ok(builder),
    };
    result.map_err(|_| ProtocolError::InvalidValue { vreg: raw.vreg })
}

fn response_code(code: victron_protocol::ResponseCode) -> u8 {
    match code {
        victron_protocol::ResponseCode::Ok => 0,
        victron_protocol::ResponseCode::Unknown1 => 1,
        victron_protocol::ResponseCode::Rejected => 2,
        victron_protocol::ResponseCode::Other(code) => code,
    }
}

fn map_wire_error(error: victron_protocol::ProtocolError) -> ProtocolError {
    match error {
        victron_protocol::ProtocolError::Truncated => ProtocolError::Truncated,
        victron_protocol::ProtocolError::BufferLimit { .. }
        | victron_protocol::ProtocolError::DepthLimit
        | victron_protocol::ProtocolError::ItemLimit => ProtocolError::BufferExceeded,
        _ => ProtocolError::Malformed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_is_read_only_and_matches_instance() {
        let protocol = VeSmartProtocol::new(DeviceId::new("solar-charger").unwrap());
        let plan = protocol.acquire_plan(3, protocol.vregs()).unwrap();
        assert_eq!(
            plan.negotiation_frames,
            vec![vec![0xfa, 0x80, 0xff], vec![0xf9, 0x80]]
        );
        assert_eq!(plan.subscribe_payload, vec![0x03, 0x03]);
        assert_eq!(plan.values_payload[0..3], [0x05, 0x03, 0x89]);
    }

    #[test]
    fn parses_and_translates_confirmed_voltage() {
        let protocol = VeSmartProtocol::new(DeviceId::new("solar-charger").unwrap());
        let bytes = [0x08, 0x03, 0x19, 0xed, 0xbb, 0x42, 0xf3, 0x0a];
        let values = protocol.parse_response(3, &bytes).unwrap();
        let sample = protocol.translate(3, &values).unwrap();
        assert_eq!(sample.pv_voltage_volts().unwrap().value(), 28.03);
        assert_eq!(
            sample.pv_voltage_volts().unwrap().quality(),
            Quality::ConfirmedNative
        );
    }

    #[test]
    fn translates_documented_panel_power_as_confirmed() {
        let protocol = VeSmartProtocol::new(DeviceId::new("solar-charger").unwrap());
        // Victron BlueSolar HEX protocol: 0xEDBC Panel power, un32,
        // scale 0.01 W. Raw 100 therefore resolves to 1 W.
        let bytes = [0x08, 0x03, 0x19, 0xed, 0xbc, 0x44, 0x64, 0x00, 0x00, 0x00];
        let values = protocol.parse_response(3, &bytes).unwrap();
        let sample = protocol.translate(3, &values).unwrap();
        assert_eq!(sample.pv_power_watts().unwrap().value(), 1.0);
        assert_eq!(
            sample.pv_power_watts().unwrap().quality(),
            Quality::ConfirmedNative
        );
    }
}
