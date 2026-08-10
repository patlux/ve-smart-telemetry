//! Control characteristic protocol: raw negotiation opcodes and the initial
//! 7-byte control info read. The Control characteristic is **not** CBOR; it
//! uses single-byte opcodes (`fa`, `f9`, `f8`, `f7`) per
//! `analysis/victronconnect-protocol-reference.md` §5.
//!
//! Observed session handshake (from the live probe logs):
//!
//! 1. read Control characteristic → 7-byte [`ControlInfo`]
//!    (captured `00 04 00 01 de 4a 00`);
//! 2. write `fa 80 ff` (chunk-size negotiation);
//! 3. write `f9 80` (ready-to-receive credit);
//! 4. device notifies `f9 01` (free-chunk count) and, on errors,
//!    `f7 03 00` (error code `0x0003`).
//!
//! ## Exact-length contract
//!
//! [`ControlMessage::parse`] parses **one exact message**: known opcodes
//! accept exactly their observed lengths (`f8`=1, `f9`=2, `fa`=3, `f7`=2 or
//! 3) and reject trailing bytes. `f7` is the only variable-width message
//! (one- or two-byte little-endian error code).
//!
//! [`ControlMessage::parse_stream`] therefore cannot unambiguously split a
//! concatenated buffer containing a 2-byte `f7` (the following byte could be
//! the second error-code byte or the next message). The provably safe
//! contract: in a concatenated stream `f7` is **exactly 3 bytes** (the
//! observed wire form `f7 03 00`); a 2-byte `f7` in a stream is an explicit
//! [`crate::ProtocolError::Malformed`] error rather than a guess.

/// Chunk-size negotiation write (`fa 80 ff`) sent by the app after reading
/// the control info, as observed in the live probes.
pub const NEGOTIATE_CHUNK_SIZE: [u8; 3] = [0xfa, 0x80, 0xff];

/// Ready-to-receive credit write (`f9 80`), sent right after the chunk-size
/// negotiation in the observed session.
pub const READY_TO_RECEIVE_80: [u8; 2] = [0xf9, 0x80];

/// The two negotiation writes in session order, as observed.
pub const NEGOTIATION_WRITES: [&[u8]; 2] = [&NEGOTIATE_CHUNK_SIZE, &READY_TO_RECEIVE_80];

/// The app clamps the negotiated chunk size to at least 20 bytes
/// (reference §5.1: "clamped to at least `0x14` / 20").
pub const MIN_ATT_CHUNK_SIZE: usize = 20;

/// A decoded Control characteristic message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlMessage {
    /// `f7` error notification from the device, with a one- or two-byte
    /// error code (little-endian when two bytes; captured `f7 03 00` → `3`).
    Error {
        /// Little-endian error code.
        code: u16,
    },
    /// `f8` clear/reset of the device's accumulated CBOR buffer.
    ClearBuffer,
    /// `f9 <n>` ready-to-receive notification/credit; `n` free chunks.
    ReadyToReceive {
        /// Number of free chunks announced/credited.
        free_chunks: u8,
    },
    /// `fa <a> <b>` chunk-size/CBOR-size negotiation write.
    ChunkSize {
        /// First negotiation byte.
        a: u8,
        /// Second negotiation byte.
        b: u8,
    },
    /// Any other opcode byte, kept bounded for diagnostics.
    Unknown {
        /// The unrecognized opcode byte.
        opcode: u8,
        /// Total bytes of the message (for diagnostics).
        len: usize,
    },
}

impl ControlMessage {
    /// The opcode byte of this message.
    pub fn opcode(&self) -> u8 {
        match self {
            ControlMessage::Error { .. } => 0xf7,
            ControlMessage::ClearBuffer => 0xf8,
            ControlMessage::ReadyToReceive { .. } => 0xf9,
            ControlMessage::ChunkSize { .. } => 0xfa,
            ControlMessage::Unknown { opcode, .. } => *opcode,
        }
    }

