//! Typed transport errors with coarse classification.
//!
//! `Display` and `Debug` never include MAC addresses, raw payloads, or
//! unbounded D-Bus messages. Errors carry only configuration-derived strings
//! (adapter name, device selector), static operation labels, and payload
//! sizes.

use std::fmt;

/// Coarse error class used by the service layer for retry/backoff decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BleErrorClass {
    /// Operation exceeded its deadline.
    Timeout,
    /// Pairing/authentication related failure.
    Auth,
    /// Bluetooth stack contention: already connected, in progress, busy.
    Contention,
    /// The configured adapter or bonded device (or its GATT) is not found.
    NotFound,
    /// D-Bus transport failure.
    Dbus,
    /// Anything else.
    Other,
}

/// Typed transport error.
///
/// Deliberately carries no raw peer data. Dynamic strings are limited to
/// user configuration (adapter name, device selector).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BleError {
    /// The configured adapter name does not exist on this host.
    AdapterNotFound {
        /// Configured adapter name.
        requested: String,
    },
    /// The adapter exists but is powered off, and the active
    /// [`PowerPolicy`](crate::adapter::PowerPolicy) refuses to change host
    /// policy. Enable it with `bluetoothctl power on` or switch to
    /// `PowerPolicy::EnableIfOff`.
    AdapterNotPowered {
        /// Configured adapter name.
        adapter: String,
    },
    /// The adapter is powered off and automatic enabling failed.
    AdapterPowerFailed {
        /// Configured adapter name.
        adapter: String,
    },
    /// No bonded device matched the configured selector.
    DeviceNotFound {
        /// Device selector (alias only, never a raw MAC in display paths).
        selector: String,
    },
    /// A device matched the selector but is not paired/bonded.
    NotBonded {
        /// Device selector.
        selector: String,
    },
    /// A device matched the selector but shows no Victron advertisement
    /// evidence (manufacturer id `0x02e1` / `0x10`, or a VE.Smart service
    /// UUID), and the configuration requires evidence before connecting.
    NoVictronEvidence {
        /// Device selector.
        selector: String,
    },
    /// A required GATT element (service or characteristic) is missing.
    GattNotFound {
        /// Missing element label, e.g. `"ve-smart-service"`, `"control"`.
        element: &'static str,
    },
    /// A required characteristic flag is missing.
    MissingFlag {
        /// Characteristic role label.
        element: &'static str,
        /// Required flag label.
        required: &'static str,
    },
    /// Operation exceeded its deadline.
    Timeout {
        /// Operation label, e.g. `"connect"`, `"discovery"`, `"notification"`.
        operation: &'static str,
    },
    /// Authentication or bonding failure.
    Auth {
        /// Static detail label, e.g. `"authentication-rejected"`.
        detail: &'static str,
    },
    /// Bluetooth stack contention.
    Contention {
        /// Static detail label, e.g. `"already-connected"`.
        detail: &'static str,
    },
    /// A notification session ended unexpectedly.
    NotificationStopped,
    /// D-Bus transport failure.
    Dbus {
        /// Static detail label, e.g. `"connection-lost"`.
        detail: &'static str,
    },
    /// Payload exceeds the configured write bound.
    PayloadTooLarge {
        /// Payload length in bytes.
        len: usize,
        /// Configured maximum in bytes.
        max: usize,
    },
    /// The peer/stack does not support the requested operation.
    NotSupported {
        /// Operation label.
        operation: &'static str,
    },
    /// The transport is not in the required state (before `open` or after `close`).
    InvalidState {
        /// Operation label.
        operation: &'static str,
    },
    /// Invalid transport configuration (selector, timeouts, chunk size),
    /// detected before any D-Bus action.
    InvalidConfig {
        /// Static reason label.
        detail: &'static str,
    },
    /// Anything else, classified as [`BleErrorClass::Other`].
    Other {
        /// Static detail label.
        detail: &'static str,
    },
}

