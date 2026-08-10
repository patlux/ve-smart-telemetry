//! Victron VE.Smart BLE identity: service/characteristic UUIDs and
//! advertisement evidence.
//!
//! Pure logic, no BlueZ, no I/O. Everything here is unit-testable and is the
//! single source of truth for "is this a Victron VE.Smart device?".

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

/// Victron manufacturer (company) identifier, `0x02e1` (737).
pub const VICTRON_MANUFACTURER_ID: u16 = 0x02e1;

/// Mask applied to the first manufacturer-data payload byte by VictronConnect.
pub const VICTRON_MANUFACTURER_DATA_MASK: u8 = 0xfe;

/// Expected first manufacturer-data payload byte after masking.
pub const VICTRON_MANUFACTURER_DATA_BYTE: u8 = 0x10;

/// VE.Smart service UUID variant 0, suffix `dfd0`.
pub const VE_SMART_SERVICE_UUID_0: Uuid = Uuid::from_bytes([
    0x30, 0x6b, 0x00, 0x01, 0xb0, 0x81, 0x40, 0x37, 0x83, 0xdc, 0xe5, 0x9f, 0xcc, 0x3c, 0xdf, 0xd0,
]);

/// VE.Smart service UUID variant 1, suffix `dfd1`.
pub const VE_SMART_SERVICE_UUID_1: Uuid = Uuid::from_bytes([
    0x30, 0x6b, 0x00, 0x01, 0xb0, 0x81, 0x40, 0x37, 0x83, 0xdc, 0xe5, 0x9f, 0xcc, 0x3c, 0xdf, 0xd1,
]);

/// Which VE.Smart service variant a device exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceVariant {
    /// Service UUID ending in `...dfd0`.
    Dfd0,
    /// Service UUID ending in `...dfd1`.
    Dfd1,
}

/// A VE.Smart characteristic role inside the service.
///
/// The Android app derives these by incrementing byte offset 3 of the service
/// UUID by the index: `Control = base + 1`, `LastData = base + 2`,
/// `Data = base + 3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CharRole {
    /// `...0002` — control / negotiation characteristic.
    Control,
    /// `...0003` — last-data chunk (final chunk of a CBOR frame).
    LastData,
    /// `...0004` — data chunk characteristic.
    Data,
}

impl CharRole {
    /// Byte-offset-3 delta for this role (`1`, `2`, `3`).
    pub const fn index(self) -> u8 {
        match self {
            CharRole::Control => 1,
            CharRole::LastData => 2,
            CharRole::Data => 3,
        }
    }

    /// Stable, payload-free label for logs and errors.
    pub const fn name(self) -> &'static str {
        match self {
            CharRole::Control => "control",
            CharRole::LastData => "last-data",
            CharRole::Data => "data",
        }
    }
}

impl ServiceVariant {
    /// Base service UUID bytes for this variant.
    const fn base_bytes(self) -> [u8; 16] {
        match self {
            ServiceVariant::Dfd0 => VE_SMART_SERVICE_UUID_0.into_bytes(),
            ServiceVariant::Dfd1 => VE_SMART_SERVICE_UUID_1.into_bytes(),
        }
    }

    /// The characteristic UUID for a role under this variant.
    pub fn char_uuid(self, role: CharRole) -> Uuid {
        let mut bytes = self.base_bytes();
        bytes[3] = bytes[3].wrapping_add(role.index());
        Uuid::from_bytes(bytes)
    }

    /// Control characteristic UUID for this variant.
    pub fn control_uuid(self) -> Uuid {
        self.char_uuid(CharRole::Control)
    }

    /// LastData characteristic UUID for this variant.
    pub fn last_data_uuid(self) -> Uuid {
        self.char_uuid(CharRole::LastData)
    }

    /// Data characteristic UUID for this variant.
    pub fn data_uuid(self) -> Uuid {
        self.char_uuid(CharRole::Data)
    }
}

/// The VE.Smart service variant matching a UUID, if any.
pub fn service_variant(uuid: &Uuid) -> Option<ServiceVariant> {
    if *uuid == VE_SMART_SERVICE_UUID_0 {
        Some(ServiceVariant::Dfd0)
    } else if *uuid == VE_SMART_SERVICE_UUID_1 {
        Some(ServiceVariant::Dfd1)
    } else {
        None
    }
}

/// The VE.Smart characteristic role of `uuid` under `variant`, if any.
pub fn char_role(variant: ServiceVariant, uuid: &Uuid) -> Option<CharRole> {
    if *uuid == variant.control_uuid() {
        Some(CharRole::Control)
    } else if *uuid == variant.last_data_uuid() {
        Some(CharRole::LastData)
    } else if *uuid == variant.data_uuid() {
        Some(CharRole::Data)
    } else {
        None
    }
}

/// True when any VE.Smart service UUID is advertised by the device.
pub fn has_victron_service_evidence(uuids: &HashSet<Uuid>) -> bool {
    uuids.iter().any(|uuid| service_variant(uuid).is_some())
}

