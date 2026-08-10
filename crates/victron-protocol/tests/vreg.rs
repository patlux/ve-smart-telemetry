//! Integration tests for the `vreg` module.
//!
//! Moved out of the module to keep library files under the 500-line guidance.

use victron_protocol::vreg::{
    charger_state_name, load_state_name, Confidence, Invalid, Scaled, VregValue,
};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn num(register: u16, raw: &str) -> f64 {
    match VregValue::new(register, hex(raw)).decode().value {
        Some(Scaled::Number(v)) => v,
        other => panic!("expected Number, got {other:?}"),
    }
}

fn integer(register: u16, raw: &str) -> i64 {
    match VregValue::new(register, hex(raw)).decode().value {
        Some(Scaled::Integer(v)) => v,
        other => panic!("expected Integer, got {other:?}"),
    }
}

fn state(register: u16, raw: &str) -> (u8, String) {
    match VregValue::new(register, hex(raw)).decode().value {
        Some(Scaled::State { code, name }) => (code, name),
        other => panic!("expected State, got {other:?}"),
    }
}

#[test]
fn solar_voltage_confirmed_scaling_from_capture() {
    // 0xedbb f30a → 0x0af3 = 2803 → 28.03 V
    let d = VregValue::new(0xEDBB, hex("f30a")).decode();
    assert_eq!(d.confidence, Confidence::Confirmed);
    assert_eq!(d.name, Some("Solar voltage"));
    assert_eq!(d.value, Some(Scaled::Number(28.03)));
    assert_eq!(d.invalid, None);

    // 3f00 → 0.63 V (captured in history-fallback)
    assert_eq!(num(0xEDBB, "3f00"), 0.63);
    // 000b → 28.16 V (captured)
    assert_eq!(num(0xEDBB, "000b"), 28.16);
    // 6e0e → 36.94 V (captured)
    assert_eq!(num(0xEDBB, "6e0e"), 36.94);
}

#[test]
fn battery_voltage_scaling_from_capture() {
    // b809 → 0x09b8 = 2488 → 24.88 V
    assert_eq!(num(0xED8D, "b809"), 24.88);
    // ba09 → 24.90 V
    assert_eq!(num(0xED8D, "ba09"), 24.90);
}

#[test]
fn solar_power_rounded_from_capture() {
    // 64000000 → 100 → round(100/100) = 1 W
    assert_eq!(integer(0xEDBC, "64000000"), 1);
    // 00000000 → 0
    assert_eq!(integer(0xEDBC, "00000000"), 0);
}

#[test]
fn battery_current_scale_and_sign() {
    // 00000000 → 0.0
    assert_eq!(num(0xED8C, "00000000"), 0.0);
    // c4ffffff → -60/1000 = -0.06 A (captured)
    assert_eq!(num(0xED8C, "c4ffffff"), -0.06);
    // 3c000000 → 0.06 A (captured)
    assert_eq!(num(0xED8C, "3c000000"), 0.06);
}

#[test]
fn load_voltage_scaling_from_capture() {
    // 5b00 → 0x005b = 91 → 9.1 V
    assert_eq!(num(0xEDA9, "5b00"), 9.1);
    assert_eq!(
        VregValue::new(0xEDA9, hex("5b00")).decode().confidence,
        Confidence::Candidate
    );
}

#[test]
fn sentinel_rejection_u16() {
    // 0x2001 with ffff → invalid, value None (captured in history-fixed-final)
    let d = VregValue::new(0x2001, hex("ffff")).decode();
    assert_eq!(d.value, None);
    assert_eq!(d.invalid, Some(Invalid::Sentinel("0xffff".into())));
    // normal u16 still decodes
    assert_eq!(integer(0x2001, "aa0a"), 2730);
}

#[test]
fn sentinel_rejection_s32_max() {
    // 0x2013 with ffffff7f → 0x7fffffff sentinel (captured)
    let d = VregValue::new(0x2013, hex("ffffff7f")).decode();
    assert_eq!(d.value, None);
    assert_eq!(d.invalid, Some(Invalid::Sentinel("0x7fffffff".into())));
}

#[test]
fn sentinel_rejection_u32_all_ones() {
    let d = VregValue::new(0x0BAD, hex("ffffffff")).decode();
    assert_eq!(d.value, None);
    assert_eq!(d.invalid, Some(Invalid::Sentinel("0xffffffff".into())));
}

#[test]
fn sentinel_rejection_s16() {
    for (raw, sentinel) in [("ff7f", "0x7fff"), ("0080", "0x8000")] {
        let d = VregValue::new(0xED8F, hex(raw)).decode();
        assert_eq!(d.value, None, "raw {raw}");
        assert_eq!(
            d.invalid,
            Some(Invalid::Sentinel(sentinel.into())),
            "raw {raw}"
        );
    }
}

