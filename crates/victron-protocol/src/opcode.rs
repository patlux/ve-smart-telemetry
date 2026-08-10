//! VE.Smart CBOR opcodes (app ⇄ device), outbound and inbound.
//!
//! Opcode numbers follow `analysis/victronconnect-protocol-reference.md`
//! §6. Outbound requests use one CBOR unsigned int for the opcode followed by
//! opcode-specific parameters. Inbound notifications use the same scheme on
//! the Data/LastData characteristics.
//!
//! The settings-write opcodes `0x06` (`setValues`) and `0x0c`
//! (`setPathValues`) are deliberately **not** represented here: this crate
//! is read-only and no public type exposes a write operation, so
//! [`OutOpcode::from_u8`] returns `None` for those wire values.

/// Outbound (app → device) CBOR opcodes observed in the reference.
///
/// Every variant is read-only. The settings-write opcodes (`0x06`
/// `setValues`, `0x0c` `setPathValues`) are deliberately absent from the
/// public API and are not constructible via [`OutOpcode::from_u8`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum OutOpcode {
    /// `0x01` getDevices — request the device list.
    GetDevices = 0x01,
    /// `0x03` subscribe(instance) — start value pushes for an instance.
    Subscribe = 0x03,
    /// `0x04` unsubscribe(instance).
    Unsubscribe = 0x04,
    /// `0x05` getValues(instance, `[vregs]`) — read VREG values.
    GetValues = 0x05,
    /// `0x0a` getPathList(instance).
    GetPathList = 0x0a,
    /// `0x0b` getPathValues(instance, `[indexes]`).
    GetPathValues = 0x0b,
}

impl OutOpcode {
    /// Map a wire byte to an opcode.
    ///
    /// Returns `None` for the settings-write opcodes `0x06`/`0x0c` and any
    /// other unobserved byte: no write opcode is constructible.
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0x01 => OutOpcode::GetDevices,
            0x03 => OutOpcode::Subscribe,
            0x04 => OutOpcode::Unsubscribe,
            0x05 => OutOpcode::GetValues,
            0x0a => OutOpcode::GetPathList,
            0x0b => OutOpcode::GetPathValues,
            _ => return None,
        })
    }

    /// Wire byte value.
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// Whether this opcode is a pure read.
    ///
    /// Every variant this crate exposes is read-only, so this is always
    /// `true`; it exists so callers can assert the read-only contract
    /// without matching on variants.
    pub fn is_read_only(self) -> bool {
        true
    }
}

/// Inbound (device → app) CBOR opcodes observed in the reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum InOpcode {
    /// `0x02` DeviceList — array of unsigned pairs.
    DeviceList = 0x02,
    /// `0x07` Response — (instance, request opcode, response code).
    Response = 0x07,
    /// `0x08` Value — (instance, vreg, raw byte string).
    Value = 0x08,
    /// `0x09` ValueResponse — (instance, vreg, response code).
    ValueResponse = 0x09,
    /// `0x0d` PathList — (instance, qCompress(zlib) blob).
    PathList = 0x0d,
    /// `0x0e` NewPath — (instance, path index, path text).
    NewPath = 0x0e,
    /// `0x0f` PathValue — (instance, path index, QVariant value).
    PathValue = 0x0f,
    /// `0x10` PathResponse — (instance, path index, response code).
    PathResponse = 0x10,
}

impl InOpcode {
    /// Map a wire byte to an inbound opcode.
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0x02 => InOpcode::DeviceList,
            0x07 => InOpcode::Response,
            0x08 => InOpcode::Value,
            0x09 => InOpcode::ValueResponse,
            0x0d => InOpcode::PathList,
            0x0e => InOpcode::NewPath,
            0x0f => InOpcode::PathValue,
            0x10 => InOpcode::PathResponse,
            _ => return None,
        })
    }

    /// Wire byte value.
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// Short human description used in diagnostics.
    pub fn description(self) -> &'static str {
        match self {
            InOpcode::DeviceList => "DeviceList",
            InOpcode::Response => "Response",
            InOpcode::Value => "Value",
            InOpcode::ValueResponse => "ValueResponse",
            InOpcode::PathList => "PathList",
            InOpcode::NewPath => "NewPath",
            InOpcode::PathValue => "PathValue",
            InOpcode::PathResponse => "PathResponse",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::OutOpcode;

    #[test]
    fn write_opcodes_are_not_constructible() {
        // The settings-write opcodes 0x06/0x0c must not be constructible.
        assert_eq!(OutOpcode::from_u8(0x06), None);
        assert_eq!(OutOpcode::from_u8(0x0c), None);
    }

    #[test]
    fn every_constructible_opcode_is_read_only() {
        for v in 0..=0xffu8 {
            if let Some(op) = OutOpcode::from_u8(v) {
                assert!(op.is_read_only(), "opcode 0x{v:02x} must be read-only");
            }
        }
    }

    #[test]
    fn read_opcodes_round_trip() {
        for v in [0x01, 0x03, 0x04, 0x05, 0x0a, 0x0b] {
            assert_eq!(OutOpcode::from_u8(v).unwrap().as_u8(), v);
        }
    }
}