/// True when the manufacturer advertisement carries Victron evidence:
/// company id `0x02e1` and first payload byte `0x10` under mask `0xfe`.
pub fn has_victron_manufacturer_evidence(manufacturer_data: &HashMap<u16, Vec<u8>>) -> bool {
    manufacturer_data
        .get(&VICTRON_MANUFACTURER_ID)
        .and_then(|data| data.first())
        .is_some_and(|byte| {
            (byte & VICTRON_MANUFACTURER_DATA_MASK) == VICTRON_MANUFACTURER_DATA_BYTE
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(s: &str) -> Uuid {
        Uuid::parse_str(s).expect("valid uuid")
    }

    #[test]
    fn service_uuids_parse_to_expected_constants() {
        assert_eq!(
            VE_SMART_SERVICE_UUID_0,
            uuid("306b0001-b081-4037-83dc-e59fcc3cdfd0")
        );
        assert_eq!(
            VE_SMART_SERVICE_UUID_1,
            uuid("306b0001-b081-4037-83dc-e59fcc3cdfd1")
        );
    }

    #[test]
    fn service_variant_detection() {
        assert_eq!(
            service_variant(&uuid("306b0001-b081-4037-83dc-e59fcc3cdfd0")),
            Some(ServiceVariant::Dfd0)
        );
        assert_eq!(
            service_variant(&uuid("306b0001-b081-4037-83dc-e59fcc3cdfd1")),
            Some(ServiceVariant::Dfd1)
        );
        assert_eq!(
            service_variant(&uuid("00001530-1212-efde-1523-785feabcd123")),
            None
        );
    }

    #[test]
    fn characteristic_uuids_derive_from_service_variant() {
        // Documented derivation: byte offset 3 of the service UUID + index.
        assert_eq!(
            ServiceVariant::Dfd0.control_uuid(),
            uuid("306b0002-b081-4037-83dc-e59fcc3cdfd0")
        );
        assert_eq!(
            ServiceVariant::Dfd0.last_data_uuid(),
            uuid("306b0003-b081-4037-83dc-e59fcc3cdfd0")
        );
        assert_eq!(
            ServiceVariant::Dfd0.data_uuid(),
            uuid("306b0004-b081-4037-83dc-e59fcc3cdfd0")
        );
        assert_eq!(
            ServiceVariant::Dfd1.control_uuid(),
            uuid("306b0002-b081-4037-83dc-e59fcc3cdfd1")
        );
        assert_eq!(
            ServiceVariant::Dfd1.last_data_uuid(),
            uuid("306b0003-b081-4037-83dc-e59fcc3cdfd1")
        );
        assert_eq!(
            ServiceVariant::Dfd1.data_uuid(),
            uuid("306b0004-b081-4037-83dc-e59fcc3cdfd1")
        );
    }

    #[test]
    fn char_role_matching() {
        let v = ServiceVariant::Dfd0;
        assert_eq!(char_role(v, &v.control_uuid()), Some(CharRole::Control));
        assert_eq!(char_role(v, &v.last_data_uuid()), Some(CharRole::LastData));
        assert_eq!(char_role(v, &v.data_uuid()), Some(CharRole::Data));
        // Cross-variant UUIDs must not match the wrong variant.
        assert_eq!(char_role(v, &ServiceVariant::Dfd1.data_uuid()), None);
        assert_eq!(
            char_role(v, &uuid("306b0005-b081-4037-83dc-e59fcc3cdfd0")),
            None
        );
    }

    #[test]
    fn service_evidence_detection() {
        let mut uuids = HashSet::new();
        assert!(!has_victron_service_evidence(&uuids));
        uuids.insert(uuid("306b0001-b081-4037-83dc-e59fcc3cdfd0"));
        assert!(has_victron_service_evidence(&uuids));
        uuids.clear();
        // Non-Victron service (e.g. Battery Service, full 128-bit form).
        uuids.insert(uuid("0000180f-0000-1000-8000-00805f9b34fb"));
        assert!(!has_victron_service_evidence(&uuids));
    }

    #[test]
    fn manufacturer_evidence_detection() {
        let mut md = HashMap::new();
        assert!(!has_victron_manufacturer_evidence(&md));
        // Wrong company id.
        md.insert(0x004c, vec![0x10, 0x01]);
        assert!(!has_victron_manufacturer_evidence(&md));
        // Right company id, wrong first byte.
        md.insert(VICTRON_MANUFACTURER_ID, vec![0x02, 0x01]);
        assert!(!has_victron_manufacturer_evidence(&md));
        // Right company id, correct first byte.
        md.insert(VICTRON_MANUFACTURER_ID, vec![0x10, 0x01, 0x02]);
        assert!(has_victron_manufacturer_evidence(&md));
        // Mask: 0x11 also matches under mask 0xfe.
        md.insert(VICTRON_MANUFACTURER_ID, vec![0x11, 0x01]);
        assert!(has_victron_manufacturer_evidence(&md));
        // Empty payload.
        md.insert(VICTRON_MANUFACTURER_ID, vec![]);
        assert!(!has_victron_manufacturer_evidence(&md));
    }
}