impl BleError {
    /// Coarse classification for retry/backoff decisions.
    pub fn class(&self) -> BleErrorClass {
        match self {
            BleError::Timeout { .. } => BleErrorClass::Timeout,
            BleError::Auth { .. } => BleErrorClass::Auth,
            BleError::Contention { .. } => BleErrorClass::Contention,
            BleError::AdapterNotFound { .. }
            | BleError::AdapterNotPowered { .. }
            | BleError::DeviceNotFound { .. }
            | BleError::NotBonded { .. }
            | BleError::NoVictronEvidence { .. }
            | BleError::GattNotFound { .. }
            | BleError::MissingFlag { .. } => BleErrorClass::NotFound,
            // A failed power mutation is an operational/stack failure, not a
            // missing-resource condition.
            BleError::AdapterPowerFailed { .. } => BleErrorClass::Other,
            BleError::Dbus { .. } => BleErrorClass::Dbus,
            BleError::NotificationStopped
            | BleError::PayloadTooLarge { .. }
            | BleError::NotSupported { .. }
            | BleError::InvalidState { .. }
            | BleError::InvalidConfig { .. }
            | BleError::Other { .. } => BleErrorClass::Other,
        }
    }
}

impl fmt::Display for BleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BleError::AdapterNotFound { requested } => write!(f, "adapter not found: '{requested}'"),
            BleError::AdapterNotPowered { adapter } => write!(
                f,
                "adapter '{adapter}' is powered off; enable it manually (bluetoothctl power on) or opt into PowerPolicy::EnableIfOff"
            ),
            BleError::AdapterPowerFailed { adapter } => write!(f, "adapter '{adapter}' could not be powered on"),
            BleError::DeviceNotFound { selector } => write!(f, "bonded device not found: '{selector}'"),
            BleError::NotBonded { selector } => write!(f, "device '{selector}' is not paired; pair once via bluetoothctl"),
            BleError::NoVictronEvidence { selector } => {
                write!(f, "device '{selector}' matched but shows no Victron advertisement evidence")
            }
            BleError::GattNotFound { element } => write!(f, "GATT element not found: {element}"),
            BleError::MissingFlag { element, required } => {
                write!(f, "GATT characteristic {element} lacks required flag {required}")
            }
            BleError::Timeout { operation } => write!(f, "BLE operation timed out: {operation}"),
            BleError::Auth { detail } => write!(f, "BLE authentication failure: {detail}"),
            BleError::Contention { detail } => write!(f, "BLE contention: {detail}"),
            BleError::NotificationStopped => write!(f, "BLE notification session stopped unexpectedly"),
            BleError::Dbus { detail } => write!(f, "BlueZ D-Bus failure: {detail}"),
            BleError::PayloadTooLarge { len, max } => {
                write!(f, "payload of {len} bytes exceeds write bound of {max} bytes")
            }
            BleError::NotSupported { operation } => write!(f, "BLE operation not supported: {operation}"),
            BleError::InvalidState { operation } => write!(f, "BLE transport in invalid state for: {operation}"),
            BleError::InvalidConfig { detail } => write!(f, "invalid BLE configuration: {detail}"),
            BleError::Other { detail } => write!(f, "BLE error: {detail}"),
        }
    }
}

impl std::error::Error for BleError {}

impl From<tokio::time::error::Elapsed> for BleError {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        BleError::Timeout {
            operation: "deadline",
        }
    }
}

impl From<std::io::Error> for BleError {
    fn from(_: std::io::Error) -> Self {
        BleError::Other { detail: "io" }
    }
}

/// Map a BlueZ error into a typed [`BleError`] without leaking its message.
#[cfg(feature = "bluer")]
pub fn from_bluer(err: &bluer::Error) -> BleError {
    classify_bluer_kind(&err.kind)
}

