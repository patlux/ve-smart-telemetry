//! Typed response records parsed from concatenated Data/LastData payloads.
//!
//! Record scanning mirrors the proven `decode_path_records()` in
//! `scripts/read-victron-history.py`: a sliding window over the decoded CBOR
//! items recognizes known opcode shapes and skips unrecognized items, so
//! mixed/unknown streams stay robust. Values that are not part of a known
//! record surface as [`Response::Unknown`].

use crate::cbor::{decode_stream, Item};
use crate::opcode::InOpcode;
use crate::ProtocolError;

/// Interpret an `Item` as a signed integer (accepting CBOR unsigned values
/// that fit, since non-negative indexes arrive as unsigned CBOR).
fn as_i64(item: Option<&Item>) -> Option<i64> {
    match item {
        Some(Item::Int(v)) => Some(*v),
        Some(Item::UInt(v)) => i64::try_from(*v).ok(),
        _ => None,
    }
}

/// Interpret an `Item` as a response code: a non-negative integer that fits
/// in `u8`. Codes outside `0..=255` are rejected (never wrapped into a
/// misleading small code).
fn as_response_code(item: Option<&Item>) -> Option<u8> {
    match as_i64(item) {
        Some(code) if (0..=255).contains(&code) => Some(code as u8),
        _ => None,
    }
}

/// Response code carried by `0x07`/`0x09`/`0x10` records.
///
/// Names follow `RESPONSE_NAMES` in the Python history reader:
/// `0` = ok, `1` = unknown-1, `2` = rejected-or-unsupported. The tested
/// charger answers `getPathList`/`getPathValues` with `2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResponseCode {
    /// `0` — request accepted.
    Ok = 0,
    /// `1` — observed, meaning unknown.
    Unknown1 = 1,
    /// `2` — rejected or unsupported by this device/firmware.
    Rejected = 2,
    /// Any other code observed in the wild.
    Other(
        /// The raw wire code.
        u8,
    ),
}

impl ResponseCode {
    /// Map a wire code byte.
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => ResponseCode::Ok,
            1 => ResponseCode::Unknown1,
            2 => ResponseCode::Rejected,
            other => ResponseCode::Other(other),
        }
    }

    /// Whether the code signals success.
    pub fn is_ok(&self) -> bool {
        matches!(self, ResponseCode::Ok)
    }

    /// Stable short name for diagnostics/labels.
    pub fn name(&self) -> &'static str {
        match self {
            ResponseCode::Ok => "ok",
            ResponseCode::Unknown1 => "unknown-1",
            ResponseCode::Rejected => "rejected-or-unsupported",
            ResponseCode::Other(v) => match v {
                0 => "ok",
                _ => "unknown",
            },
        }
    }
}

/// A typed inbound VE.Smart response record.
#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    /// `0x02` DeviceList — raw unsigned pairs.
    DeviceList {
        /// Raw unsigned pairs; exact semantics are candidate (the tested
        /// charger returned `[0, 0, 1, 0, 3, 1]` and the working reader
        /// subscribed to instance `3`).
        devices: Vec<(u64, u64)>,
    },
    /// `0x07` Response — ack/reject for an earlier request
    /// (instance, request opcode, code).
    Response {
        /// Instance the reply refers to (note: captured values include `0`,
        /// `9`, `10` even though the app subscribed to `3`).
        instance: u64,
        /// The outbound opcode being answered.
        opcode: u64,
        /// Result code.
        code: ResponseCode,
    },
    /// `0x08` Value — raw VREG payload for one register. Feed
    /// `crate::vreg::VregValue::decode` for scaling.
    Value {
        /// Instance the value belongs to.
        instance: u64,
        /// VREG register id.
        register: u16,
        /// Raw little-endian payload bytes (unscaled).
        data: Vec<u8>,
    },
    /// `0x09` ValueResponse — ack/reject for a single VREG.
    ValueResponse {
        /// Instance.
        instance: u64,
        /// VREG register id.
        register: u16,
        /// Result code.
        code: ResponseCode,
    },
    /// `0x0d` PathList — compressed path list blob
    /// (Qt `qCompress`/zlib payload; not inflated by this crate).
    PathList {
        /// Instance.
        instance: u64,
        /// Compressed blob bytes.
        compressed: Vec<u8>,
    },
    /// `0x0e` NewPath — one path registered by the device.
    NewPath {
        /// Instance.
        instance: u64,
        /// Device-defined path index.
        path_index: i64,
        /// Path text (for example `/Pv/V`).
        path: String,
    },
    /// `0x0f` PathValue — one path value (generic CBOR QVariant).
    PathValue {
        /// Instance.
        instance: u64,
        /// Device-defined path index.
        path_index: i64,
        /// Decoded value (int/float/text/bool/bytes/array/...).
        value: Item,
    },
    /// `0x10` PathResponse — ack/reject for a path operation.
    PathResponse {
        /// Instance.
        instance: u64,
        /// Device-defined path index.
        path_index: i64,
        /// Result code.
        code: ResponseCode,
    },
    /// A recognized opcode with a malformed shape, or an unrecognized
    /// opcode (the reference logs "Received unknown data opcode").
    Unknown {
        /// The CBOR opcode value.
        opcode: u64,
    },
}

