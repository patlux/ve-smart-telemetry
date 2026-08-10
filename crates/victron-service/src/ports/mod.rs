//! Narrow port traits separating orchestration from infrastructure.
//!
//! Every external capability the service needs is expressed as a small trait.
//! Concrete implementations come from sibling crates (`victron-bluez`,
//! `victron-protocol`, `victron-domain`, `victron-storage`, `victron-metrics`)
//! or from adapters in the binaries. The service tests inject fakes.

pub mod ble;
pub mod clock;
pub mod delivery;
pub mod protocol;
pub mod storage;

use std::sync::Arc;

use ble::BleSession;
use delivery::{BatchRenderer, MetricsDelivery};
use protocol::ProtocolAdapter;
use storage::StoragePort;

/// All external ports bundled for one collector instance.
pub struct CyclePorts {
    /// BLE session: discovery, connection, negotiation, subscription, request,
    /// teardown. Sibling: `victron-bluez`.
    pub ble: Box<dyn BleSession>,
    /// Protocol request/response adapter. Sibling: `victron-protocol` +
    /// `victron-domain` translation.
    pub protocol: Arc<dyn ProtocolAdapter>,
    /// Durable state + delivery spool. Sibling: `victron-storage`.
    pub storage: Box<dyn StoragePort>,
    /// VictoriaMetrics import client. Sibling: `victron-metrics`.
    pub delivery: Box<dyn MetricsDelivery>,
    /// Prometheus text batch rendering. Sibling: `victron-metrics`.
    pub renderer: Arc<dyn BatchRenderer>,
}

impl CyclePorts {
    pub fn new(
        ble: Box<dyn BleSession>,
        protocol: Arc<dyn ProtocolAdapter>,
        storage: Box<dyn StoragePort>,
        delivery: Box<dyn MetricsDelivery>,
        renderer: Arc<dyn BatchRenderer>,
    ) -> Self {
        Self {
            ble,
            protocol,
            storage,
            delivery,
            renderer,
        }
    }
}
