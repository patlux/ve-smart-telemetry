//! `victron-cli decode-fixture` — decode a sanitized VE.Smart payload fixture.

use std::path::PathBuf;

use clap::Args;
use serde_json::{json, Value};
use victron_protocol::{Response, VregValue};

use super::common::{cbor_item_json, decoded_vreg_json, print_or_write_json};
use crate::{runtime, CliError};

#[derive(Debug, Args)]
pub struct DecodeFixture {
    /// Path to a sanitized complete Data/LastData payload.
    #[arg(value_name = "PATH")]
    path: PathBuf,

    /// VE.Smart instance expected in the fixture.
    #[arg(long, default_value_t = 3)]
    instance: u16,

    /// Include raw VREG bytes in output.
    #[arg(long)]
    verbose: bool,
}

impl DecodeFixture {
    pub fn run(&self) -> Result<(), CliError> {
        if self.instance == 0 {
            return Err(runtime("instance must be positive"));
        }
        let payload = std::fs::read(&self.path)
            .map_err(|error| runtime(format!("failed to read fixture: {error}")))?;
        let responses = Response::parse_stream(&payload)
            .map_err(|error| runtime(format!("failed to decode fixture: {error}")))?;
        let records = responses
            .iter()
            .map(|response| response_json(response, self.verbose))
            .collect::<Vec<_>>();
        let matching_values = responses
            .iter()
            .filter(|response| {
                matches!(response, Response::Value { instance, .. } if *instance == u64::from(self.instance))
            })
            .count();
        print_or_write_json(
            &json!({
                "ok": true,
                "fixture": self.path,
                "instance": self.instance,
                "payloadBytes": payload.len(),
                "recordCount": records.len(),
                "matchingValueCount": matching_values,
                "records": records,
            }),
            None,
        )
    }
}

fn response_json(response: &Response, raw: bool) -> Value {
    match response {
        Response::DeviceList { devices } => json!({"type": "DeviceList", "devices": devices}),
        Response::Response {
            instance,
            opcode,
            code,
        } => json!({
            "type": "Response",
            "instance": instance,
            "opcode": format!("0x{opcode:02x}"),
            "code": code.name(),
        }),
        Response::Value {
            instance,
            register,
            data,
        } => {
            let decoded = VregValue::new(*register, data.clone()).decode();
            let mut value = decoded_vreg_json(&decoded, raw.then_some(data.as_slice()));
            if !raw {
                value.as_object_mut().expect("object").remove("raw");
            }
            json!({"type": "Value", "instance": instance, "value": value})
        }
        Response::ValueResponse {
            instance,
            register,
            code,
        } => json!({
            "type": "ValueResponse",
            "instance": instance,
            "register": format!("0x{register:04x}"),
            "code": code.name(),
        }),
        Response::PathList {
            instance,
            compressed,
        } => json!({
            "type": "PathList",
            "instance": instance,
            "compressedBytes": compressed.len(),
        }),
        Response::NewPath {
            instance,
            path_index,
            path,
        } => json!({
            "type": "NewPath",
            "instance": instance,
            "pathIndex": path_index,
            "path": path,
        }),
        Response::PathValue {
            instance,
            path_index,
            value,
        } => json!({
            "type": "PathValue",
            "instance": instance,
            "pathIndex": path_index,
            "value": cbor_item_json(value),
        }),
        Response::PathResponse {
            instance,
            path_index,
            code,
        } => json!({
            "type": "PathResponse",
            "instance": instance,
            "pathIndex": path_index,
            "code": code.name(),
        }),
        Response::Unknown { opcode } => {
            json!({"type": "Unknown", "opcode": format!("0x{opcode:x}")})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_block_is_rendered_without_raw_bytes_by_default() {
        let response = Response::Value {
            instance: 3,
            register: 0x104f,
            data: vec![0; 34],
        };
        let rendered = response_json(&response, false);
        assert_eq!(rendered["type"], "Value");
        assert!(rendered["value"].get("raw").is_none());
        assert_eq!(
            rendered["value"]["value"]["wordsLe"]
                .as_array()
                .unwrap()
                .len(),
            17
        );
    }
}