    /// Encode this message to its wire bytes.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            ControlMessage::Error { code } => {
                let mut v = Vec::with_capacity(3);
                v.push(0xf7);
                v.extend_from_slice(&code.to_le_bytes());
                v
            }
            ControlMessage::ClearBuffer => vec![0xf8],
            ControlMessage::ReadyToReceive { free_chunks } => vec![0xf9, *free_chunks],
            ControlMessage::ChunkSize { a, b } => vec![0xfa, *a, *b],
            ControlMessage::Unknown { opcode, .. } => vec![*opcode],
        }
    }

    /// Parse exactly one control message from `data`.
    ///
    /// Known opcodes accept exactly their observed lengths (`f8`=1, `f9`=2,
    /// `fa`=3, `f7`=2 or 3) and **reject trailing bytes**; unrecognized
    /// opcodes surface as [`ControlMessage::Unknown`]. Returns `Malformed`
    /// for a known opcode with any other length.
    pub fn parse(data: &[u8]) -> Result<ControlMessage, crate::ProtocolError> {
        let op = *data.first().ok_or(crate::ProtocolError::Truncated)?;
        Ok(match op {
            0xf7 => match data.len() {
                2 => ControlMessage::Error {
                    code: data[1] as u16,
                },
                3 => ControlMessage::Error {
                    code: u16::from_le_bytes([data[1], data[2]]),
                },
                _ => {
                    return Err(crate::ProtocolError::Malformed(
                        "f7 must be exactly 2 or 3 bytes",
                    ))
                }
            },
            0xf8 => {
                if data.len() != 1 {
                    return Err(crate::ProtocolError::Malformed("f8 must be exactly 1 byte"));
                }
                ControlMessage::ClearBuffer
            }
            0xf9 => {
                if data.len() != 2 {
                    return Err(crate::ProtocolError::Malformed(
                        "f9 must be exactly 2 bytes",
                    ));
                }
                ControlMessage::ReadyToReceive {
                    free_chunks: data[1],
                }
            }
            0xfa => {
                if data.len() != 3 {
                    return Err(crate::ProtocolError::Malformed(
                        "fa must be exactly 3 bytes",
                    ));
                }
                ControlMessage::ChunkSize {
                    a: data[1],
                    b: data[2],
                }
            }
            other => ControlMessage::Unknown {
                opcode: other,
                len: data.len(),
            },
        })
    }

    /// Parse a concatenated Control buffer into all contained messages.
    ///
    /// Provably safe contract: `f7` is **exactly 3 bytes** in a stream (the
    /// observed wire form), so a 2-byte `f7` followed by another message is
    /// unambiguously a 3-byte error message and a 2-byte `f7` at the end of
    /// the buffer is an explicit `Malformed` error. Unknown opcodes consume
    /// one byte so a run of unknown bytes cannot loop.
    pub fn parse_stream(data: &[u8]) -> Result<Vec<ControlMessage>, crate::ProtocolError> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < data.len() {
            let op = *data.get(i).ok_or(crate::ProtocolError::Truncated)?;
            let width = match op {
                0xf7 => 3,
                0xf8 => 1,
                0xf9 => 2,
                0xfa => 3,
                _ => 1,
            };
            let end = i + width;
            if end > data.len() {
                return Err(crate::ProtocolError::Malformed(
                    "truncated control message in concatenated stream",
                ));
            }
            out.push(ControlMessage::parse(&data[i..end])?);
            i = end;
        }
        Ok(out)
    }
}

/// The 7-byte payload read from the Control characteristic before
/// negotiation (captured `00 04 00 01 de 4a 00`).
///
/// Field semantics are **inferred** from the reference's `processControlData`
/// object layout (§5.1) and are marked as such; only the byte positions are
/// confirmed by captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlInfo {
    /// Byte 0: negotiated/protocol byte (inferred).
    pub protocol_byte: u8,
    /// Bytes 1-2 (little-endian): two-byte field (inferred).
    pub field_u16: u16,
    /// Byte 3: write/CBOR payload size limit (inferred).
    pub payload_size_limit: u8,
    /// Byte 4: one-byte field (inferred).
    pub field_byte: u8,
    /// Byte 5: free-chunk count / window (inferred).
    pub free_chunks: u8,
    /// Byte 6: trailing field (inferred).
    pub tail: u8,
}

impl ControlInfo {
    /// Expected payload length (7 bytes).
    pub const LEN: usize = 7;

