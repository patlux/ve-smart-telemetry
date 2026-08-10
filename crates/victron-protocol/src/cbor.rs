//! Concatenated CBOR stream decoding over the observed VE.Smart subset,
//! plus the exact request encoders recovered by the proven prototypes.
//!
//! The device emits **concatenated** CBOR values in one Data/LastData
//! notification (for example two `0x08` Value records back to back). This
//! module decodes every item until the input is exhausted, preserving the
//! proven history-reader behavior now used by `victron-cli read-history`.
//!
//! [minicbor] provides the bounds-checked low-level parser; this module adds
//! the generic value tree (`Item`), a depth/item budget, and per-item size
//! limits on top of it.
//!
//! [minicbor]: https://docs.rs/minicbor

use minicbor::data::Type;
use minicbor::Decoder;

use crate::ProtocolError;

/// Maximum CBOR nesting depth: the root item is depth 0 and each container
/// level (array/map/tag) adds 1. A value nested deeper than `MAX_DEPTH`
/// container levels is rejected with [`ProtocolError::DepthLimit`].
pub const MAX_DEPTH: u8 = 16;
/// Maximum number of CBOR items per decoded stream (budget). Every decoded
/// item counts — container elements, map keys and values, and tag payloads
/// each consume one unit. Exceeding the budget yields
/// [`ProtocolError::ItemLimit`].
pub const MAX_ITEMS: u32 = 4096;
/// Maximum bytes/text size of a single decoded string/bytes item.
pub const MAX_STRING_BYTES: usize = 64 * 1024;
/// Maximum number of elements in a single decoded array (or map entries).
pub const MAX_ARRAY_LEN: usize = 65_536;

/// A decoded CBOR value. Mirrors the value shapes the Python decoders return
/// (ints, byte strings, text, arrays, floats, bools, simple values, tags).
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// CBOR major 0 (unsigned int).
    UInt(u64),
    /// CBOR major 1 (negative int, value is the decoded signed number).
    Int(i64),
    /// CBOR major 2 (byte string; indefinite chunks are concatenated).
    Bytes(Vec<u8>),
    /// CBOR major 3 (text string; indefinite chunks are concatenated).
    Text(String),
    /// CBOR major 4 (array; definite or indefinite).
    Array(Vec<Item>),
    /// CBOR major 5 (map) — supported for QVariant values, not used by the
    /// observed opcode records.
    Map(Vec<(Item, Item)>),
    /// CBOR major 6 (tag) — the tag value and its single wrapped item.
    Tag(u64, Box<Item>),
    /// `false` / `true`.
    Bool(bool),
    /// `null`.
    Null,
    /// `undefined` (simple value 23).
    Undefined,
    /// Other simple value (major 7, ai 0..19, 24..31).
    Simple(u8),
    /// Half/float/double, all widened to `f64`.
    Float(f64),
}

/// Streaming decoder over a byte slice with depth/item/size bounds.
pub struct StreamDecoder<'a> {
    dec: Decoder<'a>,
    budget: u32,
}

impl<'a> StreamDecoder<'a> {
    /// Create a decoder for `data`.
    pub fn new(data: &'a [u8]) -> Self {
        StreamDecoder {
            dec: Decoder::new(data),
            budget: MAX_ITEMS,
        }
    }

    /// Current decode position in the input.
    pub fn position(&self) -> usize {
        self.dec.position()
    }

    /// Length of the input slice.
    pub fn input_len(&self) -> usize {
        self.dec.input().len()
    }

    /// Bytes remaining to decode.
    pub fn remaining(&self) -> usize {
        self.input_len() - self.position()
    }

    /// True when there is nothing left to decode.
    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Decode the next item (or return `Truncated` at end of input).
    pub fn next_item(&mut self) -> Result<Item, ProtocolError> {
        if self.is_empty() {
            return Err(ProtocolError::Truncated);
        }
        self.item_at(0)
    }

