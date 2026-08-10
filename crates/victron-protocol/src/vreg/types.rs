//! Public VREG types: confidence, invalidity reasons, scaled values, and the
//! raw payload wrapper. Re-exported from the `vreg` module root so the public
//! paths (`vreg::Confidence`, `vreg::Scaled`, ...) are unchanged.

use super::decode_register;

/// Confidence of a decoder: whether the scaling was confirmed against
/// VictronConnect on the owned device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Verified against live VictronConnect display values.
    Confirmed,
    /// Recovered by static/behavioral analysis, not yet cross-checked.
    Candidate,
}

impl Confidence {
    /// Stable string used in diagnostics (`"confirmed"` / `"candidate"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Confidence::Confirmed => "confirmed",
            Confidence::Candidate => "candidate",
        }
    }
}

/// Why a decoded value is absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    /// A known invalid sentinel was present (value stored as `0x....` hex).
    Sentinel(
        /// Hex representation of the sentinel, e.g. `"0xffff"`.
        String,
    ),
    /// The payload was shorter than the decoder needs.
    ShortPayload {
        /// Minimum bytes the decoder requires.
        need: usize,
        /// Bytes actually present.
        have: usize,
    },
    /// The payload length does not match the decoder's fixed requirement
    /// (e.g. the 34-byte `0x104f`/`0x1050` history blocks).
    LengthMismatch {
        /// Required length in bytes.
        expected: usize,
        /// Bytes actually present.
        have: usize,
    },
    /// The payload length violates the decoder's alignment requirement
    /// (e.g. `0xec20` must be a non-empty multiple of 8 bytes).
    NotAligned {
        /// Block size the payload must be a non-empty multiple of.
        multiple: usize,
        /// Bytes actually present.
        have: usize,
    },
}

/// A scaled/decoded value.
#[derive(Debug, Clone, PartialEq)]
pub enum Scaled {
    /// Floating-point scaled value (e.g. volts/amps).
    Number(f64),
    /// Integer value (raw integer, or a rounded scaled value such as the
    /// rounded watts of `0xedbc`/`0x2027`).
    Integer(i64),
    /// Enum state (e.g. charger state `0x0201`).
    State {
        /// Raw state code.
        code: u8,
        /// State name (or `Unknown(n)` fallback).
        name: String,
    },
    /// `0xec20` trend available-registers block: 8-byte slots whose first
    /// u16 is a register id (`0xffff` = empty slot).
    Slots(
        /// The decoded slots, in payload order.
        Vec<Slot>,
    ),
    /// `0x104f`/`0x1050` MPPT history/trend block: raw 34-byte payload with
    /// both endian word views until the field layout is confirmed.
    Block {
        /// Little-endian u16 view of the payload.
        words_le: Vec<u16>,
        /// Big-endian u16 view of the payload.
        words_be: Vec<u16>,
    },
}

/// One 8-byte slot of a `0xec20` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    /// Byte offset of the slot inside the payload.
    pub offset: usize,
    /// Register id in the first two bytes, or `None` for `0xffff` (empty).
    pub register: Option<u16>,
    /// The full 8-byte slot payload.
    pub raw: Vec<u8>,
}

/// Decoded result for one VREG payload.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedVreg {
    /// VREG register id.
    pub register: u16,
    /// Human name, when the register is known.
    pub name: Option<&'static str>,
    /// Unit of the scaled value.
    pub unit: Option<&'static str>,
    /// Decoder description, e.g. `"u16_le/100"`.
    pub decoder: &'static str,
    /// Confidence of this decoder.
    pub confidence: Confidence,
    /// Scaled value, or `None` when invalid/short/unsupported.
    pub value: Option<Scaled>,
    /// Why the value is absent, when it is.
    pub invalid: Option<Invalid>,
}

/// A raw VREG payload with its register id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VregValue {
    /// VREG register id.
    pub register: u16,
    /// Raw little-endian payload bytes (unscaled, as received).
    pub raw: Vec<u8>,
}

/// Charger state codes for register `0x0201` (candidate mapping from the
/// live reader's `STATE_NAMES`).
pub const CHARGER_STATES: [(u8, &str); 10] = [
    (0, "Off"),
    (2, "Fault"),
    (3, "Bulk"),
    (4, "Absorption"),
    (5, "Float"),
    (6, "Storage"),
    (7, "Equalize"),
    (245, "Starting-up"),
    (247, "Auto equalize/Recondition"),
    (252, "External control"),
];

/// Load-output state codes for register `0xeda8` (candidate).
pub const LOAD_STATES: [(u8, &str); 2] = [(0, "Off"), (1, "On")];

impl VregValue {
    /// Create a raw VREG value.
    pub fn new(register: u16, raw: impl Into<Vec<u8>>) -> Self {
        VregValue {
            register,
            raw: raw.into(),
        }
    }

    /// Decode this payload (see [`decode_register`]).
    pub fn decode(&self) -> DecodedVreg {
        decode_register(self.register, &self.raw)
    }
}

/// Look up a charger state name (`0x0201`), falling back to `Unknown(n)`.
pub fn charger_state_name(code: u8) -> String {
    CHARGER_STATES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, n)| (*n).to_string())
        .unwrap_or_else(|| format!("Unknown({code})"))
}

/// Look up a load-output state name (`0xeda8`), falling back to `Unknown(n)`.
pub fn load_state_name(code: u8) -> String {
    LOAD_STATES
        .iter()
        .find(|(c, _)| *c == code)
        .map(|(_, n)| (*n).to_string())
        .unwrap_or_else(|| format!("Unknown({code})"))
}
