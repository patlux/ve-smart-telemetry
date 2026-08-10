//! VE.Smart GATT service location and characteristic validation.

use std::time::Duration;

use bluer::gatt::remote::{Characteristic, CharacteristicWriteRequest};
use bluer::gatt::{CharacteristicFlags, WriteOp};
use bluer::Device;

use crate::error::{from_bluer, BleError};
use crate::spec::{self, CharRole, ServiceVariant};
use crate::timeout::bounded;

/// Which write procedure to use for an outbound characteristic. Decided once
/// during [`locate`] from the peer's flags; the transport then never re-reads
/// flags per write (no extra D-Bus round trip, no extra hang point).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// `write` / WriteCommand (no response, ATT Write Without Response).
    WriteWithoutResponse,
    /// `write_ext` with `WriteOp::Request` (ATT Write Request/Response).
    WriteWithResponse,
}

/// Located VE.Smart GATT surface on a connected device.
#[derive(Clone)]
pub struct VeSmartGatt {
    /// Which service variant (`...dfd0` or `...dfd1`) was found.
    pub variant: ServiceVariant,
    /// Control characteristic (`...0002`).
    pub control: Characteristic,
    /// Decided write procedure for Control.
    pub control_write: WriteMode,
    /// LastData characteristic (`...0003`).
    pub last_data: Characteristic,
    /// Decided write procedure for LastData.
    pub last_data_write: WriteMode,
    /// Data characteristic (`...0004`).
    pub data: Characteristic,
    /// Decided write procedure for Data.
    pub data_write: WriteMode,
}

/// Pure: map a characteristic UUID to its role under a service variant.
pub fn role_for_uuid(variant: ServiceVariant, uuid: &bluer::Uuid) -> Option<CharRole> {
    spec::char_role(variant, uuid)
}

/// Pure: validate the characteristic flags required for a role.
///
/// Per the reverse-engineering reference, all three characteristics must be
/// notify/indicate-capable; Control must additionally be readable.
pub fn validate_flags(flags: &CharacteristicFlags, role: CharRole) -> Result<(), BleError> {
    let notifying = flags.notify || flags.indicate;
    if !notifying {
        return Err(BleError::MissingFlag {
            element: role.name(),
            required: "notify|indicate",
        });
    }
    if role == CharRole::Control && !flags.read {
        return Err(BleError::MissingFlag {
            element: role.name(),
            required: "read",
        });
    }
    Ok(())
}

/// Pure: decide the write procedure for an outbound role from its flags.
///
/// Protocol evidence (`victronconnect-protocol-reference.md`): the app writes
/// control opcodes (`f9`, `fa 80 ff`, ready-to-receive credits) to Control and
/// CBOR chunks to Data/LastData. All three outbound roles must therefore be
/// writable (write-without-response preferred, write-with-response accepted).
/// Errors carry the role label so a failure identifies the characteristic.
pub fn pick_write_mode(flags: &CharacteristicFlags, role: CharRole) -> Result<WriteMode, BleError> {
    match role {
        CharRole::Control | CharRole::LastData | CharRole::Data => {
            if flags.write_without_response {
                Ok(WriteMode::WriteWithoutResponse)
            } else if flags.write {
                Ok(WriteMode::WriteWithResponse)
            } else {
                Err(BleError::MissingFlag {
                    element: role.name(),
                    required: "write|write-without-response",
                })
            }
        }
    }
}

/// Locate the VE.Smart service and its three characteristics on a connected
/// device.
///
/// `bluer`'s `Device::services()` internally waits for GATT service
/// resolution (its own ceiling is ~120s); the whole resolution is additionally
/// bounded by `op_timeout` so a dead controller surfaces as
/// [`BleError::Timeout`] instead of stalling the collector.
pub async fn locate(device: &Device, op_timeout: Duration) -> Result<VeSmartGatt, BleError> {
    let services = bounded("service-discovery", op_timeout, async {
        device.services().await.map_err(|e| from_bluer(&e))
    })
    .await?;
    for service in services {
        let service_uuid = bounded("service-uuid", op_timeout, async {
            service.uuid().await.map_err(|e| from_bluer(&e))
        })
        .await?;
        let Some(variant) = spec::service_variant(&service_uuid) else {
            continue;
        };
        log::info!("located VE.Smart service variant {}", variant_name(variant));

        let mut found: [Option<Characteristic>; 3] = [None, None, None];
        let mut write_modes: [WriteMode; 3] = [WriteMode::WriteWithoutResponse; 3];
        for characteristic in bounded("service-characteristics", op_timeout, async {
            service.characteristics().await.map_err(|e| from_bluer(&e))
        })
        .await?
        {
            let char_uuid = bounded("characteristic-uuid", op_timeout, async {
                characteristic.uuid().await.map_err(|e| from_bluer(&e))
            })
            .await?;
            let Some(role) = role_for_uuid(variant, &char_uuid) else {
                continue;
            };
            let flags = bounded("characteristic-flags", op_timeout, async {
                characteristic.flags().await.map_err(|e| from_bluer(&e))
            })
            .await?;
            validate_flags(&flags, role)?;
            write_modes[role.index() as usize - 1] = pick_write_mode(&flags, role)?;
            log::debug!(
                "found characteristic {} (notify/indicate + per-role read/write support)",
                role.name()
            );
            found[role.index() as usize - 1] = Some(characteristic);
        }

        return Ok(VeSmartGatt {
            variant,
            control: found[CharRole::Control.index() as usize - 1]
                .take()
                .ok_or(BleError::GattNotFound { element: "control" })?,
            control_write: write_modes[CharRole::Control.index() as usize - 1],
            last_data: found[CharRole::LastData.index() as usize - 1]
                .take()
                .ok_or(BleError::GattNotFound {
                    element: "last-data",
                })?,
            last_data_write: write_modes[CharRole::LastData.index() as usize - 1],
            data: found[CharRole::Data.index() as usize - 1]
                .take()
                .ok_or(BleError::GattNotFound { element: "data" })?,
            data_write: write_modes[CharRole::Data.index() as usize - 1],
        });
    }
    Err(BleError::GattNotFound {
        element: "ve-smart-service",
    })
}

