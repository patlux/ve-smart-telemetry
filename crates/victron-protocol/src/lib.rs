//! # victron-protocol
//!
//! Pure, runtime-independent implementation of the read-only **VE.Smart** BLE
//! protocol used by Victron solar chargers (and other Smart devices) with
//! VictronConnect. No BlueZ, no async runtime, no filesystem, no HTTP — this
//! crate only speaks the wire protocol and is therefore portable to any
//! transport (Linux/BlueZ, macOS, a test harness, a captured fixture).
//!
//! ## Scope
//!
//! This crate implements the read-only subset that a collector needs:
//!
//! * VE.Smart service/characteristic UUID constants (not BlueZ types).
//! * Control characteristic negotiation: `fa 80 ff`, `f9 80`, incoming `f9`,
//!   `f8`, `f7` messages, and the initial 7-byte control info read.
//! * Outbound CBOR request encoding: `getDevices`, `subscribe`, `getValues`,
//!   `getPathList`, `getPathValues`.
//! * Bounded Data/LastData chunk reassembly.
//! * Typed outbound chunking (Data for non-final chunks, LastData for the
//!   final chunk) via [`outbound::split_request`].
//! * Concatenated CBOR stream decoding into a generic value tree.
//! * Typed response records (DeviceList, Value, Response, PathList, NewPath,
//!   PathValue, ...).
//! * VREG payload decoding with scaling, sentinel rejection, and an explicit
//!   `Confidence` marker so candidate decoders are never presented as
//!   confirmed.
//!
//! ## Explicitly NOT implemented (intentional)
//!
//! * **Settings writes** (`setValues`/`setPathValues` opcodes `0x06`/`0x0c`
//!   and the keep-alive `setValue(0, 0x0093, ...)` write).
//! * **PIN/PUK** flows (base `VeService` characteristics `...0003`/`...0006`).
//! * **DFU** (modern `68c10001-...` and legacy `00001530-...` services).
//! * No write/settings/DFU variant exists on any public type.
//!
//! ## Unresolved protocol items
//!
//! * **Lifetime yield (`0xed8e`)**: no decoder mapping exists yet — the
//!   live reader only lists it as an opaque generic-power fallback. The
//!   crate deliberately does not invent a scaling; see `vreg` docs.
//! * **Live final-chunk behavior**: the exact characteristic alternation
//!   rule for multi-chunk outbound writes is pending live BLE confirmation.
//!   [`outbound::split_request`] commits only to the final-chunk rule
//!   (non-final → Data, final → LastData), which is what the inbound
//!   reassembler depends on.
//!
//! ## Evidence base
//!
//! Behavior is ported from the reverse-engineering notes in
//! `analysis/victronconnect-protocol-reference.md` and the proven Python
//! readers `scripts/read-victron-live-values.py` and
//! `scripts/read-victron-history.py`. Captured wire frames (sanitized, from a
//! user-owned MPPT charger) live in `fixtures/protocol/` and are exercised by
//! the test suite. Field semantics that static analysis could only infer are
//! marked `candidate` in the docs; `Confidence::Confirmed` is used only for
//! solar voltage (`0xedbb`), the one decoder the live tooling confirmed.
//!
//! ## Limits (bounds, tuned for a Raspberry Pi Zero W)
//!
//! * Reassembler buffer: 64 KiB by default (`frame::Reassembler`).
//! * CBOR nesting depth ≤ 16, item budget ≤ 4096 per stream, per-item
//!   string/bytes ≤ 64 KiB, array ≤ 65536 elements (`cbor`).
//! * Requests: ≤ 512 registers / 512 path indexes.
//!
//! ## Example
//!
//! ```
//! use victron_protocol::{Request, Response, frame::Reassembler, vreg::VregValue};
//!
//! // 1. Encode a getValues request for the confirmed dashboard registers.
//! let req = Request::GetValues { instance: 3, registers: vec![0xedbb, 0xed8d] };
//! let bytes = req.encode().unwrap();
//! assert_eq!(bytes, vec![0x05, 0x03, 0x82, 0x19, 0xed, 0xbb, 0x19, 0xed, 0x8d]);
//!
//! // 2. Reassemble notifications into complete payloads, then parse.
//! let mut ra = Reassembler::new();
//! // A captured single-frame LastData notification (complete stream):
//! let payload = ra.push_last_data(&[0x08, 0x03, 0x19, 0xed, 0xbb, 0x42, 0xf3, 0x0a]).unwrap();
//! let responses = Response::parse_stream(payload.as_deref().unwrap()).unwrap();
//! assert!(matches!(&responses[0], Response::Value { register: 0xedbb, data, .. } if data == &[0xf3, 0x0a]));
//!
//! // 3. Decode the raw VREG payload (solar voltage, confirmed u16/100).
//! let v = VregValue::new(0xedbb, vec![0xf3, 0x0a]).decode();
//! assert_eq!(v.value, Some(victron_protocol::vreg::Scaled::Number(28.03)));
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs, rust_2018_idioms)]

