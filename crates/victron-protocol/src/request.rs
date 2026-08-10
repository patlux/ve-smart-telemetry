//! Read-only outbound VE.Smart requests and their exact CBOR encoding.
//!
//! Wire format (from `analysis/victronconnect-protocol-reference.md` §6 and
//! the proven prototypes): one CBOR unsigned opcode followed by opcode-specific
//! parameters. Encoding is minimal-width and matches `cbor_uint`/`cbor_int`/
//! `cbor_array_uints` in the Python scripts byte for byte.

use minicbor::Encoder;

use crate::opcode::OutOpcode;
use crate::ProtocolError;

/// Maximum number of registers in one `GetValues` request.
pub const MAX_REGISTERS: usize = 512;
/// Maximum number of path indexes in one `GetPathValues` request.
pub const MAX_PATH_INDEXES: usize = 512;

/// A read-only VE.Smart request.
///
/// There is deliberately **no** variant for `setValues`/`setPathValues`,
/// PIN/PUK, or DFU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// `getDevices()` — opcode `0x01`, no parameters. Requests the device
    /// list (response: `DeviceList`).
    GetDevices,
    /// `subscribe(instance)` — opcode `0x03`. Starts value pushes for the
    /// given instance (the tested charger used instance `3`).
    Subscribe {
        /// Device instance id.
        instance: u16,
    },
    /// `getValues(instance, registers)` — opcode `0x05`. Reads the given
    /// VREG ids on the instance (responses: `Value` records).
    GetValues {
        /// Device instance id.
        instance: u16,
        /// VREG register ids to read.
        registers: Vec<u16>,
    },
    /// `getPathList(instance)` — opcode `0x0a`. Requests the compressed path
    /// list (response: `PathList`). The tested charger rejects this opcode
    /// (response code `2`); kept for devices that support it.
    GetPathList {
        /// Device instance id.
        instance: u16,
    },
    /// `getPathValues(instance, path_indexes)` — opcode `0x0b`. Reads path
    /// values by index (responses: `PathValue` records).
    GetPathValues {
        /// Device instance id.
        instance: u16,
        /// Device-defined path indexes (from `PathList`/`NewPath`).
        path_indexes: Vec<i64>,
    },
}

impl Request {
    /// The CBOR opcode of this request.
    pub fn opcode(&self) -> OutOpcode {
        match self {
            Request::GetDevices => OutOpcode::GetDevices,
            Request::Subscribe { .. } => OutOpcode::Subscribe,
            Request::GetValues { .. } => OutOpcode::GetValues,
            Request::GetPathList { .. } => OutOpcode::GetPathList,
            Request::GetPathValues { .. } => OutOpcode::GetPathValues,
        }
    }