/// Write `data` to `characteristic` bounded by `max` bytes, using the
/// pre-decided [`WriteMode`] from [`locate`]. Write capability was validated
/// during locate, so a device that lacks write support never reaches this
/// point with a generic error.
pub async fn write_bounded(
    characteristic: &Characteristic,
    mode: WriteMode,
    data: &[u8],
    max: usize,
) -> Result<(), BleError> {
    if data.is_empty() {
        return Err(BleError::InvalidState { operation: "write" });
    }
    if data.len() > max {
        return Err(BleError::PayloadTooLarge {
            len: data.len(),
            max,
        });
    }
    match mode {
        WriteMode::WriteWithoutResponse => {
            characteristic.write(data).await.map_err(|e| from_bluer(&e))
        }
        WriteMode::WriteWithResponse => {
            let request = CharacteristicWriteRequest {
                op_type: WriteOp::Request,
                ..Default::default()
            };
            characteristic
                .write_ext(data, &request)
                .await
                .map_err(|e| from_bluer(&e))
        }
    }
}

/// Stable, MAC-free variant label.
fn variant_name(variant: ServiceVariant) -> &'static str {
    match variant {
        ServiceVariant::Dfd0 => "dfd0",
        ServiceVariant::Dfd1 => "dfd1",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_for_uuid_matches_documented_uuids() {
        let v = ServiceVariant::Dfd0;
        let ctrl = bluer::Uuid::parse_str("306b0002-b081-4037-83dc-e59fcc3cdfd0").unwrap();
        let last = bluer::Uuid::parse_str("306b0003-b081-4037-83dc-e59fcc3cdfd0").unwrap();
        let data = bluer::Uuid::parse_str("306b0004-b081-4037-83dc-e59fcc3cdfd0").unwrap();
        assert_eq!(role_for_uuid(v, &ctrl), Some(CharRole::Control));
        assert_eq!(role_for_uuid(v, &last), Some(CharRole::LastData));
        assert_eq!(role_for_uuid(v, &data), Some(CharRole::Data));
    }

    fn flags(
        read: bool,
        write: bool,
        write_without_response: bool,
        notify: bool,
        indicate: bool,
    ) -> CharacteristicFlags {
        CharacteristicFlags {
            broadcast: false,
            read,
            write_without_response,
            write,
            notify,
            indicate,
            authenticated_signed_writes: false,
            extended_properties: false,
            reliable_write: false,
            writable_auxiliaries: false,
            encrypt_read: false,
            encrypt_write: false,
            encrypt_authenticated_read: false,
            encrypt_authenticated_write: false,
            secure_read: false,
            secure_write: false,
            authorize: false,
        }
    }

    #[test]
    fn notify_or_indicate_accepted_for_non_control_roles() {
        let notify_only = flags(false, false, false, true, false);
        let indicate_only = flags(false, false, false, false, true);
        for role in [CharRole::LastData, CharRole::Data] {
            assert!(validate_flags(&notify_only, role).is_ok());
            assert!(validate_flags(&indicate_only, role).is_ok());
        }
        // Control additionally requires read, so notify-only is rejected here.
        assert!(validate_flags(&notify_only, CharRole::Control).is_err());
    }

    #[test]
    fn control_requires_read() {
        let no_read = flags(false, false, false, true, false);
        let with_read = flags(true, false, false, true, false);
        assert!(validate_flags(&no_read, CharRole::Control).is_err());
        assert!(validate_flags(&with_read, CharRole::Control).is_ok());
        // Data/LastData do not require read.
        assert!(validate_flags(&no_read, CharRole::Data).is_ok());
        assert!(validate_flags(&no_read, CharRole::LastData).is_ok());
    }

    #[test]
    fn missing_notify_is_rejected() {
        let none = flags(true, false, false, false, false);
        assert!(validate_flags(&none, CharRole::Control).is_err());
        assert!(validate_flags(&none, CharRole::Data).is_err());
    }

    #[test]
    fn write_modes_pick_without_response_when_available() {
        let both = flags(false, true, true, false, false);
        let only_request = flags(false, true, false, false, false);
        let only_command = flags(false, false, true, false, false);
        for role in [CharRole::Control, CharRole::LastData, CharRole::Data] {
            assert_eq!(
                pick_write_mode(&both, role).unwrap(),
                WriteMode::WriteWithoutResponse
            );
            assert_eq!(
                pick_write_mode(&only_command, role).unwrap(),
                WriteMode::WriteWithoutResponse
            );
            assert_eq!(
                pick_write_mode(&only_request, role).unwrap(),
                WriteMode::WriteWithResponse
            );
        }
    }

    #[test]
    fn write_requirement_errors_identify_control_vs_data() {
        let notify_only = flags(true, false, false, true, false);
        for (role, label) in [
            (CharRole::Control, "control"),
            (CharRole::LastData, "last-data"),
            (CharRole::Data, "data"),
        ] {
            let err = pick_write_mode(&notify_only, role).unwrap_err();
            assert_eq!(
                err,
                BleError::MissingFlag {
                    element: label,
                    required: "write|write-without-response"
                }
            );
            // Display names the characteristic, never a raw address/payload.
            let text = err.to_string();
            assert!(
                text.contains(label),
                "error must identify the characteristic: {text}"
            );
            assert!(text.contains("write|write-without-response"));
        }
    }
}