pub mod cbor;
pub mod control;
pub mod frame;
pub mod opcode;
pub mod outbound;
pub mod request;
pub mod response;
pub mod vreg;

// Root re-exports of the primary public API (small, documented surface).
pub use cbor::Item;
pub use frame::{Reassembler, ReassemblyError};
pub use outbound::{split_request, OutboundChunk, OutboundTarget};
pub use request::Request;
pub use response::{Response, ResponseCode};
pub use vreg::{Confidence, DecodedVreg, Invalid, Scaled, Slot, VregValue};

/// Victron manufacturer company id in BLE advertisement manufacturer data.
pub const VICTRON_MANUFACTURER_ID: u16 = 0x02e1;

/// VE.Smart GATT service UUID, variant ending `dfd0`.
pub const VE_SMART_SERVICE_DFD0: &str = "306b0001-b081-4037-83dc-e59fcc3cdfd0";
/// VE.Smart GATT service UUID, variant ending `dfd1`.
pub const VE_SMART_SERVICE_DFD1: &str = "306b0001-b081-4037-83dc-e59fcc3cdfd1";

/// Control characteristic UUID (service variant `dfd0`).
pub const VE_SMART_CONTROL_DFD0: &str = "306b0002-b081-4037-83dc-e59fcc3cdfd0";
/// LastData characteristic UUID (service variant `dfd0`).
pub const VE_SMART_LAST_DATA_DFD0: &str = "306b0003-b081-4037-83dc-e59fcc3cdfd0";
/// Data characteristic UUID (service variant `dfd0`).
pub const VE_SMART_DATA_DFD0: &str = "306b0004-b081-4037-83dc-e59fcc3cdfd0";

/// Control characteristic UUID (service variant `dfd1`).
pub const VE_SMART_CONTROL_DFD1: &str = "306b0002-b081-4037-83dc-e59fcc3cdfd1";
/// LastData characteristic UUID (service variant `dfd1`).
pub const VE_SMART_LAST_DATA_DFD1: &str = "306b0003-b081-4037-83dc-e59fcc3cdfd1";
/// Data characteristic UUID (service variant `dfd1`).
pub const VE_SMART_DATA_DFD1: &str = "306b0004-b081-4037-83dc-e59fcc3cdfd1";

/// Base Victron service UUID (`VeService`). Its PIN/PUK/DFU characteristics
/// are deliberately out of scope for this read-only crate.
pub const VE_BASE_SERVICE_UUID: &str = "97580001-ddf1-48be-b73e-182664615d8e";

/// The two observed VE.Smart service UUID variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceVariant {
    /// Service UUID ending `...dfd0` (observed on the tested MPPT charger).
    Dfd0,
    /// Service UUID ending `...dfd1`.
    Dfd1,
}

