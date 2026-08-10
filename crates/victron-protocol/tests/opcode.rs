//! Integration tests for the read-only public API contract.
//!
//! The production contract: no settings/write operation variant exists on
//! any public type. `Request` is exhaustive over read-only variants (a
//! write variant would break the match below at compile time), and the
//! settings-write opcodes `0x06`/`0x0c` are not constructible via
//! `OutOpcode::from_u8`.

use victron_protocol::opcode::OutOpcode;
use victron_protocol::Request;

/// Compile-time proof that `Request` has no write variant: this match is
/// exhaustive over the read-only variants, so adding a write variant would
/// fail to compile.
fn request_opcode_is_read_only(r: &Request) -> bool {
    match r {
        Request::GetDevices => true,
        Request::Subscribe { .. } => true,
        Request::GetValues { .. } => true,
        Request::GetPathList { .. } => true,
        Request::GetPathValues { .. } => true,
    }
}

#[test]
fn request_enum_is_exhaustively_read_only() {
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
        assert!(request_opcode_is_read_only(&r));
        assert!(r.opcode().is_read_only());
    }
}

#[test]
fn write_opcodes_are_not_constructible_via_public_api() {
    // The settings-write wire values must not map to any public variant.
    assert_eq!(OutOpcode::from_u8(0x06), None);
    assert_eq!(OutOpcode::from_u8(0x0c), None);
}

#[test]
fn every_public_out_opcode_is_read_only() {
    for v in 0..=0xffu8 {
        if let Some(op) = OutOpcode::from_u8(v) {
            assert!(op.is_read_only(), "opcode 0x{v:02x} must be read-only");
        }
    }
}