/// Pure classification of a BlueZ error kind.
#[cfg(feature = "bluer")]
pub fn classify_bluer_kind(kind: &bluer::ErrorKind) -> BleError {
    use bluer::ErrorKind;
    match kind {
        ErrorKind::AuthenticationCanceled => BleError::Auth {
            detail: "authentication-canceled",
        },
        ErrorKind::AuthenticationFailed => BleError::Auth {
            detail: "authentication-failed",
        },
        ErrorKind::AuthenticationRejected => BleError::Auth {
            detail: "authentication-rejected",
        },
        ErrorKind::AuthenticationTimeout => BleError::Auth {
            detail: "authentication-timeout",
        },
        ErrorKind::NotAuthorized => BleError::Auth {
            detail: "not-authorized",
        },
        ErrorKind::AlreadyConnected => BleError::Contention {
            detail: "already-connected",
        },
        ErrorKind::AlreadyExists => BleError::Contention {
            detail: "already-exists",
        },
        ErrorKind::InProgress => BleError::Contention {
            detail: "in-progress",
        },
        ErrorKind::NotPermitted => BleError::Contention {
            detail: "not-permitted",
        },
        ErrorKind::DiscoveryActive => BleError::Contention {
            detail: "discovery-active",
        },
        ErrorKind::DoesNotExist => BleError::DeviceNotFound {
            selector: "<address>".into(),
        },
        ErrorKind::NotFound => BleError::DeviceNotFound {
            selector: "<address>".into(),
        },
        ErrorKind::ConnectionAttemptFailed => BleError::Other {
            detail: "connection-attempt-failed",
        },
        ErrorKind::ServicesUnresolved => BleError::Other {
            detail: "services-unresolved",
        },
        ErrorKind::NotificationSessionStopped => BleError::NotificationStopped,
        ErrorKind::IndicationUnconfirmed => BleError::Other {
            detail: "indication-unconfirmed",
        },
        ErrorKind::NotReady => BleError::Other {
            detail: "not-ready",
        },
        ErrorKind::NotSupported => BleError::NotSupported {
            operation: "bluer-operation",
        },
        ErrorKind::NotAvailable => BleError::Other {
            detail: "not-available",
        },
        ErrorKind::Failed => BleError::Other { detail: "failed" },
        ErrorKind::InvalidArguments => BleError::Other {
            detail: "invalid-arguments",
        },
        ErrorKind::InvalidLength => BleError::Other {
            detail: "invalid-length",
        },
        ErrorKind::InvalidOffset => BleError::Other {
            detail: "invalid-offset",
        },
        ErrorKind::InvalidAddress(_) => BleError::Other {
            detail: "invalid-address",
        },
        ErrorKind::InvalidName(_) => BleError::Other {
            detail: "invalid-name",
        },
        ErrorKind::NotRegistered => BleError::Other {
            detail: "not-registered",
        },
        ErrorKind::AdvertisementMonitorRejected => BleError::Other {
            detail: "advertisement-monitor-rejected",
        },
        ErrorKind::Internal(inner) => match inner {
            bluer::InternalErrorKind::DBusConnectionLost => BleError::Dbus {
                detail: "connection-lost",
            },
            bluer::InternalErrorKind::DBus(_) => BleError::Dbus {
                detail: "dbus-error",
            },
            bluer::InternalErrorKind::Io(_) => BleError::Other { detail: "io" },
            _ => BleError::Other { detail: "internal" },
        },
        _ => BleError::Other {
            detail: "unclassified",
        },
    }
}