#[test]
fn trend_available_vregs_block_from_capture() {
    // 0xec20 captured block: slots 0xed8d, 0xedec, 0xec3e, 0xed8c
    let raw = "8dedffffffffffffecedffffffffffff3eecffffffffffff8cedffffffffffff";
    let d = VregValue::new(0xEC20, hex(raw)).decode();
    match d.value {
        Some(Scaled::Slots(slots)) => {
            assert_eq!(slots.len(), 4);
            let regs: Vec<Option<u16>> = slots.iter().map(|s| s.register).collect();
            assert_eq!(
                regs,
                vec![Some(0xed8d), Some(0xedec), Some(0xec3e), Some(0xed8c)]
            );
            assert_eq!(slots[0].offset, 0);
            assert_eq!(slots[3].offset, 24);
        }
        other => panic!("expected Slots, got {other:?}"),
    }
}

#[test]
fn history_block_words_from_capture() {
    // 0x104f 34-byte block; words match the Python decoder output.
    let raw = "010012000000f7dc0100f7dc01006b14e80a1e0100ffffffffffffffffffffffffff";
    let d = VregValue::new(0x104F, hex(raw)).decode();
    match d.value {
        Some(Scaled::Block { words_le, words_be }) => {
            assert_eq!(words_le.len(), 17);
            assert_eq!(words_le[0], 1);
            assert_eq!(words_le[1], 18);
            assert_eq!(words_le[3], 0xdcf7);
            assert_eq!(words_le[7], 0x146b);
            assert_eq!(words_le[10], 0xff00);
            assert_eq!(words_be[0], 256);
            assert_eq!(words_be[1], 4608);
        }
        other => panic!("expected Block, got {other:?}"),
    }
}

#[test]
fn generic_length_fallbacks() {
    // u8
    let d = VregValue::new(0x0200, hex("01")).decode();
    assert_eq!(d.decoder, "u8");
    assert_eq!(d.value, Some(Scaled::Integer(1)));
    // u16
    let d = VregValue::new(0x0BAD, hex("aa0a")).decode();
    assert_eq!(d.decoder, "raw_u16_le");
    assert_eq!(d.value, Some(Scaled::Integer(2730)));
    // s32
    let d = VregValue::new(0x0BAD, hex("02000000")).decode();
    assert_eq!(d.decoder, "raw_s32_le");
    assert_eq!(d.value, Some(Scaled::Integer(2)));
    // s32 negative
    let d = VregValue::new(0x0BAD, hex("e2ffffff")).decode();
    assert_eq!(d.value, Some(Scaled::Integer(-30)));
    // odd length → raw
    let d = VregValue::new(0x0BAD, hex("010203")).decode();
    assert_eq!(d.decoder, "raw");
    assert_eq!(d.value, None);
}

#[test]
fn short_payload_flagged() {
    let d = VregValue::new(0xEDBB, hex("f3")).decode();
    assert_eq!(d.value, None);
    assert_eq!(d.invalid, Some(Invalid::ShortPayload { need: 2, have: 1 }));
}

#[test]
fn state_names() {
    assert_eq!(charger_state_name(3), "Bulk");
    assert_eq!(charger_state_name(0), "Off");
    assert_eq!(charger_state_name(252), "External control");
    assert_eq!(charger_state_name(99), "Unknown(99)");
    assert_eq!(load_state_name(1), "On");
    assert_eq!(load_state_name(7), "Unknown(7)");
}

// Golden tests for the explicitly candidate decoders added from the live
// reader's FIELDS table (scripts/read-victron-live-values.py). Expected
// values are the Python `decode_raw` outputs for the same bytes; none of
// these decoders is Confirmed.

#[test]
fn solar_current_candidate_from_live_reader() {
    // 0xedbd `u16_10` A: 0x012c = 300 → 30.0 A
    let d = VregValue::new(0xEDBD, hex("2c01")).decode();
    assert_eq!(d.confidence, Confidence::Candidate);
    assert_eq!(d.name, Some("Solar current"));
    assert_eq!(d.unit, Some("A"));
    assert_eq!(d.decoder, "u16_le/10");
    assert_eq!(d.value, Some(Scaled::Number(30.0)));
    // 0x0000 → 0.0 A
    assert_eq!(num(0xEDBD, "0000"), 0.0);
    // sentinel 0xffff → invalid, no value
    let d = VregValue::new(0xEDBD, hex("ffff")).decode();
    assert_eq!(d.value, None);
    assert_eq!(d.invalid, Some(Invalid::Sentinel("0xffff".into())));
    // short payload
    let d = VregValue::new(0xEDBD, hex("2c")).decode();
    assert_eq!(d.value, None);
    assert_eq!(d.invalid, Some(Invalid::ShortPayload { need: 2, have: 1 }));
}

