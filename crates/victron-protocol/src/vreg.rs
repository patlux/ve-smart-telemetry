//! VREG payload decoding: raw little-endian VREG values → scaled values,
//! with sentinel rejection and an explicit confidence marker.
//!
//! Decoders are ported from `decode_vreg_payload()` in
//! `scripts/read-victron-history.py` and the `FIELDS`/`decode_raw()` tables
//! in `scripts/read-victron-live-values.py` (the union of both readers).
//! Solar voltage (`0xedbb`, u16 LE / 100) and panel power (`0xedbc`, u32
//! LE / 100 W) are marked [`Confidence::Confirmed`]. Both match live target
//! captures; the panel-power identity, type, unit, and scale are additionally
//! specified by Victron's BlueSolar HEX protocol. Everything else remains
//! [`Confidence::Candidate`] until independently confirmed.
//!
//! Explicitly candidate decoders added from the live reader's `FIELDS`
//! table (all `candidate` there too): `0xedbd` solar current `u16_le/10` A,
//! `0x0201` battery/charger state `u8` state enum, `0xeda8` load-output
//! state `u8` load-state enum, `0xedad` load-output current `u16_le/10` A,
//! `0xedaa` load-output power `u16_le` W.
//!
//! **Rounding parity:** `0xedbc` and `0x2027` use the proven Python
//! decoder's `round(value / 100)`, which rounds half to even. The crate
//! implements that with an exact integer helper (`round_hundredths_ties_even`
//! in the private `round` submodule) instead of `f64::round` (half away from
//! zero), so the `.5` boundaries match Python bit for bit.
//!
//! **Remaining blocker:** the lifetime-yield register (`0xed8e`, listed as
//! a generic-power fallback in the live reader) has **no** decoder mapping
//! yet — the crate deliberately does not invent one. Confirming `0xed8e`
//! (and the `0xed8f` legacy-current flag) against VictronConnect is the
//! open item for full dashboard coverage.
//!
//! Sentinels (rejected, value becomes `None` + `invalid`):
//!
//! * `u16`: `0xffff`
//! * `u32`: `0xffffffff`
//! * `s16`: `0x7fff`, `-0x8000`
//! * `s32`: `0x7fffffff`, `-0x80000000`

mod round;
mod types;

pub use types::{
    charger_state_name, load_state_name, Confidence, DecodedVreg, Invalid, Scaled, Slot, VregValue,
    CHARGER_STATES, LOAD_STATES,
};

use round::round_hundredths_ties_even;