impl ServiceVariant {
    /// Service UUID for this variant.
    pub fn service_uuid(self) -> &'static str {
        match self {
            ServiceVariant::Dfd0 => VE_SMART_SERVICE_DFD0,
            ServiceVariant::Dfd1 => VE_SMART_SERVICE_DFD1,
        }
    }

    /// Control characteristic UUID for this variant.
    pub fn control_uuid(self) -> &'static str {
        match self {
            ServiceVariant::Dfd0 => VE_SMART_CONTROL_DFD0,
            ServiceVariant::Dfd1 => VE_SMART_CONTROL_DFD1,
        }
    }

    /// LastData characteristic UUID for this variant.
    pub fn last_data_uuid(self) -> &'static str {
        match self {
            ServiceVariant::Dfd0 => VE_SMART_LAST_DATA_DFD0,
            ServiceVariant::Dfd1 => VE_SMART_LAST_DATA_DFD1,
        }
    }

    /// Data characteristic UUID for this variant.
    pub fn data_uuid(self) -> &'static str {
        match self {
            ServiceVariant::Dfd0 => VE_SMART_DATA_DFD0,
            ServiceVariant::Dfd1 => VE_SMART_DATA_DFD1,
        }
    }
}

/// The three VE.Smart characteristics for one service variant, as plain
/// strings so callers can map them onto their transport without a BlueZ
/// dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VeSmartChars {
    /// Control characteristic UUID.
    pub control: &'static str,
    /// LastData characteristic UUID (inbound finalization + outbound requests).
    pub last_data: &'static str,
    /// Data characteristic UUID (inbound chunking).
    pub data: &'static str,
}

impl ServiceVariant {
    /// All three characteristics for this variant.
    pub fn characteristics(self) -> VeSmartChars {
        VeSmartChars {
            control: self.control_uuid(),
            last_data: self.last_data_uuid(),
            data: self.data_uuid(),
        }
    }
}

/// Typed protocol error. Display output never includes captured payload
/// bytes, register dumps, or other device data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// A CBOR decode/encode failure (message only, no payload bytes).
    Cbor(String),
    /// Input ended in the middle of a CBOR item.
    Truncated,
    /// CBOR nesting exceeded [`cbor::MAX_DEPTH`].
    DepthLimit,
    /// CBOR item count exceeded [`cbor::MAX_ITEMS`].
    ItemLimit,
    /// A response carried a register value outside the `u16` range.
    RegisterOutOfRange(u64),
    /// A known-but-unhandled or structurally unexpected message.
    Malformed(&'static str),
    /// Reassembly buffer would exceed its configured capacity.
    BufferLimit {
        /// Configured capacity in bytes.
        capacity: usize,
        /// Total bytes needed (buffered + incoming chunk).
        needed: usize,
    },
    /// A request exceeded its documented size limit.
    RequestLimit(&'static str),
    /// Outbound chunking was asked for an empty payload or a zero chunk
    /// size (see [`crate::outbound::split_request`]).
    InvalidOutbound(&'static str),
}

impl core::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ProtocolError::Cbor(m) => write!(f, "cbor error: {m}"),
            ProtocolError::Truncated => write!(f, "truncated cbor item"),
            ProtocolError::DepthLimit => write!(f, "cbor nesting depth limit exceeded"),
            ProtocolError::ItemLimit => write!(f, "cbor item budget exceeded"),
            ProtocolError::RegisterOutOfRange(v) => {
                write!(f, "register value {v} out of u16 range")
            }
            ProtocolError::Malformed(what) => write!(f, "malformed protocol message: {what}"),
            ProtocolError::BufferLimit { capacity, needed } => {
                write!(f, "reassembly buffer limit ({needed} > {capacity} bytes)")
            }
            ProtocolError::RequestLimit(what) => write!(f, "request exceeds limit: {what}"),
            ProtocolError::InvalidOutbound(what) => write!(f, "invalid outbound chunking: {what}"),
        }
    }
}

impl std::error::Error for ProtocolError {}

impl From<minicbor::decode::Error> for ProtocolError {
    fn from(e: minicbor::decode::Error) -> Self {
        if e.is_end_of_input() {
            ProtocolError::Truncated
        } else {
            ProtocolError::Cbor(e.to_string())
        }
    }
}