    fn item_at(&mut self, depth: u8) -> Result<Item, ProtocolError> {
        // Depth semantics: root item is depth 0; each container level adds 1.
        if depth > MAX_DEPTH {
            return Err(ProtocolError::DepthLimit);
        }
        // Item semantics: every decoded item consumes one budget unit.
        if self.budget == 0 {
            return Err(ProtocolError::ItemLimit);
        }
        self.budget -= 1;
        // depth is bounded by MAX_DEPTH above, so this cannot overflow; the
        // saturating form keeps the arithmetic total even on 8-bit depth.
        let next_depth = depth.saturating_add(1);

        let ty = self.dec.datatype().map_err(ProtocolError::from)?;
        match ty {
            Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
                Ok(Item::UInt(self.dec.u64().map_err(ProtocolError::from)?))
            }
            Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::Int => {
                Ok(Item::Int(self.dec.i64().map_err(ProtocolError::from)?))
            }
            Type::F16 | Type::F32 | Type::F64 => {
                Ok(Item::Float(self.dec.f64().map_err(ProtocolError::from)?))
            }
            Type::Bool => Ok(Item::Bool(self.dec.bool().map_err(ProtocolError::from)?)),
            Type::Null => {
                self.dec.null().map_err(ProtocolError::from)?;
                Ok(Item::Null)
            }
            Type::Undefined => {
                self.dec.undefined().map_err(ProtocolError::from)?;
                Ok(Item::Undefined)
            }
            Type::Simple => Ok(Item::Simple(
                self.dec.simple().map_err(ProtocolError::from)?,
            )),
            Type::Bytes | Type::BytesIndef => {
                let mut buf = Vec::new();
                for chunk in self.dec.bytes_iter().map_err(ProtocolError::from)? {
                    let c = chunk.map_err(ProtocolError::from)?;
                    if buf.len().saturating_add(c.len()) > MAX_STRING_BYTES {
                        return Err(ProtocolError::Cbor("byte string exceeds limit".into()));
                    }
                    buf.extend_from_slice(c);
                }
                Ok(Item::Bytes(buf))
            }
            Type::String | Type::StringIndef => {
                let mut s = String::new();
                for chunk in self.dec.str_iter().map_err(ProtocolError::from)? {
                    let c = chunk.map_err(ProtocolError::from)?;
                    if s.len().saturating_add(c.len()) > MAX_STRING_BYTES {
                        return Err(ProtocolError::Cbor("text string exceeds limit".into()));
                    }
                    s.push_str(c);
                }
                Ok(Item::Text(s))
            }
            Type::Array | Type::ArrayIndef => {
                let len = self.dec.array().map_err(ProtocolError::from)?;
                let mut items = Vec::new();
                match len {
                    Some(n) => {
                        // Compare in u64 first: `n as usize` would truncate
                        // on 32-bit targets (e.g. arm-unknown-linux-gnueabihf).
                        if n > MAX_ARRAY_LEN as u64 {
                            return Err(ProtocolError::Cbor("array exceeds length limit".into()));
                        }
                        for _ in 0..n as usize {
                            items.push(self.item_at(next_depth)?);
                        }
                    }
                    None => loop {
                        if self.dec.datatype().map_err(ProtocolError::from)? == Type::Break {
                            self.dec.skip().map_err(ProtocolError::from)?;
                            break;
                        }
                        if items.len() >= MAX_ARRAY_LEN {
                            return Err(ProtocolError::Cbor("array exceeds length limit".into()));
                        }
                        items.push(self.item_at(next_depth)?);
                    },
                }
                Ok(Item::Array(items))
            }
            Type::Map | Type::MapIndef => {
                let len = self.dec.map().map_err(ProtocolError::from)?;
                let mut entries = Vec::new();
                match len {
                    Some(n) => {
                        if n > MAX_ARRAY_LEN as u64 {
                            return Err(ProtocolError::Cbor("map exceeds length limit".into()));
                        }
                        for _ in 0..n as usize {
                            let k = self.item_at(next_depth)?;
                            let v = self.item_at(next_depth)?;
                            entries.push((k, v));
                        }
                    }
                    None => loop {
                        if self.dec.datatype().map_err(ProtocolError::from)? == Type::Break {
                            self.dec.skip().map_err(ProtocolError::from)?;
                            break;
                        }
                        if entries.len() >= MAX_ARRAY_LEN {
                            return Err(ProtocolError::Cbor("map exceeds length limit".into()));
                        }
                        let k = self.item_at(next_depth)?;
                        let v = self.item_at(next_depth)?;
                        entries.push((k, v));
                    },
                }
                Ok(Item::Map(entries))
            }
            Type::Tag => {
                let tag = self.dec.tag().map_err(ProtocolError::from)?;
                let inner = self.item_at(next_depth)?;
                Ok(Item::Tag(tag.as_u64(), Box::new(inner)))
            }
            Type::Break => Err(ProtocolError::Malformed(
                "unexpected CBOR break at top level",
            )),
            Type::Unknown(b) => Err(ProtocolError::Cbor(format!(
                "unsupported CBOR byte 0x{b:02x}"
            ))),
        }
    }
}