impl Response {
    /// Decode a complete payload (one LastData finalization) into records.
    pub fn parse_stream(payload: &[u8]) -> Result<Vec<Response>, ProtocolError> {
        let items = decode_stream(payload)?;
        Self::parse_items(&items)
    }

    /// Parse records from already-decoded CBOR items (sliding-window scan).
    ///
    /// Unrecognized opcodes and non-opcode items are skipped silently (the
    /// proven Python scanner advances one item without emitting a record);
    /// recognized opcodes with a malformed shape surface as
    /// [`Response::Unknown`] and consume their **expected record width**, so
    /// the record's parameter items are never re-scanned as opcodes.
    pub fn parse_items(items: &[Item]) -> Result<Vec<Response>, ProtocolError> {
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < items.len() {
            let opcode = match &items[i] {
                Item::UInt(u) => *u,
                _ => {
                    i += 1;
                    continue;
                }
            };
            // Expected item width of each known record (opcode + parameters)
            // is used inline below: a malformed record consumes its full
            // width so its parameters are never re-scanned as opcodes.
            let record = match opcode {
                // 0x02 DeviceList: opcode + array of unsigned pairs (2 items).
                // A malformed array (odd entries or non-uint elements) is
                // rejected wholesale — never a partial valid list.
                0x02 => match items.get(i + 1) {
                    Some(Item::Array(entries)) => {
                        let mut pairs = Vec::new();
                        let mut e = entries.iter();
                        let mut ok = true;
                        loop {
                            match (e.next(), e.next()) {
                                (Some(Item::UInt(ai)), Some(Item::UInt(bi))) => {
                                    pairs.push((*ai, *bi))
                                }
                                (None, None) => break,
                                _ => {
                                    ok = false;
                                    break;
                                }
                            }
                        }
                        if ok {
                            (Some(Response::DeviceList { devices: pairs }), 2)
                        } else {
                            (Some(Response::Unknown { opcode }), 2)
                        }
                    }
                    _ => (Some(Response::Unknown { opcode }), 2),
                },
                // 0x07 Response: opcode + instance + req-opcode + code (4 items).
                0x07 => match (
                    items.get(i + 1),
                    items.get(i + 2),
                    as_response_code(items.get(i + 3)),
                ) {
                    (Some(Item::UInt(inst)), Some(Item::UInt(req)), Some(code)) => (
                        Some(Response::Response {
                            instance: *inst,
                            opcode: *req,
                            code: ResponseCode::from_u8(code),
                        }),
                        4,
                    ),
                    _ => (Some(Response::Unknown { opcode }), 4),
                },
                // 0x08 Value: opcode + instance + register + bytes (4 items).
                0x08 => match (items.get(i + 1), items.get(i + 2), items.get(i + 3)) {
                    (Some(Item::UInt(inst)), Some(Item::UInt(reg)), Some(Item::Bytes(data))) => {
                        let register = u16::try_from(*reg)
                            .map_err(|_| ProtocolError::RegisterOutOfRange(*reg))?;
                        (
                            Some(Response::Value {
                                instance: *inst,
                                register,
                                data: data.clone(),
                            }),
                            4,
                        )
                    }
                    _ => (Some(Response::Unknown { opcode }), 4),
                },
                // 0x09 ValueResponse: opcode + instance + register + code (4 items).
                0x09 => match (
                    items.get(i + 1),
                    items.get(i + 2),
                    as_response_code(items.get(i + 3)),
                ) {
                    (Some(Item::UInt(inst)), Some(Item::UInt(reg)), Some(code)) => {
                        let register = u16::try_from(*reg)
                            .map_err(|_| ProtocolError::RegisterOutOfRange(*reg))?;
                        (
                            Some(Response::ValueResponse {
                                instance: *inst,
                                register,
                                code: ResponseCode::from_u8(code),
                            }),
                            4,
                        )
                    }
                    _ => (Some(Response::Unknown { opcode }), 4),
                },
                // 0x0d PathList: opcode + instance + bytes (3 items).
                0x0d => match (items.get(i + 1), items.get(i + 2)) {
                    (Some(Item::UInt(inst)), Some(Item::Bytes(blob))) => (
                        Some(Response::PathList {
                            instance: *inst,
                            compressed: blob.clone(),
                        }),
                        3,
                    ),
                    _ => (Some(Response::Unknown { opcode }), 3),
                },
                // 0x0e NewPath: opcode + instance + index + text (4 items).
                0x0e => match (items.get(i + 1), as_i64(items.get(i + 2)), items.get(i + 3)) {
                    (Some(Item::UInt(inst)), Some(idx), Some(Item::Text(path))) => (
                        Some(Response::NewPath {
                            instance: *inst,
                            path_index: idx,
                            path: path.clone(),
                        }),
                        4,
                    ),
                    _ => (Some(Response::Unknown { opcode }), 4),
                },
                // 0x0f PathValue: opcode + instance + index + any value (4 items).
                0x0f => match (items.get(i + 1), as_i64(items.get(i + 2)), items.get(i + 3)) {
                    (Some(Item::UInt(inst)), Some(idx), Some(value)) => (
                        Some(Response::PathValue {
                            instance: *inst,
                            path_index: idx,
                            value: value.clone(),
                        }),
                        4,
                    ),
                    _ => (Some(Response::Unknown { opcode }), 4),
                },
                // 0x10 PathResponse: opcode + instance + index + code (4 items).
                0x10 => match (
                    items.get(i + 1),
                    as_i64(items.get(i + 2)),
                    as_response_code(items.get(i + 3)),
                ) {
                    (Some(Item::UInt(inst)), Some(idx), Some(code)) => (
                        Some(Response::PathResponse {
                            instance: *inst,
                            path_index: idx,
                            code: ResponseCode::from_u8(code),
                        }),
                        4,
                    ),
                    _ => (Some(Response::Unknown { opcode }), 4),
                },
                // Unrecognized opcodes are skipped silently (Python parity).
                _ => (None, 1),
            };
            match record {
                (Some(record), consumed) => {
                    out.push(record);
                    i += consumed;
                }
                (None, consumed) => i += consumed,
            }
        }
        Ok(out)
    }

