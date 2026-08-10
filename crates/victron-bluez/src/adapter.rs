//! BlueZ adapter selection and power-state handling.

use std::time::Duration;

use bluer::{Adapter, Session};

use crate::error::{from_bluer, BleError};
use crate::timeout::bounded;

/// Default BlueZ adapter name.
pub const DEFAULT_ADAPTER: &str = "hci0";

/// How the transport treats a powered-off adapter.
///
/// The `Powered` property is a **host-wide** policy switch: it affects
/// discoverability, connectability, BR/EDR and LE operation of the whole
/// controller. This crate never flips it silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PowerPolicy {
    /// Never change adapter state. A powered-off adapter yields
    /// [`BleError::AdapterNotPowered`] with operator instructions.
    #[default]
    RequireManual,
    /// Explicit opt-in: enable a powered-off adapter, logging the
    /// host-policy change. This is a broad host policy change.
    EnableIfOff,
}

/// Pure adapter-name selection.
///
/// Returns the configured name when present on the host, otherwise the
/// default `hci0`, otherwise the first available adapter, otherwise
/// [`BleError::AdapterNotFound`].
pub fn pick_adapter_name<'a>(
    names: impl IntoIterator<Item = &'a str>,
    configured: Option<&str>,
) -> Result<String, BleError> {
    let names: Vec<&str> = names.into_iter().collect();
    if let Some(configured) = configured {
        if names.contains(&configured) {
            return Ok(configured.to_string());
        }
        return Err(BleError::AdapterNotFound {
            requested: configured.to_string(),
        });
    }
    if names.contains(&DEFAULT_ADAPTER) {
        return Ok(DEFAULT_ADAPTER.to_string());
    }
    names
        .first()
        .map(|name| (*name).to_string())
        .ok_or_else(|| BleError::AdapterNotFound {
            requested: DEFAULT_ADAPTER.to_string(),
        })
}

/// Resolve the configured adapter from a live session.
pub async fn resolve_adapter(
    session: &Session,
    configured: Option<&str>,
    op_timeout: Duration,
) -> Result<Adapter, BleError> {
    let names = bounded("adapter-names", op_timeout, async {
        session.adapter_names().await.map_err(|e| from_bluer(&e))
    })
    .await?;
    let name = pick_adapter_name(names.iter().map(String::as_str), configured)?;
    session.adapter(&name).map_err(|e| from_bluer(&e))
}

/// Ensure the adapter is powered, honoring [`PowerPolicy`].
pub async fn ensure_powered(
    adapter: &Adapter,
    policy: PowerPolicy,
    op_timeout: Duration,
) -> Result<(), BleError> {
    let powered = bounded("adapter-powered", op_timeout, async {
        adapter.is_powered().await.map_err(|e| from_bluer(&e))
    })
    .await?;
    if powered {
        return Ok(());
    }
    match policy {
        PowerPolicy::RequireManual => Err(BleError::AdapterNotPowered {
            adapter: adapter.name().to_string(),
        }),
        PowerPolicy::EnableIfOff => {
            log::warn!(
                "adapter '{}' is powered off; enabling it — this is a broad host policy change",
                adapter.name()
            );
            bounded("adapter-set-powered", op_timeout, async {
                adapter.set_powered(true).await.map_err(|e| from_bluer(&e))
            })
            .await?;
            let powered = bounded("adapter-powered", op_timeout, async {
                adapter.is_powered().await.map_err(|e| from_bluer(&e))
            })
            .await?;
            if powered {
                Ok(())
            } else {
                Err(BleError::AdapterPowerFailed {
                    adapter: adapter.name().to_string(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::BleErrorClass;

    #[test]
    fn picks_configured_adapter() {
        let names = ["hci0", "hci1"];
        assert_eq!(pick_adapter_name(names, Some("hci1")).unwrap(), "hci1");
    }

    #[test]
    fn defaults_to_hci0_when_unconfigured() {
        let names = ["hci3", "hci0"];
        assert_eq!(pick_adapter_name(names, None).unwrap(), "hci0");
    }

    #[test]
    fn falls_back_to_first_adapter_without_hci0() {
        let names = ["hci3", "hci7"];
        assert_eq!(pick_adapter_name(names, None).unwrap(), "hci3");
    }

    #[test]
    fn missing_configured_adapter_is_not_found() {
        let names = ["hci0", "hci1"];
        let err = pick_adapter_name(names, Some("hci9")).unwrap_err();
        assert_eq!(err.class(), BleErrorClass::NotFound);
        assert!(err.to_string().contains("hci9"));
    }

    #[test]
    fn no_adapters_is_not_found() {
        let names: [&str; 0] = [];
        let err = pick_adapter_name(names, None).unwrap_err();
        assert_eq!(err.class(), BleErrorClass::NotFound);
    }
}
