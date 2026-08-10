//! Integration tests for the `response` module.
//!
//! Moved out of the module to keep library files under the 500-line guidance.

use victron_protocol::{Item, ProtocolError, Response, ResponseCode};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn device_list_from_capture() {
    let responses = Response::parse_stream(&hex("029f000001000301ff")).unwrap();
    assert_eq!(
        responses,
        vec![Response::DeviceList {
            devices: vec![(0, 0), (1, 0), (3, 1)]
        }]
    );
}

#[test]
fn subscribe_ok_response_from_capture() {
    let responses = Response::parse_stream(&hex("07000300")).unwrap();
    assert_eq!(
        responses,
        vec![Response::Response {
            instance: 0,
            opcode: 3,
            code: ResponseCode::Ok
        }]
    );
}

#[test]
fn subscribe_rejected_from_capture() {
    let responses = Response::parse_stream(&hex("07090302")).unwrap();
    assert_eq!(
        responses,
        vec![Response::Response {
            instance: 9,
            opcode: 3,
            code: ResponseCode::Rejected
        }]
    );
}

#[test]
fn pathlist_rejected_from_capture() {
    let responses = Response::parse_stream(&hex("070a0302")).unwrap();
    assert_eq!(
        responses,
        vec![Response::Response {
            instance: 10,
            opcode: 3,
            code: ResponseCode::Rejected
        }]
    );
}

#[test]
fn single_value_record_from_capture() {
    let responses = Response::parse_stream(&hex("080319edbb42f30a")).unwrap();
    assert_eq!(
        responses,
        vec![Response::Value {
            instance: 3,
            register: 0xedbb,
            data: vec![0xf3, 0x0a]
        }]
    );
}

#[test]
fn concatenated_value_records_from_capture() {
    // Two records in one stream (captured).
    let responses = Response::parse_stream(&hex("080319ed8d42ba09080319edbb42000b")).unwrap();
    assert_eq!(
        responses,
        vec![
            Response::Value {
                instance: 3,
                register: 0xed8d,
                data: vec![0xba, 0x09]
            },
            Response::Value {
                instance: 3,
                register: 0xedbb,
                data: vec![0x00, 0x0b]
            },
        ]
    );
}

#[test]
fn five_concatenated_records_from_capture() {
    let responses = Response::parse_stream(&hex(
        "080319ed8f420100080319ed8c443c000000080319ec8a420100080319edbc4464000000080319edbb426e0e",
    ))
    .unwrap();
    assert_eq!(responses.len(), 5);
    assert_eq!(
        responses[0],
        Response::Value {
            instance: 3,
            register: 0xed8f,
            data: vec![0x01, 0x00]
        }
    );
    assert_eq!(
        responses[1],
        Response::Value {
            instance: 3,
            register: 0xed8c,
            data: vec![0x3c, 0, 0, 0]
        }
    );
    assert_eq!(
        responses[2],
        Response::Value {
            instance: 3,
            register: 0xec8a,
            data: vec![0x01, 0x00]
        }
    );
    assert_eq!(
        responses[3],
        Response::Value {
            instance: 3,
            register: 0xedbc,
            data: vec![0x64, 0, 0, 0]
        }
    );
    assert_eq!(
        responses[4],
        Response::Value {
            instance: 3,
            register: 0xedbb,
            data: vec![0x6e, 0x0e]
        }
    );
}