/// Decode a concatenated CBOR stream into all of its items.
///
/// Errors on the first malformed/truncated item (the Python implementation
/// appends an error marker and stops; here the caller receives the typed
/// error instead — see `parse_stream` for record-level leniency).
pub fn decode_stream(data: &[u8]) -> Result<Vec<Item>, ProtocolError> {
    let mut d = StreamDecoder::new(data);
    let mut out = Vec::new();
    while !d.is_empty() {
        out.push(d.next_item()?);
    }
    Ok(out)
}

/// Encode a CBOR unsigned int with minimal width (matches the Python
/// `cbor_uint` exactly).
pub fn encode_uint(n: u64) -> Vec<u8> {
    let mut e = minicbor::Encoder::new(Vec::new());
    e.u64(n).expect("u64 always encodable");
    e.into_writer()
}

/// Encode a CBOR signed int with minimal width (matches the Python `cbor_int`).
pub fn encode_int(n: i64) -> Vec<u8> {
    let mut e = minicbor::Encoder::new(Vec::new());
    e.i64(n).expect("i64 always encodable");
    e.into_writer()
}

/// Encode a CBOR array header with minimal width (matches Python `cbor_array_*`).
pub fn encode_array_len(len: usize) -> Vec<u8> {
    let mut e = minicbor::Encoder::new(Vec::new());
    e.array(len as u64).expect("array len always encodable");
    e.into_writer()
}

/// Encode a CBOR byte string with minimal header width.
pub fn encode_bytes(data: &[u8]) -> Vec<u8> {
    let mut e = minicbor::Encoder::new(Vec::new());
    e.bytes(data).expect("bytes always encodable");
    e.into_writer()
}