    /// Parse a control-info payload. Returns `None` unless the payload is
    /// **exactly** 7 bytes: shorter reads are rejected (the reference
    /// rejects them with a length diagnostic) and trailing data is rejected
    /// rather than silently ignored.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() != Self::LEN {
            return None;
        }
        Some(ControlInfo {
            protocol_byte: data[0],
            field_u16: u16::from_le_bytes([data[1], data[2]]),
            payload_size_limit: data[3],
            field_byte: data[4],
            free_chunks: data[5],
            tail: data[6],
        })
    }
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
    fn decode_ready_to_receive() {
        let m = ControlMessage::parse(&hex("f901")).unwrap();
        assert_eq!(m, ControlMessage::ReadyToReceive { free_chunks: 1 });
    }

    #[test]
    fn decode_error_two_bytes_le() {
        let m = ControlMessage::parse(&hex("f70300")).unwrap();
        assert_eq!(m, ControlMessage::Error { code: 0x0003 });
    }

    #[test]
    fn decode_error_one_byte() {
        let m = ControlMessage::parse(&hex("f705")).unwrap();
        assert_eq!(m, ControlMessage::Error { code: 5 });
    }

    #[test]
    fn decode_clear_buffer() {
        let m = ControlMessage::parse(&hex("f8")).unwrap();
        assert_eq!(m, ControlMessage::ClearBuffer);
    }

    #[test]
    fn decode_unknown_opcode() {
        let m = ControlMessage::parse(&hex("fe0102")).unwrap();
        assert_eq!(
            m,
            ControlMessage::Unknown {
                opcode: 0xfe,
                len: 3
            }
        );
    }

    #[test]
    fn truncated_control_errors() {
        assert!(matches!(
            ControlMessage::parse(&[]),
            Err(crate::ProtocolError::Truncated)
        ));
        assert!(matches!(
            ControlMessage::parse(&hex("f9")),
            Err(crate::ProtocolError::Malformed(_))
        ));
        assert!(matches!(
            ControlMessage::parse(&hex("fa80")),
            Err(crate::ProtocolError::Malformed(_))
        ));
    }

    #[test]
    fn trailing_bytes_rejected_for_known_messages() {
        // Exact-length contract: known opcodes reject trailing bytes.
        for raw in ["f801", "f90102", "fa80ff00", "f7", "f7030000"] {
            assert!(
                matches!(
                    ControlMessage::parse(&hex(raw)),
                    Err(crate::ProtocolError::Malformed(_))
                ),
                "{raw} must be rejected"
            );
        }
        // The exact accepted lengths still parse.
        assert_eq!(
            ControlMessage::parse(&hex("f8")).unwrap(),
            ControlMessage::ClearBuffer
        );
        assert_eq!(
            ControlMessage::parse(&hex("f901")).unwrap(),
            ControlMessage::ReadyToReceive { free_chunks: 1 }
        );
        assert_eq!(
            ControlMessage::parse(&hex("fa80ff")).unwrap(),
            ControlMessage::ChunkSize { a: 0x80, b: 0xff }
        );
        assert_eq!(
            ControlMessage::parse(&hex("f703")).unwrap(),
            ControlMessage::Error { code: 3 }
        );
        assert_eq!(
            ControlMessage::parse(&hex("f70300")).unwrap(),
            ControlMessage::Error { code: 3 }
        );
    }

    #[test]
    fn stream_f7_contract_is_exactly_three_bytes() {
        // A 2-byte f7 at the end of a stream is truncated → explicit error.
        assert!(matches!(
            ControlMessage::parse_stream(&hex("f703")),
            Err(crate::ProtocolError::Malformed(_))
        ));
        // Under the 3-byte contract, `f7 03 f8` is unambiguously a 3-byte
        // error message (code 0xf803), not f7(2) + f8.
        let msgs = ControlMessage::parse_stream(&hex("f703f8")).unwrap();
        assert_eq!(msgs, vec![ControlMessage::Error { code: 0xf803 }]);
        // 3-byte f7 in a stream is fine.
        let msgs = ControlMessage::parse_stream(&hex("f70300f8")).unwrap();
        assert_eq!(
            msgs,
            vec![
                ControlMessage::Error { code: 3 },
                ControlMessage::ClearBuffer
            ]
        );
    }

    #[test]
    fn control_info_trailing_data_rejected() {
        // Exactly 7 bytes required; 8 bytes must not silently parse.
        assert_eq!(ControlInfo::parse(&hex("00040001de4a0000")), None);
        assert_eq!(ControlInfo::parse(&hex("00040001de4a")), None);
    }

    #[test]
    fn encode_matches_captured_negotiation() {
        assert_eq!(
            ControlMessage::ChunkSize { a: 0x80, b: 0xff }.encode(),
            hex("fa80ff")
        );
        assert_eq!(
            ControlMessage::ReadyToReceive { free_chunks: 0x80 }.encode(),
            hex("f980")
        );
        assert_eq!(
            ControlMessage::ReadyToReceive { free_chunks: 1 }.encode(),
            hex("f901")
        );
        assert_eq!(ControlMessage::Error { code: 3 }.encode(), hex("f70300"));
        assert_eq!(NEGOTIATION_WRITES, [&hex("fa80ff")[..], &hex("f980")[..]]);
    }

    #[test]
    fn parse_concatenated_control_stream() {
        let msgs = ControlMessage::parse_stream(&hex("f901f70300f8")).unwrap();
        assert_eq!(
            msgs,
            vec![
                ControlMessage::ReadyToReceive { free_chunks: 1 },
                ControlMessage::Error { code: 3 },
                ControlMessage::ClearBuffer,
            ]
        );
    }

    #[test]
    fn parse_control_info_capture() {
        let info = ControlInfo::parse(&hex("00040001de4a00")).unwrap();
        assert_eq!(info.protocol_byte, 0x00);
        assert_eq!(info.field_u16, 0x0004);
        assert_eq!(info.payload_size_limit, 0x01);
        assert_eq!(info.field_byte, 0xde);
        assert_eq!(info.free_chunks, 0x4a);
        assert_eq!(info.tail, 0x00);
    }

    #[test]
    fn short_control_info_rejected() {
        assert_eq!(ControlInfo::parse(&hex("00040001")), None);
    }
}
