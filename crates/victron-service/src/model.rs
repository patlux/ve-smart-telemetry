//! Service-side identity model.
//!
//! The canonical domain model (`Sample`, `ChargerState`, `Quality`,
//! `DeviceId`, ...) lives in `victron-domain` and is re-exported from
//! [`crate`]. This module only holds the one service-owned identity type that
//! the domain crate deliberately does not model: the configured VE.Smart
//! *instance* paired with the stable device name.

use victron_domain::DeviceId;

/// Stable identity of the configured Victron device: the canonical domain
/// [`DeviceId`] (validated label) plus the VE.Smart instance to subscribe to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    /// Canonical validated device label (metric label, DB key, log tag).
    pub device: DeviceId,
    /// VE.Smart instance to subscribe to (e.g. `3`).
    pub instance: u16,
}

impl DeviceIdentity {
    pub fn new(device: DeviceId, instance: u16) -> Self {
        Self { device, instance }
    }

    /// The device label as a string slice.
    pub fn name(&self) -> &str {
        self.device.as_str()
    }
}