/// Decode a raw VREG payload for a register id.
pub fn decode_register(register: u16, raw: &[u8]) -> DecodedVreg {
    let u16le = |off: usize| {
        raw.get(off..off + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let s16le = |off: usize| {
        raw.get(off..off + 2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
    };
    let u32le = |off: usize| {
        raw.get(off..off + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let s32le = |off: usize| {
        raw.get(off..off + 4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };

    let invalid_u16 = |v: u16| {
        if v == 0xffff {
            Some(Invalid::Sentinel("0xffff".into()))
        } else {
            None
        }
    };
    let invalid_u32 = |v: u32| {
        if v == 0xffff_ffff {
            Some(Invalid::Sentinel("0xffffffff".into()))
        } else {
            None
        }
    };
    let invalid_s16 = |v: i16| {
        if v == 0x7fff {
            Some(Invalid::Sentinel("0x7fff".into()))
        } else if v == -0x8000 {
            Some(Invalid::Sentinel("0x8000".into()))
        } else {
            None
        }
    };
    let invalid_s32 = |v: i32| {
        if v == 0x7fff_ffff {
            Some(Invalid::Sentinel("0x7fffffff".into()))
        } else if v == -0x8000_0000 {
            Some(Invalid::Sentinel("0x80000000".into()))
        } else {
            None
        }
    };

    /// Decode with a fixed raw-length requirement, mapping a too-short
    /// payload to `ShortPayload`.
    fn short(need: usize, have: usize) -> Option<Invalid> {
        if have < need {
            Some(Invalid::ShortPayload { need, have })
        } else {
            None
        }
    }

    let named = |name: &'static str,
                 unit: Option<&'static str>,
                 decoder: &'static str,
                 confidence: Confidence,
                 value: Option<Scaled>,
                 invalid: Option<Invalid>| {
        DecodedVreg {
            register,
            name: Some(name),
            unit,
            decoder,
            confidence,
            value,
            invalid,
        }
    };

    // Shared helpers for the candidate decoders added from the live reader's
    // FIELDS table (all `candidate`; sentinel + short-payload preserved).
    let u16_candidate = |name: &'static str,
                         unit: Option<&'static str>,
                         decoder: &'static str,
                         div: f64,
                         v: Option<u16>,
                         inv: Option<Invalid>| {
        let value =
            v.and_then(|v| (invalid_u16(v).is_none()).then_some(Scaled::Number(v as f64 / div)));
        let sentinel = v.and_then(invalid_u16);
        named(
            name,
            unit,
            decoder,
            Confidence::Candidate,
            value,
            sentinel.or(inv),
        )
    };
    let u16_candidate_int = |name: &'static str,
                             unit: Option<&'static str>,
                             decoder: &'static str,
                             v: Option<u16>,
                             inv: Option<Invalid>| {
        let value = v.and_then(|v| (invalid_u16(v).is_none()).then_some(Scaled::Integer(v as i64)));
        let sentinel = v.and_then(invalid_u16);
        named(
            name,
            unit,
            decoder,
            Confidence::Candidate,
            value,
            sentinel.or(inv),
        )
    };
    let state_candidate = |name: &'static str,
                           decoder: &'static str,
                           lookup: fn(u8) -> String,
                           inv: Option<Invalid>| {
        let value = raw.first().copied().map(|c| Scaled::State {
            code: c,
            name: lookup(c),
        });
        named(name, None, decoder, Confidence::Candidate, value, inv)
    };

    match register {
        0xEDBB => {
            let inv = short(2, raw.len());
            let v = u16le(0);
            let value = v.and_then(|v| {
                (invalid_u16(v).is_none()).then_some(Scaled::Number(v as f64 / 100.0))
            });
            let sentinel = v.and_then(invalid_u16);
            named(
                "Solar voltage",
                Some("V"),
                "u16_le/100",
                Confidence::Confirmed,
                value,
                sentinel.or(inv),
            )
        }
        0xEDBC => {
            let inv = short(4, raw.len());
            let v = u32le(0);
            let value = v.and_then(|v| {
                (invalid_u32(v).is_none())
                    .then_some(Scaled::Integer(round_hundredths_ties_even(v as i64)))
            });
            let sentinel = v.and_then(invalid_u32);
            named(
                "Solar power",
                Some("W"),
                "u32_le/100 rounded",
                Confidence::Confirmed,
                value,
                sentinel.or(inv),
            )
        }
        0xED8D => {
            let inv = short(2, raw.len());
            let v = u16le(0);
            let value = v.and_then(|v| {
                (invalid_u16(v).is_none()).then_some(Scaled::Number(v as f64 / 100.0))
            });
            let sentinel = v.and_then(invalid_u16);
            named(
                "Battery voltage",
                Some("V"),
                "u16_le/100",
                Confidence::Candidate,
                value,
                sentinel.or(inv),
            )
        }
        0xED8C | 0x2013 => {
            let inv = short(4, raw.len());
            let v = s32le(0);
            let value = v.and_then(|v| {
                (invalid_s32(v).is_none()).then_some(Scaled::Number(v as f64 / 1000.0))
            });
            let sentinel = v.and_then(invalid_s32);
            let name = if register == 0xED8C {
                "Battery current"
            } else {
                "Trend/current-like value"
            };
            named(
                name,
                Some("A"),
                "s32_le/1000",
                Confidence::Candidate,
                value,
                sentinel.or(inv),
            )
        }
        0xED8F => {
            let inv = short(2, raw.len());
            let v = s16le(0);
            let value =
                v.and_then(|v| (invalid_s16(v).is_none()).then_some(Scaled::Integer(v as i64)));
            let sentinel = v.and_then(invalid_s16);
            named(
                "Current flag/legacy current",
                None,
                "s16_le",
                Confidence::Candidate,
                value,
                sentinel.or(inv),
            )
        }
        0xEDA9 => {
            let inv = short(2, raw.len());
            let v = u16le(0);
            let value = v.and_then(|v| {
                (invalid_u16(v).is_none()).then_some(Scaled::Number(v as f64 / 10.0))
            });
            let sentinel = v.and_then(invalid_u16);
            named(
                "Load/output voltage-like value",
                Some("V"),
                "u16_le/10",
                Confidence::Candidate,
                value,
                sentinel.or(inv),
            )
        }
        0xEDBD => {
            // Solar current (live reader FIELDS: `u16_10` A, candidate).
            u16_candidate(
                "Solar current",
                Some("A"),
                "u16_le/10",
                10.0,
                u16le(0),
                short(2, raw.len()),
            )
        }
        0x0201 => {
            // Battery/charger state (`state_enum`); bounded known/unknown code.
            state_candidate(
                "Battery state",
                "u8 state enum",
                charger_state_name,
                short(1, raw.len()),
            )
        }
        0xEDA8 => {
            // Load-output state (`load_state_enum`); bounded known/unknown code.
            state_candidate(
                "Load output state",
                "u8 load state enum",
                load_state_name,
                short(1, raw.len()),
            )
        }
        0xEDAD => {
            // Load-output current (live reader FIELDS: `u16_10` A, candidate).
            u16_candidate(
                "Load output current",
                Some("A"),
                "u16_le/10",
                10.0,
                u16le(0),
                short(2, raw.len()),
            )
        }
        0xEDAA => {
            // Load-output power (live reader FIELDS: `u16` W, candidate).
            u16_candidate_int(
                "Load output power",
                Some("W"),
                "u16_le",
                u16le(0),
                short(2, raw.len()),
            )
        }
        0x2027 => {
            let inv = short(4, raw.len());
            let v = s32le(0);
            let value = v.and_then(|v| {
                (invalid_s32(v).is_none())
                    .then_some(Scaled::Integer(round_hundredths_ties_even(v as i64)))
            });
            let sentinel = v.and_then(invalid_s32);
            named(
                "Power-like trend value",
                Some("W"),
                "s32_le/100",
                Confidence::Candidate,
                value,
                sentinel.or(inv),
            )
        }
        0xEC20 => {
            if raw.len() % 8 == 0 && !raw.is_empty() {
                let mut slots = Vec::with_capacity(raw.len() / 8);
                for (offset, chunk) in raw.chunks_exact(8).enumerate() {
                    let reg = u16::from_le_bytes([chunk[0], chunk[1]]);
                    slots.push(Slot {
                        offset: offset * 8,
                        register: (reg != 0xffff).then_some(reg),
                        raw: chunk.to_vec(),
                    });
                }
                named(
                    "Trend available-vregs block",
                    None,
                    "8-byte slots, first u16 register",
                    Confidence::Candidate,
                    Some(Scaled::Slots(slots)),
                    None,
                )
            } else {
                DecodedVreg {
                    register,
                    name: Some("Trend available-vregs block"),
                    unit: None,
                    decoder: "8-byte slots, first u16 register",
                    confidence: Confidence::Candidate,
                    value: None,
                    invalid: Some(Invalid::NotAligned {
                        multiple: 8,
                        have: raw.len(),
                    }),
                }
            }
        }
        0x104F | 0x1050 => {
            if raw.len() == 34 {
                let words_le: Vec<u16> = (0..raw.len().saturating_sub(1))
                    .step_by(2)
                    .filter_map(|i| raw.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]])))
                    .collect();
                let words_be: Vec<u16> = (0..raw.len().saturating_sub(1))
                    .step_by(2)
                    .filter_map(|i| raw.get(i..i + 2).map(|b| u16::from_be_bytes([b[0], b[1]])))
                    .collect();
                named(
                    "MPPT history/trend block",
                    None,
                    "raw 34-byte block; field layout pending",
                    Confidence::Candidate,
                    Some(Scaled::Block { words_le, words_be }),
                    None,
                )
            } else {
                DecodedVreg {
                    register,
                    name: Some("MPPT history/trend block"),
                    unit: None,
                    decoder: "raw 34-byte block; field layout pending",
                    confidence: Confidence::Candidate,
                    value: None,
                    invalid: Some(Invalid::LengthMismatch {
                        expected: 34,
                        have: raw.len(),
                    }),
                }
            }
        }
        _ => {
            // Generic length-based fallbacks (matches the Python decoder).
            match raw.len() {
                1 => DecodedVreg {
                    register,
                    name: None,
                    unit: None,
                    decoder: "u8",
                    confidence: Confidence::Candidate,
                    value: Some(Scaled::Integer(raw[0] as i64)),
                    invalid: None,
                },
                2 => {
                    let v = u16::from_le_bytes([raw[0], raw[1]]);
                    let sentinel = invalid_u16(v);
                    DecodedVreg {
                        register,
                        name: None,
                        unit: None,
                        decoder: "raw_u16_le",
                        confidence: Confidence::Candidate,
                        value: sentinel.is_none().then_some(Scaled::Integer(v as i64)),
                        invalid: sentinel,
                    }
                }
                4 => {
                    let v = i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
                    let sentinel = if raw == [0xff; 4] {
                        Some(Invalid::Sentinel("0xffffffff".into()))
                    } else {
                        invalid_s32(v)
                    };
                    DecodedVreg {
                        register,
                        name: None,
                        unit: None,
                        decoder: "raw_s32_le",
                        confidence: Confidence::Candidate,
                        value: sentinel.is_none().then_some(Scaled::Integer(v as i64)),
                        invalid: sentinel,
                    }
                }
                _ => DecodedVreg {
                    register,
                    name: None,
                    unit: None,
                    decoder: "raw",
                    confidence: Confidence::Candidate,
                    value: None,
                    invalid: None,
                },
            }
        }
    }
}
