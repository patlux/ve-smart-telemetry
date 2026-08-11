use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use victron_bluez::adapter::PowerPolicy;
use victron_bluez::discovery::DeviceSelector;
use victron_bluez::TransportConfig;
use victron_protocol::cbor::Item;
use victron_protocol::{DecodedVreg, Invalid, Scaled};

use crate::{runtime, CliError};

pub fn transport_config(
    device: &str,
    adapter: &str,
    discovery: Duration,
    response: Duration,
    connect: Duration,
) -> Result<TransportConfig, CliError> {
    let selector = DeviceSelector::new(Some(device.to_string()), None)
        .map_err(|error| runtime(error.to_string()))?;
    Ok(TransportConfig {
        adapter: Some(adapter.to_string()),
        selector,
        power_policy: PowerPolicy::RequireManual,
        connect_timeout: connect,
        discovery_timeout: discovery,
        notification_timeout: response,
        operation_timeout: response,
        write_chunk_size: victron_protocol::control::MIN_ATT_CHUNK_SIZE,
        require_advertisement_evidence: false,
    })
}

pub fn unix_ms_now() -> Result<u64, CliError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| runtime("system clock precedes Unix epoch"))?;
    u64::try_from(elapsed.as_millis()).map_err(|_| runtime("system clock is out of range"))
}

pub fn parse_u16(input: &str) -> Result<u16, String> {
    let value = input.trim();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u16::from_str_radix(hex, 16).map_err(|_| format!("invalid u16 register: {input}"))
    } else {
        value
            .parse::<u16>()
            .map_err(|_| format!("invalid u16 register: {input}"))
    }
}

pub fn decoded_vreg_json(decoded: &DecodedVreg, raw: Option<&[u8]>) -> Value {
    let value = match &decoded.value {
        Some(Scaled::Number(value)) => json!(value),
        Some(Scaled::Integer(value)) => json!(value),
        Some(Scaled::State { code, name }) => json!({"code": code, "name": name}),
        Some(Scaled::Slots(slots)) => Value::Array(
            slots
                .iter()
                .map(|slot| {
                    json!({
                        "offset": slot.offset,
                        "register": slot.register.map(|register| format!("0x{register:04x}")),
                        "raw": hex::encode(&slot.raw),
                    })
                })
                .collect(),
        ),
        Some(Scaled::Block { words_le, words_be }) => {
            json!({"wordsLe": words_le, "wordsBe": words_be})
        }
        None => Value::Null,
    };
    let invalid = match &decoded.invalid {
        Some(Invalid::Sentinel(value)) => json!({"kind": "sentinel", "value": value}),
        Some(Invalid::ShortPayload { need, have }) => {
            json!({"kind": "short-payload", "need": need, "have": have})
        }
        Some(Invalid::LengthMismatch { expected, have }) => {
            json!({"kind": "length-mismatch", "expected": expected, "have": have})
        }
        Some(Invalid::NotAligned { multiple, have }) => {
            json!({"kind": "not-aligned", "multiple": multiple, "have": have})
        }
        None => Value::Null,
    };
    json!({
        "register": format!("0x{:04x}", decoded.register),
        "name": decoded.name,
        "unit": decoded.unit,
        "decoder": decoded.decoder,
        "confidence": decoded.confidence.as_str(),
        "value": value,
        "invalid": invalid,
        "raw": raw.map(hex::encode),
    })
}

pub fn cbor_item_json(item: &Item) -> Value {
    match item {
        Item::UInt(value) => json!(value),
        Item::Int(value) => json!(value),
        Item::Bytes(value) => json!({"bytes": hex::encode(value)}),
        Item::Text(value) => json!(value),
        Item::Array(values) => Value::Array(values.iter().map(cbor_item_json).collect()),
        Item::Map(entries) => Value::Array(
            entries
                .iter()
                .map(|(key, value)| json!({"key": cbor_item_json(key), "value": cbor_item_json(value)}))
                .collect(),
        ),
        Item::Tag(tag, value) => json!({"tag": tag, "value": cbor_item_json(value)}),
        Item::Bool(value) => json!(value),
        Item::Null => Value::Null,
        Item::Undefined => json!({"undefined": true}),
        Item::Simple(value) => json!({"simple": value}),
        Item::Float(value) => json!(value),
    }
}

pub fn write_json(value: &Value, path: &std::path::Path) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| runtime(format!("failed to encode JSON: {error}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| runtime(format!("failed to create output directory: {error}")))?;
    }
    std::fs::write(path, format!("{text}\n"))
        .map_err(|error| runtime(format!("failed to write output: {error}")))
}

pub fn print_or_write_json(value: &Value, out: Option<&std::path::Path>) -> Result<(), CliError> {
    if let Some(path) = out {
        write_json(value, path)?;
    }
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| runtime(format!("failed to encode JSON: {error}")))?;
    println!("{text}");
    Ok(())
}