#[test]
fn charger_state_candidate_from_live_reader() {
    // 0x0201 `state_enum`: bounded known/unknown code.
    let d = VregValue::new(0x0201, hex("03")).decode();
    assert_eq!(d.confidence, Confidence::Candidate);
    assert_eq!(d.name, Some("Battery state"));
    assert_eq!(d.decoder, "u8 state enum");
    assert_eq!(
        d.value,
        Some(Scaled::State {
            code: 3,
            name: "Bulk".into()
        })
    );
    assert_eq!(state(0x0201, "f5"), (245, "Starting-up".into()));
    assert_eq!(state(0x0201, "63"), (99, "Unknown(99)".into()));
    // empty payload → short, no value
    let d = VregValue::new(0x0201, Vec::<u8>::new()).decode();
    assert_eq!(d.value, None);
    assert_eq!(d.invalid, Some(Invalid::ShortPayload { need: 1, have: 0 }));
}

#[test]
fn load_state_candidate_from_live_reader() {
    // 0xeda8 `load_state_enum`.
    let d = VregValue::new(0xEDA8, hex("01")).decode();
    assert_eq!(d.confidence, Confidence::Candidate);
    assert_eq!(d.name, Some("Load output state"));
    assert_eq!(d.decoder, "u8 load state enum");
    assert_eq!(
        d.value,
        Some(Scaled::State {
            code: 1,
            name: "On".into()
        })
    );
    assert_eq!(state(0xEDA8, "00"), (0, "Off".into()));
    assert_eq!(state(0xEDA8, "07"), (7, "Unknown(7)".into()));
    let d = VregValue::new(0xEDA8, Vec::<u8>::new()).decode();
    assert_eq!(d.value, None);
    assert_eq!(d.invalid, Some(Invalid::ShortPayload { need: 1, have: 0 }));
}

#[test]
fn load_current_candidate_from_live_reader() {
    // 0xedad `u16_10` A: 0x012c = 300 → 30.0 A
    let d = VregValue::new(0xEDAD, hex("2c01")).decode();
    assert_eq!(d.confidence, Confidence::Candidate);
    assert_eq!(d.name, Some("Load output current"));
    assert_eq!(d.unit, Some("A"));
    assert_eq!(d.decoder, "u16_le/10");
    assert_eq!(d.value, Some(Scaled::Number(30.0)));
    // sentinel
    let d = VregValue::new(0xEDAD, hex("ffff")).decode();
    assert_eq!(d.value, None);
    assert_eq!(d.invalid, Some(Invalid::Sentinel("0xffff".into())));
}

#[test]
fn load_power_candidate_from_live_reader() {
    // 0xedaa `u16` W: 0x0064 = 100 → 100 W (integer, no scaling)
    let d = VregValue::new(0xEDAA, hex("6400")).decode();
    assert_eq!(d.confidence, Confidence::Candidate);
    assert_eq!(d.name, Some("Load output power"));
    assert_eq!(d.unit, Some("W"));
    assert_eq!(d.decoder, "u16_le");
    assert_eq!(d.value, Some(Scaled::Integer(100)));
    // sentinel
    let d = VregValue::new(0xEDAA, hex("ffff")).decode();
    assert_eq!(d.value, None);
    assert_eq!(d.invalid, Some(Invalid::Sentinel("0xffff".into())));
}

// Golden rounding regressions: the proven Python decoder uses
// `round(value / 100)` (ties-to-even) for 0xedbc and 0x2027. Expected
// values below are `python3 -c 'print(round(v/100))'` outputs.

#[test]
fn solar_power_rounding_matches_python_ties_to_even() {
    // 0xedbc is u32_le; positive .5 boundaries and ordinary non-ties.
    for (raw, want) in [
        ("96000000", 2), // 150 → 1.5 → 2 (tie to even)
        ("fa000000", 2), // 250 → 2.5 → 2 (tie to even)
        ("5e010000", 4), // 350 → 3.5 → 4 (tie to even)
        ("95000000", 1), // 149 → 1.49 → 1
        ("97000000", 2), // 151 → 1.51 → 2
        ("f9000000", 2), // 249 → 2.49 → 2
        ("fb000000", 3), // 251 → 2.51 → 3
        ("63000000", 1), // 99 → 0.99 → 1
        ("65000000", 1), // 101 → 1.01 → 1
        ("00000000", 0), // 0 → 0
        ("01000000", 0), // 1 → 0.01 → 0
        ("05000000", 0), // 5 → 0.05 → 0
        ("0f000000", 0), // 15 → 0.15 → 0
    ] {
        let d = VregValue::new(0xEDBC, hex(raw)).decode();
        assert_eq!(
            d.value,
            Some(Scaled::Integer(want)),
            "0xedbc raw {raw} (Python round parity)"
        );
        assert_eq!(d.unit, Some("W"));
        assert_eq!(d.name, Some("Solar power"));
        assert_eq!(d.confidence, Confidence::Candidate);
    }
}

