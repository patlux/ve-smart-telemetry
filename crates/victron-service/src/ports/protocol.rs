//! Protocol request/response adapter port.
//!
//! The concrete implementation lives at the seam of `victron-protocol`
//! (wire encoding/decoding, chunk reassembly, VREG decoding) and
//! `victron-domain` (canonical sample normalization). The service only ever
//! sees `AcquirePlan` and the canonical domain `Sample` through this trait.

use victron_domain::Sample;

/// Byte payloads a session needs for one acquisition.
///
/// `negotiation_frames` are control writes (e.g. `fa 80 ff`, `f9 80`),
/// `subscribe_payload` and `values_payload` are CBOR requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcquirePlan {
    pub negotiation_frames: Vec<Vec<u8>>,
    pub subscribe_payload: Vec<u8>,
    pub values_payload: Vec<u8>,
}

/// A single decoded VREG record before normalization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawValue {
    pub vreg: u16,
    pub raw: Vec<u8>,
}

/// Protocol-level failure. Bounded, no raw payloads in display output.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("malformed response")]
    Malformed,
    #[error("truncated response")]
    Truncated,
    #[error("incoming buffer limit exceeded")]
    BufferExceeded,
    #[error("empty response")]
    EmptyResponse,
    #[error("unsupported opcode {0:#x}")]
    UnsupportedOpcode(u8),
    #[error("unknown vreg {0:#x}")]
    UnknownVreg(u16),
    #[error("invalid value for vreg {vreg:#x}")]
    InvalidValue { vreg: u16 },
    #[error("device is not subscribed")]
    NotSubscribed,
    #[error("device rejected the request (response code {code})")]
    ContentionResponse { code: i64 },
    #[error("not wired: {0}")]
    NotWired(&'static str),
}

/// Request encoding + response decoding + sample normalization.
///
/// All methods are synchronous: the protocol crate is runtime-independent.
pub trait ProtocolAdapter: Send + Sync {
    /// The VREG set this collector requests each cycle.
    fn vregs(&self) -> &[u16];

    /// Build the byte payloads for one acquisition on `instance`.
    fn acquire_plan(&self, instance: u16, vregs: &[u16]) -> Result<AcquirePlan, ProtocolError>;

    /// Decode the accumulated Data/LastData stream into raw VREG values.
    fn parse_response(&self, instance: u16, bytes: &[u8]) -> Result<Vec<RawValue>, ProtocolError>;

    /// Normalize raw VREG values into a [`Sample`], rejecting sentinels and
    /// impossible values.
    fn translate(&self, instance: u16, values: &[RawValue]) -> Result<Sample, ProtocolError>;
}