/// Encode a CBOR text string with minimal header width.
pub fn encode_text(text: &str) -> Vec<u8> {
    let mut e = minicbor::Encoder::new(Vec::new());
    e.str(text).expect("str always encodable");
    e.into_writer()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn decode_unsigned_widths() {
        let d = decode_stream(&hex("000117181818221901001a000100001b0000000100000000")).unwrap();
        assert_eq!(
            d,
            vec![
                Item::UInt(0),
                Item::UInt(1),
                Item::UInt(23),
                Item::UInt(24),
                Item::UInt(34),
                Item::UInt(256),
                Item::UInt(0x10000),
                Item::UInt(0x1_0000_0000),
            ]
        );
    }

    #[test]
    fn decode_negative_int() {
        // 0x20 = -1, 0x2b = -12, 0x3863 = -100, 0x39ffff = -65536
        let d = decode_stream(&hex("202b386339ffff")).unwrap();
        assert_eq!(
            d,
            vec![
                Item::Int(-1),
                Item::Int(-12),
                Item::Int(-100),
                Item::Int(-65536)
            ]
        );
    }

    #[test]
    fn decode_indefinite_array() {
        // DeviceList payload as observed: 02 9f 000001000301ff
        let d = decode_stream(&hex("029f000001000301ff")).unwrap();
        assert_eq!(
            d,
            vec![
                Item::UInt(2),
                Item::Array(vec![
                    Item::UInt(0),
                    Item::UInt(0),
                    Item::UInt(1),
                    Item::UInt(0),
                    Item::UInt(3),
                    Item::UInt(1),
                ]),
            ]
        );
    }

    #[test]
    fn decode_indefinite_bytes_and_text() {
        // 5f 42 0102 43 030405 ff  → b"\x01\x02\x03\x04\x05"
        // 7f 62 6869 64 6a6b6c6d ff → "hijklm"
        let d = decode_stream(&hex("5f42010243030405ff7f626869646a6b6c6dff")).unwrap();
        assert_eq!(
            d,
            vec![
                Item::Bytes(vec![1, 2, 3, 4, 5]),
                Item::Text("hijklm".into())
            ]
        );
    }

    #[test]
    fn decode_floats_bool_null_undefined_simple_tag() {
        // f9 3c00 = 1.0 f16; fa 40490fdb = 3.1415927 f32;
        // fb 4024000000000000 = 10.0 f64; f5 true; f6 null; f7 undefined;
        // c1 01 = tag(1, 1)
        let d = decode_stream(&hex("f93c00fa40490fdbfb4024000000000000f5f6f7c101")).unwrap();
        assert_eq!(
            d,
            vec![
                Item::Float(1.0),
                Item::Float(3.1415927410125732), // f32 widened to f64
                Item::Float(10.0),
                Item::Bool(true),
                Item::Null,
                Item::Undefined,
                Item::Tag(1, Box::new(Item::UInt(1))),
            ]
        );
    }

    #[test]
    fn decode_simple_value() {
        let d = decode_stream(&hex("e0")).unwrap(); // simple(0)
        assert_eq!(d, vec![Item::Simple(0)]);
    }

    #[test]
    fn decode_concatenated_records() {
        // Two Value records back to back (captured: 080319ed8d42ba09080319edbb42000b)
        let d = decode_stream(&hex("080319ed8d42ba09080319edbb42000b")).unwrap();
        assert_eq!(d.len(), 8);
        assert_eq!(d[3], Item::Bytes(vec![0xba, 0x09]));
        assert_eq!(d[7], Item::Bytes(vec![0x00, 0x0b]));
    }

    #[test]
    fn truncated_stream_errors() {
        // byte string claims 2 bytes, only 1 present
        assert_eq!(decode_stream(&hex("42ba")), Err(ProtocolError::Truncated));
        // int claims 4 bytes, none present
        assert_eq!(decode_stream(&hex("1a")), Err(ProtocolError::Truncated));
    }

    #[test]
    fn depth_limit_enforced() {
        // 0x81 * 20 then 0x01 → nesting depth 20 > MAX_DEPTH
        let mut data = vec![0x81; 20];
        data.push(0x01);
        assert_eq!(decode_stream(&data), Err(ProtocolError::DepthLimit));
    }

    #[test]
    fn array_length_limit_enforced() {
        // array header claims 70000 elements
        let data = hex("9a00011170"); // 0x9a 0x00011170 = 70000
        assert_eq!(
            decode_stream(&data),
            Err(ProtocolError::Cbor("array exceeds length limit".into()))
        );
    }

    #[test]
    fn huge_array_length_not_truncated_on_32bit() {
        // 8-byte array header claiming 0x1_0000_0000 elements. On a 32-bit
        // target `n as usize` would truncate to 0 and silently yield an
        // empty array; the u64 comparison must reject it first.
        let data = hex("9b0000000100000000");
        assert_eq!(
            decode_stream(&data),
            Err(ProtocolError::Cbor("array exceeds length limit".into()))
        );
    }

    #[test]
    fn item_budget_enforced() {
        // MAX_ITEMS = 4096: 4096 single-byte uints fit, 4097 do not.
        let ok = vec![0x00u8; MAX_ITEMS as usize];
        assert_eq!(decode_stream(&ok).unwrap().len(), MAX_ITEMS as usize);
        let too_many = vec![0x00u8; MAX_ITEMS as usize + 1];
        assert_eq!(decode_stream(&too_many), Err(ProtocolError::ItemLimit));
    }

    #[test]
    fn break_at_top_level_is_malformed() {
        assert_eq!(
            decode_stream(&hex("ff")),
            Err(ProtocolError::Malformed(
                "unexpected CBOR break at top level"
            ))
        );
    }

    #[test]
    fn encode_widths_match_python_cbor_uint() {
        // Python cbor_uint golden values
        assert_eq!(encode_uint(0), hex("00"));
        assert_eq!(encode_uint(23), hex("17"));
        assert_eq!(encode_uint(24), hex("1818"));
        assert_eq!(encode_uint(255), hex("18ff"));
        assert_eq!(encode_uint(256), hex("190100"));
        assert_eq!(encode_uint(0xedbb), hex("19edbb"));
        assert_eq!(encode_uint(0x1_0000), hex("1a00010000"));
        assert_eq!(encode_uint(u64::MAX), hex("1bffffffffffffffff"));
    }

    #[test]
    fn encode_int_matches_python_cbor_int() {
        assert_eq!(encode_int(-1), hex("20"));
        assert_eq!(encode_int(-12), hex("2b"));
        assert_eq!(encode_int(-100), hex("3863"));
        assert_eq!(encode_int(-65536), hex("39ffff"));
        assert_eq!(encode_int(-4294967296), hex("3affffffff"));
        assert_eq!(encode_int(5), hex("05"));
    }

    #[test]
    fn encode_array_header_matches_python() {
        assert_eq!(encode_array_len(0), hex("80"));
        assert_eq!(encode_array_len(1), hex("81"));
        assert_eq!(encode_array_len(11), hex("8b"));
        assert_eq!(encode_array_len(24), hex("9818"));
        assert_eq!(encode_array_len(300), hex("99012c"));
    }
}
