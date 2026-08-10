//! Fixture-driven tests over `fixtures/protocol/*.bin`.
//!
//! Provenance is **not** uniform — see `fixtures/protocol/README.md`:
//!
//! * most fixtures are **captured verbatim** wire payloads from the
//!   user-owned MPPT charger (sanitized), with provenance in the README;
//! * `value-history-104f`/`1050` are **captured raw payloads wrapped
//!   synthetically** (the 34-byte VREG payload is captured; the record
//!   wrapper was built with the crate's CBOR encoder);
//! * the `0x0d`-`0x10` path records are **fully synthetic** and live only
//!   in `tests/response.rs`.
//!
//! Expected values below are the verified outputs of the proven Python
//! decoders (`scripts/read-victron-history.py` / `read-victron-live-values.py`)
//! run over the same bytes — see the fixture README for the reference run.

use victron_protocol::cbor::Item;
use victron_protocol::control::{ControlInfo, ControlMessage};
use victron_protocol::frame::Reassembler;
use victron_protocol::request::Request;
use victron_protocol::response::{Response, ResponseCode};
use victron_protocol::vreg::{Confidence, Scaled};

/// Load a fixture's raw bytes (relative to `fixtures/protocol/`).
fn fixture(name: &str) -> &'static [u8] {
    match name {
        "notify-devices-indef" => {
            include_bytes!("../../../fixtures/protocol/notify-devices-indef.bin")
        }
        "ctrl-ready-01" => include_bytes!("../../../fixtures/protocol/ctrl-ready-01.bin"),
        "ctrl-error-0300" => include_bytes!("../../../fixtures/protocol/ctrl-error-0300.bin"),
        "ctrl-info-initial" => include_bytes!("../../../fixtures/protocol/ctrl-info-initial.bin"),
        "value-solar-voltage" => {
            include_bytes!("../../../fixtures/protocol/value-solar-voltage.bin")
        }
        "value-battery-voltage" => {
            include_bytes!("../../../fixtures/protocol/value-battery-voltage.bin")
        }
        "value-load-voltage" => include_bytes!("../../../fixtures/protocol/value-load-voltage.bin"),
        "value-negative-current" => {
            include_bytes!("../../../fixtures/protocol/value-negative-current.bin")
        }
        "value-trend-ec20" => include_bytes!("../../../fixtures/protocol/value-trend-ec20.bin"),
        "value-history-104f" => include_bytes!("../../../fixtures/protocol/value-history-104f.bin"),
        "value-history-1050" => include_bytes!("../../../fixtures/protocol/value-history-1050.bin"),
        "value-concat-two" => include_bytes!("../../../fixtures/protocol/value-concat-two.bin"),
        "value-concat-five" => include_bytes!("../../../fixtures/protocol/value-concat-five.bin"),
        "value-state-0200" => include_bytes!("../../../fixtures/protocol/value-state-0200.bin"),
        "value-device-0202" => include_bytes!("../../../fixtures/protocol/value-device-0202.bin"),
        "value-stat-2001" => include_bytes!("../../../fixtures/protocol/value-stat-2001.bin"),
        "response-subscribe-ok" => {
            include_bytes!("../../../fixtures/protocol/response-subscribe-ok.bin")
        }
        "response-subscribe-reject" => {
            include_bytes!("../../../fixtures/protocol/response-subscribe-reject.bin")
        }
        "response-pathlist-reject" => {
            include_bytes!("../../../fixtures/protocol/response-pathlist-reject.bin")
        }
        "request-getdevices" => include_bytes!("../../../fixtures/protocol/request-getdevices.bin"),
        "request-subscribe3" => include_bytes!("../../../fixtures/protocol/request-subscribe3.bin"),
        "request-getvalues11" => {
            include_bytes!("../../../fixtures/protocol/request-getvalues11.bin")
        }
        "request-pathlist3" => include_bytes!("../../../fixtures/protocol/request-pathlist3.bin"),
        other => panic!("unknown fixture {other}"),
    }
}

fn value(register: u16, data: Vec<u8>) -> Response {
    Response::Value {
        instance: 3,
        register,
        data,
    }
}

fn decoded_vreg(responses: &[Response], want_register: u16) -> victron_protocol::vreg::DecodedVreg {
    let vreg = responses
        .iter()
        .filter_map(Response::as_vreg_value)
        .find(|v| v.register == want_register)
        .unwrap_or_else(|| panic!("no value record for 0x{want_register:04x} in {responses:?}"));
    vreg.decode()
}

#[test]
fn fixture_device_list() {
    let responses = Response::parse_stream(fixture("notify-devices-indef")).unwrap();
    assert_eq!(
        responses,
        vec![Response::DeviceList {
            devices: vec![(0, 0), (1, 0), (3, 1)]
        }]
    );
}