    /// Encode to a freshly allocated request payload.
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        let mut out = Vec::new();
        self.encode_into(&mut out)?;
        Ok(out)
    }

    /// Encode into `out` (appended).
    pub fn encode_into(&self, out: &mut Vec<u8>) -> Result<(), ProtocolError> {
        let mut e = Encoder::new(out);
        let _ = e
            .u64(self.opcode().as_u8() as u64)
            .map_err(|err| ProtocolError::Cbor(err.to_string()))?;
        match self {
            Request::GetDevices => {}
            Request::Subscribe { instance } => {
                let _ = e
                    .u64(*instance as u64)
                    .map_err(|err| ProtocolError::Cbor(err.to_string()))?;
            }
            Request::GetValues {
                instance,
                registers,
            } => {
                if registers.len() > MAX_REGISTERS {
                    return Err(ProtocolError::RequestLimit(
                        "getValues supports at most 512 registers",
                    ));
                }
                let _ = e
                    .u64(*instance as u64)
                    .map_err(|err| ProtocolError::Cbor(err.to_string()))?;
                let _ = e
                    .array(registers.len() as u64)
                    .map_err(|err| ProtocolError::Cbor(err.to_string()))?;
                for reg in registers {
                    let _ = e
                        .u64(*reg as u64)
                        .map_err(|err| ProtocolError::Cbor(err.to_string()))?;
                }
            }
            Request::GetPathList { instance } => {
                let _ = e
                    .u64(*instance as u64)
                    .map_err(|err| ProtocolError::Cbor(err.to_string()))?;
            }
            Request::GetPathValues {
                instance,
                path_indexes,
            } => {
                if path_indexes.len() > MAX_PATH_INDEXES {
                    return Err(ProtocolError::RequestLimit(
                        "getPathValues supports at most 512 path indexes",
                    ));
                }
                let _ = e
                    .u64(*instance as u64)
                    .map_err(|err| ProtocolError::Cbor(err.to_string()))?;
                let _ = e
                    .array(path_indexes.len() as u64)
                    .map_err(|err| ProtocolError::Cbor(err.to_string()))?;
                for idx in path_indexes {
                    let _ = e
                        .i64(*idx)
                        .map_err(|err| ProtocolError::Cbor(err.to_string()))?;
                }
            }
        }
        Ok(())
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
    fn get_devices_bytes_match_capture() {
        // Captured request bytes (probe logs) and Python cbor_uint(1).
        assert_eq!(Request::GetDevices.encode().unwrap(), hex("01"));
    }

    #[test]
    fn subscribe_bytes_match_capture() {
        // Captured: 0303 for instance 3.
        assert_eq!(
            Request::Subscribe { instance: 3 }.encode().unwrap(),
            hex("0303")
        );
    }

    #[test]
    fn get_values_11_registers_match_capture() {
        // Captured exact request from the live probe:
        // 05 03 8b 19edbb 19edbd 19edbc 19ed8d 19ed8c 190201 19eda8 19edad 19edaa 19ed8f 19ed8e
        let registers = vec![
            0xEDBB, 0xEDBD, 0xEDBC, 0xED8D, 0xED8C, 0x0201, 0xEDA8, 0xEDAD, 0xEDAA, 0xED8F, 0xED8E,
        ];
        let bytes = Request::GetValues {
            instance: 3,
            registers,
        }
        .encode()
        .unwrap();
        assert_eq!(
            bytes,
            hex("05038b19edbb19edbd19edbc19ed8d19ed8c19020119eda819edad19edaa19ed8f19ed8e")
        );
    }

    #[test]
    fn get_values_single_register() {
        // Python: get_values_request(3, [0xEDBB]) → 05038119edbb
        assert_eq!(
            Request::GetValues {
                instance: 3,
                registers: vec![0xEDBB]
            }
            .encode()
            .unwrap(),
            hex("05038119edbb")
        );
    }

    #[test]
    fn get_path_list_bytes_match_capture() {
        // Captured: 0a03
        assert_eq!(
            Request::GetPathList { instance: 3 }.encode().unwrap(),
            hex("0a03")
        );
    }

    #[test]
    fn get_path_values_positive_indexes() {
        // Python dry-run example: indexes [0..5) → 0b03850001020304
        assert_eq!(
            Request::GetPathValues {
                instance: 3,
                path_indexes: vec![0, 1, 2, 3, 4]
            }
            .encode()
            .unwrap(),
            hex("0b03850001020304")
        );
    }

    #[test]
    fn get_path_values_negative_indexes() {
        // Python: get_path_values_request(3, [0, -1, -12, -100]) → 0b038400202b3863
        assert_eq!(
            Request::GetPathValues {
                instance: 3,
                path_indexes: vec![0, -1, -12, -100]
            }
            .encode()
            .unwrap(),
            hex("0b038400202b3863")
        );
    }

    #[test]
    fn request_size_limits_enforced() {
        let regs = vec![0xEDBB; MAX_REGISTERS + 1];
        assert!(matches!(
            Request::GetValues {
                instance: 3,
                registers: regs
            }
            .encode(),
            Err(ProtocolError::RequestLimit(_))
        ));
        let idxs = vec![0i64; MAX_PATH_INDEXES + 1];
        assert!(matches!(
            Request::GetPathValues {
                instance: 3,
                path_indexes: idxs
            }
            .encode(),
            Err(ProtocolError::RequestLimit(_))
        ));
    }

    #[test]
    fn all_requests_are_read_only() {
        for r in [
            Request::GetDevices,
            Request::Subscribe { instance: 1 },
            Request::GetValues {
                instance: 1,
                registers: vec![],
            },
            Request::GetPathList { instance: 1 },
            Request::GetPathValues {
                instance: 1,
                path_indexes: vec![],
            },
        ] {
            assert!(
                r.opcode().is_read_only(),
                "opcode {} must be read-only",
                r.opcode().as_u8()
            );
        }
    }
}
