//! BLE session port.
//!
//! The concrete implementation lives in `victron-bluez` (BlueZ/D-Bus via
//! `bluer`). It owns GATT service discovery, characteristic plumbing, CCCD
//! writes, notification handling and CBOR chunking. The service drives it
//! through coarse phases and hands it the exact byte payloads produced by the
//! protocol adapter.
//!
//! Seam note: `victron-bluez` is built in parallel and its exact API is not
//! final; this trait is the contract the bluez lane should implement.

use std::time::Duration;

use async_trait::async_trait;

/// Classification of a BLE failure. Kept bounded and free of sensitive data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BleError {
    /// Configured device was not found during discovery.
    #[error("device not found")]
    NotFound,
    /// Device out of range or link lost.
    #[error("device out of range")]
    OutOfRange,
    /// Another client (e.g. VictronConnect) holds or steals the connection.
    #[error("connection contention")]
    Contention,
    /// Bond/pairing problem.
    #[error("authentication failure")]
    Authentication,
    /// A bounded BLE operation or service phase exceeded its deadline.
    #[error("timeout: {operation}")]
    Timeout { operation: &'static str },
    /// Connection dropped while a request was in flight.
    #[error("connection dropped")]
    Disconnected,
    /// Adapter/BlueZ level failure.
    #[error("transport: {0}")]
    Transport(String),
    /// Anything else; keep the payload small and non-sensitive.
    #[error("{0}")]
    Other(String),
    /// The adapter is a placeholder until a concrete sibling crate is wired.
    #[error("not wired: {0}")]
    NotWired(&'static str),
}

impl BleError {
    /// Stable, payload-free classification for structured logs.
    pub fn kind(&self) -> &'static str {
        match self {
            BleError::NotFound => "not_found",
            BleError::OutOfRange => "out_of_range",
            BleError::Contention => "contention",
            BleError::Authentication => "authentication",
            BleError::Timeout { .. } => "timeout",
            BleError::Disconnected => "disconnected",
            BleError::Transport(_) => "transport",
            BleError::Other(_) => "other",
            BleError::NotWired(_) => "not_wired",
        }
    }

    /// Bounded operation label when the failure is a timeout.
    pub fn operation(&self) -> Option<&'static str> {
        match self {
            BleError::Timeout { operation } => Some(operation),
            _ => None,
        }
    }
}

/// One short-lived BLE session: discover -> connect -> negotiate -> subscribe
/// -> request -> disconnect.
///
/// The trait is `?Send`-friendly, matching the BlueZ lane's
/// `victron_bluez::BleTransport` (`#[async_trait(?Send)]`): a single-threaded
/// collector does not require the session (or its notification streams) to be
/// `Send`, and requiring `Send` futures would make awaiting the real BlueZ
/// transport impossible.
#[async_trait(?Send)]
pub trait BleSession {
    /// Resolve the configured bonded device.
    async fn discover(&mut self) -> Result<(), BleError>;

    /// Connect GATT and locate the VE.Smart service.
    async fn connect(&mut self) -> Result<(), BleError>;

    /// Negotiate the VE.Smart control channel.
    ///
    /// `frames` are the raw control writes produced by the protocol adapter
    /// (e.g. `fa 80 ff`, then `f9 80`). The session decides how to interleave
    /// control reads/notifications.
    async fn negotiate(&mut self, frames: &[Vec<u8>]) -> Result<(), BleError>;

    /// Enable notifications and subscribe to `instance`.
    ///
    /// `payload` is the CBOR `subscribe` request from the protocol adapter.
    async fn subscribe(&mut self, instance: u16, payload: &[u8]) -> Result<(), BleError>;

    /// Send one bounded `getValues` request and wait for the complete response.
    ///
    /// `payload` is the CBOR request from the adapter; `timeout` is the
    /// protocol response deadline. Returns the accumulated Data/LastData
    /// byte stream for the protocol adapter to parse.
    async fn request_values(
        &mut self,
        payload: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>, BleError>;

    /// Finish a successful acquisition cycle. Implementations may retain a
    /// healthy connection they created for reuse by the next cycle. The
    /// default remains a hard disconnect, so implementations keep the
    /// original lifecycle unless they explicitly opt into reuse.
    async fn finish_cycle(&mut self) -> Result<(), BleError> {
        self.disconnect().await
    }

    /// Hard-close the connection. Failure and shutdown paths always use this
    /// operation. Must be idempotent and safe to call after any failure.
    async fn disconnect(&mut self) -> Result<(), BleError>;
}