#[test]
fn long_byte_string_value_from_capture() {
    // 0xec20 with a 32-byte payload (0x58 0x20 header).
    let responses = Response::parse_stream(&hex(
        "080319ec2058208dedffffffffffffecedffffffffffff3eecffffffffffff8cedffffffffffff",
    ))
    .unwrap();
    match &responses[0] {
        Response::Value { register, data, .. } => {
            assert_eq!(*register, 0xec20);
            assert_eq!(data.len(), 32);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn history_block_records_from_capture() {
    let responses = Response::parse_stream(&hex(
        "080319104f5822010012000000f7dc0100f7dc01006b14e80a1e0100ffffffffffffffffffffffffff",
    ))
    .unwrap();
    match &responses[0] {
        Response::Value { register, data, .. } => {
            assert_eq!(*register, 0x104f);
            assert_eq!(data.len(), 34);
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn synthetic_path_records_round_trip() {
    // Synthetic (the tested device rejects the path API, so no live path
    // records exist yet): one each of 0x0d/0x0e/0x0f/0x10 concatenated,
    // built with the crate's own encoders.
    use victron_protocol::cbor::{encode_bytes, encode_int, encode_text, encode_uint};
    let mut payload = Vec::new();
    payload.extend(encode_uint(0x0d));
    payload.extend(encode_uint(3));
    payload.extend(encode_bytes(&[0x00, 0x00, 0x00, 0x01, 0xca, 0xfe]));
    payload.extend(encode_uint(0x0e));
    payload.extend(encode_uint(3));
    payload.extend(encode_int(0x61)); // positive index encodes as CBOR uint
    payload.extend(encode_text("/Pv/V"));
    payload.extend(encode_uint(0x0f));
    payload.extend(encode_uint(3));
    payload.extend(encode_int(0x20));
    payload.extend(hex("fa40490fdb")); // 3.1415927 f32
    payload.extend(encode_uint(0x10));
    payload.extend(encode_uint(3));
    payload.extend(encode_int(0x21));
    payload.extend(encode_uint(0));

    let responses = Response::parse_stream(&payload).unwrap();
    assert_eq!(
        responses,
        vec![
            Response::PathList {
                instance: 3,
                compressed: vec![0x00, 0x00, 0x00, 0x01, 0xca, 0xfe]
            },
            Response::NewPath {
                instance: 3,
                path_index: 0x61,
                path: "/Pv/V".into()
            },
            Response::PathValue {
                instance: 3,
                path_index: 0x20,
                value: Item::Float(3.1415927410125732)
            },
            Response::PathResponse {
                instance: 3,
                path_index: 0x21,
                code: ResponseCode::Ok
            },
        ]
    );
}

#[test]
fn unknown_opcodes_skipped_silently() {
    // 0x06 setValues opcode on receive is unrecognized inbound and is
    // skipped silently (Python parity) — never interpreted as a write.
    let responses = Response::parse_stream(&hex("060003")).unwrap();
    assert_eq!(responses, Vec::<Response>::new());
}

#[test]
fn malformed_known_opcode_surfaces_as_unknown() {
    // 0x08 with a UInt where the byte string should be → Unknown{8},
    // then the remaining items are skipped.
    let responses = Response::parse_stream(&hex("080319edbb01")).unwrap();
    assert_eq!(responses, vec![Response::Unknown { opcode: 8 }]);
}

#[test]
fn non_opcode_items_are_skipped() {
    // A stray byte-string followed by a valid Value record; the stray
    // item is skipped silently (matching the Python scanner).
    let responses = Response::parse_stream(&hex("420102080319edbb42f30a")).unwrap();
    assert_eq!(
        responses,
        vec![Response::Value {
            instance: 3,
            register: 0xedbb,
            data: vec![0xf3, 0x0a]
        }]
    );
}

#[test]
fn truncated_payload_errors() {
    assert_eq!(
        Response::parse_stream(&hex("080319edbb42f3")),
        Err(ProtocolError::Truncated)
    );
}

#[test]
fn register_out_of_range_errors() {
    // register encoded as u64 > 0xffff
    assert_eq!(
        Response::parse_stream(&hex("081a000000031b0000ffff00000000420102")),
        Err(ProtocolError::RegisterOutOfRange(0xffff_0000_0000))
    );
}

#[test]
fn response_code_names() {
    assert_eq!(ResponseCode::from_u8(0).name(), "ok");
    assert_eq!(ResponseCode::from_u8(2).name(), "rejected-or-unsupported");
    assert!(ResponseCode::Ok.is_ok());
    assert!(!ResponseCode::Rejected.is_ok());
}

#[test]
fn response_code_outside_u8_is_not_wrapped() {
    // 0x07 with code 256: must NOT wrap to 0 (ok). It surfaces as Unknown.
    let responses = Response::parse_stream(&hex("070305190100")).unwrap();
    assert_eq!(responses, vec![Response::Unknown { opcode: 7 }]);
    // 0x10 with a negative code is malformed too.
    let responses = Response::parse_stream(&hex("10032120")).unwrap();
    assert_eq!(responses, vec![Response::Unknown { opcode: 0x10 }]);
}

#[test]
fn unknown_opcode_reporting_is_bounded() {
    use victron_protocol::opcode::InOpcode;
    // A large unknown opcode must not be truncated into a known u8.
    let big = Response::Unknown { opcode: 0x108 }; // 0x108 as u8 would be 8 (Value)
    assert_eq!(big.opcode(), None);
    // A small unknown opcode that is a known inbound opcode reports it.
    let small = Response::Unknown { opcode: 8 };
    assert_eq!(small.opcode(), Some(InOpcode::Value));
}

#[test]
fn malformed_device_list_is_rejected_wholesale() {
    // Odd number of entries: no partial list.
    let responses = Response::parse_stream(&hex("0283010203")).unwrap();
    assert_eq!(responses, vec![Response::Unknown { opcode: 2 }]);
    // Non-uint element: no partial list.
    let responses = Response::parse_stream(&hex("028201420102")).unwrap();
    assert_eq!(responses, vec![Response::Unknown { opcode: 2 }]);
    // Even, all-uint array still parses (0x84 = array(4)).
    let responses = Response::parse_stream(&hex("028400010003")).unwrap();
    assert_eq!(
        responses,
        vec![Response::DeviceList {
            devices: vec![(0, 1), (0, 3)]
        }]
    );
}

#[test]
fn malformed_record_parameters_are_not_rescanned_as_opcodes() {
    // 0x08 with a UInt where the byte string should be. The following
    // items (0x0d, 3, bytes) are the malformed record's parameters and must
    // NOT be re-scanned into a spurious PathList record.
    let responses = Response::parse_stream(&hex("080319edbb0d03420102")).unwrap();
    assert_eq!(responses, vec![Response::Unknown { opcode: 8 }]);
}