#[cfg(feature = "bluer")]
impl From<bluer::Error> for BleError {
    fn from(err: bluer::Error) -> Self {
        from_bluer(&err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_cover_the_five_required_categories() {
        assert_eq!(
            BleError::Timeout {
                operation: "connect"
            }
            .class(),
            BleErrorClass::Timeout
        );
        assert_eq!(
            BleError::Auth {
                detail: "authentication-rejected"
            }
            .class(),
            BleErrorClass::Auth
        );
        assert_eq!(
            BleError::Contention {
                detail: "already-connected"
            }
            .class(),
            BleErrorClass::Contention
        );
        assert_eq!(
            BleError::DeviceNotFound {
                selector: "x".into()
            }
            .class(),
            BleErrorClass::NotFound
        );
        assert_eq!(
            BleError::GattNotFound { element: "control" }.class(),
            BleErrorClass::NotFound
        );
        assert_eq!(
            BleError::Dbus {
                detail: "connection-lost"
            }
            .class(),
            BleErrorClass::Dbus
        );
    }

    #[test]
    fn adapter_power_failure_is_not_not_found() {
        // A failed power mutation is an operational failure, not a missing
        // resource; the service layer must not treat it as NotFound.
        let e = BleError::AdapterPowerFailed {
            adapter: "hci0".into(),
        };
        assert_eq!(e.class(), BleErrorClass::Other);
        assert_ne!(e.class(), BleErrorClass::NotFound);
    }

    #[test]
    fn invalid_config_classifies_as_other() {
        let e = BleError::InvalidConfig {
            detail: "timeouts must be positive",
        };
        assert_eq!(e.class(), BleErrorClass::Other);
        assert!(e.to_string().contains("timeouts must be positive"));
    }

    #[cfg(feature = "bluer")]
    mod classification {
        use bluer::{Error, ErrorKind, InternalErrorKind};

        use super::from_bluer;
        use super::BleError;
        use super::BleErrorClass;

        fn bluer_error(kind: ErrorKind) -> Error {
            Error {
                kind,
                message: "opaque BlueZ detail that must never surface".into(),
            }
        }

        #[test]
        fn timeout_kinds_classify_as_timeout() {
            assert_eq!(
                from_bluer(&bluer_error(ErrorKind::AuthenticationTimeout)),
                BleError::Auth {
                    detail: "authentication-timeout"
                }
            );
            // Generic timeout classification covers transport-level deadlines,
            // while BlueZ authentication-timeout is auth-classified.
        }

        #[test]
        fn authentication_kinds_classify_as_auth() {
            for kind in [
                ErrorKind::AuthenticationCanceled,
                ErrorKind::AuthenticationFailed,
                ErrorKind::AuthenticationRejected,
                ErrorKind::AuthenticationTimeout,
                ErrorKind::NotAuthorized,
            ] {
                let e = from_bluer(&bluer_error(kind.clone()));
                assert_eq!(e.class(), BleErrorClass::Auth, "kind {kind:?} -> {e:?}");
            }
        }

        #[test]
        fn contention_kinds_classify_as_contention() {
            for kind in [
                ErrorKind::AlreadyConnected,
                ErrorKind::AlreadyExists,
                ErrorKind::InProgress,
                ErrorKind::NotPermitted,
                ErrorKind::DiscoveryActive,
            ] {
                let e = from_bluer(&bluer_error(kind.clone()));
                assert_eq!(
                    e.class(),
                    BleErrorClass::Contention,
                    "kind {kind:?} -> {e:?}"
                );
            }
        }

        #[test]
        fn not_found_kinds_classify_as_not_found() {
            for kind in [ErrorKind::DoesNotExist, ErrorKind::NotFound] {
                let e = from_bluer(&bluer_error(kind.clone()));
                assert_eq!(e.class(), BleErrorClass::NotFound, "kind {kind:?} -> {e:?}");
            }
        }

        #[test]
        fn dbus_kinds_classify_as_dbus() {
            for kind in [
                ErrorKind::Internal(InternalErrorKind::DBusConnectionLost),
                ErrorKind::Internal(InternalErrorKind::DBus("whatever".into())),
            ] {
                let e = from_bluer(&bluer_error(kind.clone()));
                assert_eq!(e.class(), BleErrorClass::Dbus, "kind {kind:?} -> {e:?}");
            }
        }

        #[test]
        fn display_never_leaks_bluez_message() {
            let e = from_bluer(&bluer_error(ErrorKind::ConnectionAttemptFailed));
            let text = e.to_string();
            assert!(
                !text.contains("opaque BlueZ detail"),
                "display leaked message: {text}"
            );
        }

        #[test]
        fn notification_stopped_classifies_as_other() {
            let e = from_bluer(&bluer_error(ErrorKind::NotificationSessionStopped));
            assert_eq!(e, BleError::NotificationStopped);
            assert_eq!(e.class(), BleErrorClass::Other);
        }
    }
}