#[test]
fn trend_power_rounding_matches_python_ties_to_even() {
    // 0x2027 is s32_le; signed .5 boundaries and ordinary non-ties.
    for (raw, want) in [
        ("96000000", 2),  // 150 → 1.5 → 2
        ("fa000000", 2),  // 250 → 2.5 → 2
        ("5e010000", 4),  // 350 → 3.5 → 4
        ("6affffff", -2), // -150 → -1.5 → -2
        ("06ffffff", -2), // -250 → -2.5 → -2
        ("a2feffff", -4), // -350 → -3.5 → -4
        ("95000000", 1),  // 149 → 1
        ("97000000", 2),  // 151 → 2
        ("6bffffff", -1), // -149 → -1
        ("69ffffff", -2), // -151 → -2
        ("07ffffff", -2), // -249 → -2
        ("05ffffff", -3), // -251 → -3
        ("9dffffff", -1), // -99 → -1
        ("9bffffff", -1), // -101 → -1
        ("01000000", 0),  // 1 → 0
        ("ffffffff", 0),  // -1 → 0 (not an s32 sentinel)
        ("05000000", 0),  // 5 → 0
        ("fbffffff", 0),  // -5 → 0
    ] {
        let d = VregValue::new(0x2027, hex(raw)).decode();
        assert_eq!(
            d.value,
            Some(Scaled::Integer(want)),
            "0x2027 raw {raw} (Python round parity)"
        );
        assert_eq!(d.unit, Some("W"));
        assert_eq!(d.name, Some("Power-like trend value"));
        assert_eq!(d.confidence, Confidence::Candidate);
    }
}

#[test]
fn history_block_requires_exactly_34_bytes() {
    // 33 bytes (captured 34-byte payload minus one byte) → typed mismatch.
    let d = VregValue::new(
        0x104F,
        hex("010012000000f7dc0100f7dc01006b14e80a1e0100ffffffffffffffffffffffff"),
    )
    .decode();
    assert_eq!(d.value, None);
    assert_eq!(
        d.invalid,
        Some(Invalid::LengthMismatch {
            expected: 34,
            have: 33
        })
    );
    assert_eq!(d.confidence, Confidence::Candidate);

    // 35 bytes → typed mismatch, no Block.
    let d = VregValue::new(
        0x1050,
        hex("010012000000f7dc0100f7dc01006b14e80a1e0100ffffffffffffffffffffffffff00"),
    )
    .decode();
    assert_eq!(d.value, None);
    assert_eq!(
        d.invalid,
        Some(Invalid::LengthMismatch {
            expected: 34,
            have: 35
        })
    );
}

#[test]
fn trend_block_reports_alignment_not_remainder() {
    // 12 bytes is not a multiple of 8: report the actual length and the
    // multiple-of-8 requirement (not a misleading ShortPayload remainder).
    let d = VregValue::new(0xEC20, hex("8dedffffffffffffecedffff")).decode();
    assert_eq!(d.value, None);
    assert_eq!(
        d.invalid,
        Some(Invalid::NotAligned {
            multiple: 8,
            have: 12
        })
    );
    assert_eq!(d.confidence, Confidence::Candidate);

    // Empty payload: also NotAligned (must be a non-empty multiple of 8).
    let d = VregValue::new(0xEC20, Vec::<u8>::new()).decode();
    assert_eq!(d.value, None);
    assert_eq!(
        d.invalid,
        Some(Invalid::NotAligned {
            multiple: 8,
            have: 0
        })
    );
}

#[test]
fn lifetime_yield_register_has_no_invented_mapping() {
    // 0xed8e is the lifetime-yield register; the live reader only lists it
    // as an opaque generic-power fallback. The crate must NOT invent a
    // scaling — it falls through to the generic length-based decoder.
    let d = VregValue::new(0xED8E, hex("6400")).decode();
    assert_eq!(d.name, None);
    assert_eq!(d.decoder, "raw_u16_le");
    assert_eq!(d.value, Some(Scaled::Integer(100)));
}