#[test]
fn fixture_control_messages() {
    let ready = ControlMessage::parse(fixture("ctrl-ready-01")).unwrap();
    assert_eq!(ready, ControlMessage::ReadyToReceive { free_chunks: 1 });

    let err = ControlMessage::parse(fixture("ctrl-error-0300")).unwrap();
    assert_eq!(err, ControlMessage::Error { code: 0x0003 });

    let info = ControlInfo::parse(fixture("ctrl-info-initial")).unwrap();
    assert_eq!(info.protocol_byte, 0x00);
    assert_eq!(info.field_u16, 0x0004);
    assert_eq!(info.payload_size_limit, 0x01);
    assert_eq!(info.field_byte, 0xde);
    assert_eq!(info.free_chunks, 0x4a);
    assert_eq!(info.tail, 0x00);
}

#[test]
fn fixture_requests_encode_to_captured_bytes() {
    assert_eq!(
        Request::GetDevices.encode().unwrap(),
        fixture("request-getdevices")
    );
    assert_eq!(
        Request::Subscribe { instance: 3 }.encode().unwrap(),
        fixture("request-subscribe3")
    );
    let registers = vec![
        0xEDBB, 0xEDBD, 0xEDBC, 0xED8D, 0xED8C, 0x0201, 0xEDA8, 0xEDAD, 0xEDAA, 0xED8F, 0xED8E,
    ];
    assert_eq!(
        Request::GetValues {
            instance: 3,
            registers
        }
        .encode()
        .unwrap(),
        fixture("request-getvalues11")
    );
    assert_eq!(
        Request::GetPathList { instance: 3 }.encode().unwrap(),
        fixture("request-pathlist3")
    );
}

#[test]
fn fixture_single_value_records() {
    let responses = Response::parse_stream(fixture("value-solar-voltage")).unwrap();
    assert_eq!(responses, vec![value(0xedbb, vec![0xf3, 0x0a])]);
    // Python: solar voltage 28.03 V, confirmed
    let d = decoded_vreg(&responses, 0xedbb);
    assert_eq!(d.confidence, Confidence::Confirmed);
    assert_eq!(d.value, Some(Scaled::Number(28.03)));

    let responses = Response::parse_stream(fixture("value-battery-voltage")).unwrap();
    assert_eq!(responses, vec![value(0xed8d, vec![0xb8, 0x09])]);
    // Python: battery voltage 24.88 V
    let d = decoded_vreg(&responses, 0xed8d);
    assert_eq!(d.value, Some(Scaled::Number(24.88)));

    let responses = Response::parse_stream(fixture("value-load-voltage")).unwrap();
    assert_eq!(responses, vec![value(0xeda9, vec![0x5b, 0x00])]);
    // Python: load/output voltage-like 9.1 V, candidate
    let d = decoded_vreg(&responses, 0xeda9);
    assert_eq!(d.confidence, Confidence::Candidate);
    assert_eq!(d.value, Some(Scaled::Number(9.1)));

    let responses = Response::parse_stream(fixture("value-negative-current")).unwrap();
    assert_eq!(responses, vec![value(0xed8c, vec![0xc4, 0xff, 0xff, 0xff])]);
    // Python: battery current -0.06 A
    let d = decoded_vreg(&responses, 0xed8c);
    assert_eq!(d.value, Some(Scaled::Number(-0.06)));
}

#[test]
fn fixture_concatenated_value_streams() {
    // Two Value records in one notification (Python: 24.90 V + 28.16 V).
    let responses = Response::parse_stream(fixture("value-concat-two")).unwrap();
    assert_eq!(responses.len(), 2);
    assert_eq!(
        decoded_vreg(&responses, 0xed8d).value,
        Some(Scaled::Number(24.90))
    );
    assert_eq!(
        decoded_vreg(&responses, 0xedbb).value,
        Some(Scaled::Number(28.16))
    );

    // Five Value records in one notification.
    let responses = Response::parse_stream(fixture("value-concat-five")).unwrap();
    assert_eq!(responses.len(), 5);
    assert_eq!(
        decoded_vreg(&responses, 0xed8f).value,
        Some(Scaled::Integer(1))
    );
    assert_eq!(
        decoded_vreg(&responses, 0xed8c).value,
        Some(Scaled::Number(0.06))
    );
    assert_eq!(
        decoded_vreg(&responses, 0xec8a).value,
        Some(Scaled::Integer(1))
    );
    assert_eq!(
        decoded_vreg(&responses, 0xedbc).value,
        Some(Scaled::Integer(1))
    );
    assert_eq!(
        decoded_vreg(&responses, 0xedbb).value,
        Some(Scaled::Number(36.94))
    );
}

