//! Linux-only async BLE transport for Victron VE.Smart devices.
//!
//! This crate talks to the host BlueZ daemon over D-Bus and exposes a narrow,
//! wire-agnostic transport seam. It moves **raw bytes plus characteristic
//! identity** only — no CBOR, no VREG decoding, no domain measurements.
//!
//! # Scope
//!
//! - select a configured adapter (`hci0` by default)
//! - ensure the adapter is powered *without silently changing broad host
//!   policy* (see [`PowerPolicy`](adapter::PowerPolicy))
//! - resolve a bonded, configured device by alias/address plus Victron
//!   manufacturer/service advertisement evidence
//! - connect and locate the VE.Smart service variant `...dfd0` / `...dfd1`
//!   with its `Control`, `LastData`, and `Data` characteristics
//! - subscribe to notifications on all three characteristics
//! - read `Control`, bounded write-without-response where the peer supports it
//! - emit typed notifications tagged with their source characteristic
//! - read RSSI when BlueZ has it, close the session cleanly
//! - classify timeout / authentication / contention / not-found / D-Bus errors
//!
//! # Out of scope
//!
//! Pairing and PIN/PUK automation. BlueZ owns the pre-established bond;
//! pair once with `bluetoothctl` outside this crate.
//!
//! # Runtime requirements (Linux)
//!
//! - a running `bluetoothd` with a D-Bus system bus
//! - an enabled adapter (or explicit [`PowerPolicy::EnableIfOff`](crate::adapter::PowerPolicy::EnableIfOff))
//! - a pre-bonded Victron device
//!
//! The `bluer` backend additionally requires `libdbus-1` (`libdbus-1-dev` on
//! Debian/Raspberry Pi OS) at build time, including the `armhf` variant when
//! cross-compiling for the Pi Zero W.

pub mod error;
pub mod spec;
mod timeout;
pub mod transport;

#[cfg(feature = "bluer")]
pub mod adapter;
#[cfg(feature = "bluer")]
pub mod discovery;
#[cfg(feature = "bluer")]
pub mod gatt;
#[cfg(feature = "bluer")]
pub mod session;

#[cfg(feature = "bluer")]
pub use session::{BluezTransport, TransportConfig};

pub use error::{BleError, BleErrorClass};
pub use transport::{BleTransport, Notification, NotificationSource};
