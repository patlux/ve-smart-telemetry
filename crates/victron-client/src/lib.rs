//! Shared read-only VE.Smart BLE client.
//!
//! This crate owns the production session protocol used by both the collector
//! daemon and diagnostic CLI: negotiation, outbound chunking, bounded
//! Data/LastData reassembly, receive-credit replenishment, subscription drain,
//! response correlation, one bounded read retry, and deterministic cleanup.

mod flow;
mod session;

pub use session::VeSmartBleSession;