    /// The inbound opcode of this record, when it is a known opcode.
    ///
    /// An [`Response::Unknown`] opcode is reported only when it fits in a
    /// `u8`; a larger value is never truncated into a misleading known
    /// opcode and yields `None`.
    pub fn opcode(&self) -> Option<InOpcode> {
        let v = match self {
            Response::DeviceList { .. } => InOpcode::DeviceList,
            Response::Response { .. } => InOpcode::Response,
            Response::Value { .. } => InOpcode::Value,
            Response::ValueResponse { .. } => InOpcode::ValueResponse,
            Response::PathList { .. } => InOpcode::PathList,
            Response::NewPath { .. } => InOpcode::NewPath,
            Response::PathValue { .. } => InOpcode::PathValue,
            Response::PathResponse { .. } => InOpcode::PathResponse,
            Response::Unknown { opcode } => {
                return u8::try_from(*opcode).ok().and_then(InOpcode::from_u8)
            }
        };
        Some(v)
    }

    /// If this is a `0x08` Value record, convert it to a [`crate::vreg::VregValue`].
    pub fn as_vreg_value(&self) -> Option<crate::vreg::VregValue> {
        match self {
            Response::Value { register, data, .. } => {
                Some(crate::vreg::VregValue::new(*register, data.clone()))
            }
            _ => None,
        }
    }
}