#[test]
fn fixture_trend_and_history_blocks() {
    // 0xec20 trend available-vregs block.
    let responses = Response::parse_stream(fixture("value-trend-ec20")).unwrap();
    let d = decoded_vreg(&responses, 0xec20);
    match d.value {
        Some(Scaled::Slots(slots)) => {
            let regs: Vec<Option<u16>> = slots.iter().map(|s| s.register).collect();
            assert_eq!(
                regs,
                vec![Some(0xed8d), Some(0xedec), Some(0xec3e), Some(0xed8c)]
            );
        }
        other => panic!("expected Slots, got {other:?}"),
    }

    // 0x104f / 0x1050 history blocks (34 bytes each).
    let responses = Response::parse_stream(fixture("value-history-104f")).unwrap();
    let d = decoded_vreg(&responses, 0x104f);
    match d.value {
        Some(Scaled::Block { words_le, words_be }) => {
            assert_eq!(words_le.len(), 17);
            assert_eq!(words_le[0], 1);
            assert_eq!(words_le[3], 0xdcf7);
            assert_eq!(words_be[0], 256);
            assert_eq!(words_be[3], 63452);
        }
        other => panic!("expected Block, got {other:?}"),
    }

    let responses = Response::parse_stream(fixture("value-history-1050")).unwrap();
    let d = decoded_vreg(&responses, 0x1050);
    match d.value {
        Some(Scaled::Block { words_le, words_be }) => {
            assert_eq!(words_le.len(), 17);
            assert_eq!(words_le[0], 0x9400);
            assert_eq!(words_be[0], 148);
            assert_eq!(words_le[16], 92);
        }
        other => panic!("expected Block, got {other:?}"),
    }
}

#[test]
fn fixture_generic_and_stat_registers() {
    // 0x0200 → u8 1
    let responses = Response::parse_stream(fixture("value-state-0200")).unwrap();
    let d = decoded_vreg(&responses, 0x0200);
    assert_eq!(d.decoder, "u8");
    assert_eq!(d.value, Some(Scaled::Integer(1)));

    // 0x0202 → raw_s32_le 2
    let responses = Response::parse_stream(fixture("value-device-0202")).unwrap();
    let d = decoded_vreg(&responses, 0x0202);
    assert_eq!(d.decoder, "raw_s32_le");
    assert_eq!(d.value, Some(Scaled::Integer(2)));

    // 0x2001 aa0a → 2730
    let responses = Response::parse_stream(fixture("value-stat-2001")).unwrap();
    let d = decoded_vreg(&responses, 0x2001);
    assert_eq!(d.value, Some(Scaled::Integer(2730)));
}

#[test]
fn fixture_response_codes() {
    let responses = Response::parse_stream(fixture("response-subscribe-ok")).unwrap();
    assert_eq!(
        responses,
        vec![Response::Response {
            instance: 0,
            opcode: 3,
            code: ResponseCode::Ok
        }]
    );

    let responses = Response::parse_stream(fixture("response-subscribe-reject")).unwrap();
    assert_eq!(
        responses,
        vec![Response::Response {
            instance: 9,
            opcode: 3,
            code: ResponseCode::Rejected
        }]
    );

    let responses = Response::parse_stream(fixture("response-pathlist-reject")).unwrap();
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
fn fixture_reassembly_through_the_pipeline() {
    // Feed every captured complete-stream fixture through the reassembler
    // as a LastData notification and require the same parse result.
    for name in [
        "notify-devices-indef",
        "value-solar-voltage",
        "value-concat-two",
        "value-trend-ec20",
        "response-subscribe-reject",
    ] {
        let mut ra = Reassembler::new();
        let payload = ra
            .push_last_data(fixture(name))
            .unwrap()
            .expect("complete payload");
        let via_reassembler = Response::parse_stream(&payload).unwrap();
        let direct = Response::parse_stream(fixture(name)).unwrap();
        assert_eq!(via_reassembler, direct, "fixture {name}");
    }
}

#[test]
fn fixture_cbor_items_match_python_decode() {
    // Spot-check the generic item tree against the Python `decode_stream`
    // outputs documented in the fixture README.
    let items = victron_protocol::cbor::decode_stream(fixture("value-concat-five")).unwrap();
    assert_eq!(items[0], Item::UInt(8));
    assert_eq!(items[1], Item::UInt(3));
    assert_eq!(items[2], Item::UInt(0xed8f));
    assert_eq!(items[3], Item::Bytes(vec![0x01, 0x00]));
    assert_eq!(items[7], Item::Bytes(vec![0x3c, 0, 0, 0]));

    let items = victron_protocol::cbor::decode_stream(fixture("notify-devices-indef")).unwrap();
    assert_eq!(
        items[1],
        Item::Array(vec![
            Item::UInt(0),
            Item::UInt(0),
            Item::UInt(1),
            Item::UInt(0),
            Item::UInt(3),
            Item::UInt(1),
        ])
    );
}
