//! Cross-check tool: decode every `fixtures/protocol/*.bin` fixture and
//! print a compact summary.
//!
//! The expected values preserve the former prototype cross-check; this Rust
//! example is now the maintained manual inspection surface.
//!
//! ```text
//! cargo run --example crosscheck -- /path/to/fixtures/protocol
//! ```

use std::path::Path;

use victron_protocol::cbor::{decode_stream, Item};
use victron_protocol::control::{ControlInfo, ControlMessage};
use victron_protocol::frame::Reassembler;
use victron_protocol::response::Response;
use victron_protocol::vreg::{Confidence, DecodedVreg, Scaled};

fn fmt_item(item: &Item) -> String {
    match item {
        Item::UInt(v) => format!("{v}"),
        Item::Int(v) => format!("{v}"),
        Item::Bytes(b) => format!("bytes:{}", hex(b)),
        Item::Text(s) => format!("text:{s:?}"),
        Item::Array(items) => format!(
            "[{}]",
            items.iter().map(fmt_item).collect::<Vec<_>>().join(",")
        ),
        Item::Map(entries) => format!(
            "{{{}}}",
            entries
                .iter()
                .map(|(k, v)| format!("{}:{}", fmt_item(k), fmt_item(v)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Item::Tag(t, inner) => format!("tag({t},{})", fmt_item(inner)),
        Item::Bool(b) => format!("{b}"),
        Item::Null => "null".into(),
        Item::Undefined => "undefined".into(),
        Item::Simple(s) => format!("simple({s})"),
        Item::Float(f) => format!("float:{f}"),
    }
}

fn fmt_scaled(scaled: &Scaled) -> String {
    match scaled {
        Scaled::Number(v) => format!("{v}"),
        Scaled::Integer(v) => format!("{v}"),
        Scaled::State { code, name } => format!("state({code},{name})"),
        Scaled::Slots(slots) => format!(
            "slots[{}]",
            slots
                .iter()
                .map(|s| format!(
                    "@{}+{}",
                    s.register
                        .map(|r| format!("0x{r:04x}"))
                        .unwrap_or_else(|| "empty".into()),
                    hex(&s.raw)
                ))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Scaled::Block { words_le, words_be } => {
            format!("block(words_le={words_le:?}, words_be={words_be:?})")
        }
    }
}

fn fmt_vreg(d: &DecodedVreg) -> String {
    let name = d.name.unwrap_or("-");
    let value = d.value.as_ref().map(fmt_scaled).unwrap_or_else(|| {
        d.invalid
            .as_ref()
            .map(|i| format!("invalid:{i:?}"))
            .unwrap_or_else(|| "None".into())
    });
    format!(
        "0x{:04x} name={name:?} decoder={:?} confidence={} value={value}",
        d.register,
        d.decoder,
        match d.confidence {
            Confidence::Confirmed => "confirmed",
            Confidence::Candidate => "candidate",
        }
    )
}

fn fmt_response(r: &Response) -> String {
    match r {
        Response::DeviceList { devices } => format!("DeviceList({devices:?})"),
        Response::Response {
            instance,
            opcode,
            code,
        } => {
            format!(
                "Response(instance={instance}, opcode={opcode}, code={}({}))",
                code.name(),
                code_is(code)
            )
        }
        Response::Value {
            instance,
            register,
            data,
        } => {
            format!(
                "Value(instance={instance}, register=0x{register:04x}, raw={})",
                hex(data)
            )
        }
        Response::ValueResponse {
            instance,
            register,
            code,
        } => {
            format!(
                "ValueResponse(instance={instance}, register=0x{register:04x}, code={})",
                code.name()
            )
        }
        Response::PathList {
            instance,
            compressed,
        } => {
            format!(
                "PathList(instance={instance}, blob={} bytes)",
                compressed.len()
            )
        }
        Response::NewPath {
            instance,
            path_index,
            path,
        } => {
            format!("NewPath(instance={instance}, index={path_index}, path={path:?})")
        }
        Response::PathValue {
            instance,
            path_index,
            value,
        } => {
            format!(
                "PathValue(instance={instance}, index={path_index}, value={})",
                fmt_item(value)
            )
        }
        Response::PathResponse {
            instance,
            path_index,
            code,
        } => {
            format!(
                "PathResponse(instance={instance}, index={path_index}, code={})",
                code.name()
            )
        }
        Response::Unknown { opcode } => format!("Unknown(opcode={opcode})"),
    }
}

fn code_is(c: &victron_protocol::response::ResponseCode) -> u8 {
    match c {
        victron_protocol::response::ResponseCode::Ok => 0,
        victron_protocol::response::ResponseCode::Unknown1 => 1,
        victron_protocol::response::ResponseCode::Rejected => 2,
        victron_protocol::response::ResponseCode::Other(v) => *v,
    }
}

fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "fixtures/protocol".into());
    let dir = Path::new(&dir);
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("fixtures dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "bin").unwrap_or(false))
        .map(|e| e.path().file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();

    for name in names {
        let path = dir.join(format!("{name}.bin"));
        let raw = std::fs::read(&path).expect("read fixture");
        println!("== {name} ==");

        let first = raw.first().copied();
        match first {
            Some(0xf7) | Some(0xf8) | Some(0xf9) | Some(0xfa) => {
                let msg = ControlMessage::parse(&raw).expect("control message");
                println!("control: {msg:?}");
                continue;
            }
            _ => {}
        }

        // Data/LastData payloads are CBOR; fall back to the 7-byte control
        // info read only when CBOR decoding fails.
        let mut ra = Reassembler::new();
        let payload = ra.push_last_data(&raw).unwrap().expect("payload");
        let items = match decode_stream(&payload) {
            Ok(items) => items,
            Err(_) if raw.len() == 7 => {
                if let Some(info) = ControlInfo::parse(&raw) {
                    println!("control-info: {info:?}");
                    continue;
                }
                println!("items: error(not cbor, not control-info)");
                continue;
            }
            Err(e) => {
                println!("items: error({e})");
                continue;
            }
        };
        println!(
            "items: [{}]",
            items.iter().map(fmt_item).collect::<Vec<_>>().join(", ")
        );
        match Response::parse_items(&items) {
            Ok(records) => {
                println!(
                    "records: [{}]",
                    records
                        .iter()
                        .map(fmt_response)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                for r in records {
                    if let Some(v) = r.as_vreg_value() {
                        println!("  vreg: {}", fmt_vreg(&v.decode()));
                    }
                }
            }
            Err(e) => println!("records: error({e})"),
        }
    }
}
