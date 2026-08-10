//! Validated stable device identity.
//!
//! A [`DeviceId`] is the stable local name used to identify one configured
//! Victron device (e.g. `"solar-charger"`). It is independent of the wire
//! protocol and of any Bluetooth identity: the same device keeps the same id
//! even if its BLE address or bond changes.

use crate::error::DomainError;
use std::fmt;
use std::str::FromStr;

/// Maximum length of a device id in bytes.
///
/// Bound keeps ids usable as metric label values, database keys, and log
/// tags without unbounded growth. 64 bytes is far below the Prometheus label
/// value limit and any SQLite `TEXT` key concern.
pub const MAX_DEVICE_ID_LEN: usize = 64;

/// A validated, stable local device name.
///
/// Rules:
///
/// - non-empty
/// - at most [`MAX_DEVICE_ID_LEN`] bytes
/// - only ASCII letters, digits, `-`, `_`, `.` (no whitespace, no slashes,
///   no non-ASCII characters)
///
/// The charset keeps ids safe as metric labels, file/DB keys, and log
/// strings without the crate knowing any particular backend.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DeviceId {
    name: String,
}

impl DeviceId {
    /// Validate and construct a device id.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidDeviceId`] when the name is empty, too
    /// long, contains surrounding whitespace, or contains a character
    /// outside `[A-Za-z0-9._-]`.
    pub fn new(name: &str) -> Result<Self, DomainError> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(DomainError::InvalidDeviceId {
                value: name.to_string(),
                reason: "must not be empty",
            });
        }
        if trimmed.len() > MAX_DEVICE_ID_LEN {
            return Err(DomainError::InvalidDeviceId {
                value: name.to_string(),
                reason: "must be at most 64 bytes",
            });
        }
        if trimmed != name {
            return Err(DomainError::InvalidDeviceId {
                value: name.to_string(),
                reason: "must not contain surrounding whitespace",
            });
        }
        if !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        {
            return Err(DomainError::InvalidDeviceId {
                value: name.to_string(),
                reason: "only ASCII letters, digits, '-', '_' and '.' are allowed",
            });
        }
        Ok(Self {
            name: trimmed.to_string(),
        })
    }

    /// The device id as a `&str`.
    pub fn as_str(&self) -> &str {
        &self.name
    }

    /// Length of the id in bytes.
    pub fn len(&self) -> usize {
        self.name.len()
    }

    /// The id is never empty by construction.
    pub fn is_empty(&self) -> bool {
        false
    }
}

impl AsRef<str> for DeviceId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DeviceId {
    type Err = DomainError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<&str> for DeviceId {
    type Error = DomainError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn hash_of(id: &DeviceId) -> u64 {
        let mut h = DefaultHasher::new();
        id.hash(&mut h);
        h.finish()
    }

    #[test]
    fn accepts_valid_names() {
        for name in [
            "solar-charger",
            "Solar_Charger",
            "victron-01",
            "load.output_2",
            "a",
        ] {
            let id = DeviceId::new(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(id.as_str(), name);
            assert_eq!(id.len(), name.len());
            assert!(!id.is_empty());
        }
    }

    #[test]
    fn accepts_max_length_name() {
        let name = "a".repeat(MAX_DEVICE_ID_LEN);
        let id = DeviceId::new(&name).unwrap();
        assert_eq!(id.as_str(), name);
    }

    #[test]
    fn rejects_empty_and_whitespace() {
        for name in ["", "   ", "\t\n"] {
            assert!(matches!(
                DeviceId::new(name),
                Err(DomainError::InvalidDeviceId { .. })
            ));
        }
    }

    #[test]
    fn rejects_surrounding_whitespace() {
        for name in [" solar-charger", "solar-charger ", " solar-charger "] {
            assert!(DeviceId::new(name).is_err());
        }
    }

    #[test]
    fn rejects_internal_whitespace_and_illegal_chars() {
        for name in [
            "with space",
            "slas/h",
            "colon:1",
            "umlautä",
            "emoji🎉",
            "quote'",
            "dquote\"",
        ] {
            assert!(
                DeviceId::new(name).is_err(),
                "expected rejection for {:?}",
                name
            );
        }
    }

    #[test]
    fn rejects_too_long() {
        let name = "a".repeat(MAX_DEVICE_ID_LEN + 1);
        assert!(DeviceId::new(&name).is_err());
    }

    #[test]
    fn error_carries_input_and_reason() {
        let err = DeviceId::new("").unwrap_err();
        match err {
            DomainError::InvalidDeviceId { value, reason } => {
                assert_eq!(value, "");
                assert!(reason.contains("empty"));
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn from_str_and_try_from_match_new() {
        let a: DeviceId = "solar-charger".parse().unwrap();
        let b = DeviceId::try_from("solar-charger").unwrap();
        let c = DeviceId::new("solar-charger").unwrap();
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert!("bad id!".parse::<DeviceId>().is_err());
    }

    #[test]
    fn display_and_as_ref() {
        let id = DeviceId::new("solar-charger").unwrap();
        assert_eq!(id.to_string(), "solar-charger");
        assert_eq!(id.as_ref(), "solar-charger");
    }

    #[test]
    fn equality_and_hash_are_structural() {
        let a = DeviceId::new("solar-charger").unwrap();
        let b = DeviceId::new("solar-charger").unwrap();
        let c = DeviceId::new("other").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(hash_of(&a), hash_of(&b));
    }
}
