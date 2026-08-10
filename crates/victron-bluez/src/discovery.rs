//! Resolution of the configured bonded Victron device.
//!
//! Resolution uses the configured alias/address **plus** Victron
//! advertisement evidence (manufacturer id `0x02e1` with `0x10` byte, or a
//! VE.Smart service UUID). A bounded discovery scan refreshes advertisement
//! data before the transport gives up.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use bluer::{Adapter, Address, Device, Uuid};
use futures::StreamExt;

use crate::error::{from_bluer, BleError};
use crate::spec;
use crate::timeout::bounded;

/// Maximum length of a configured device alias (BlueZ aliases are short).
pub const MAX_ALIAS_LEN: usize = 64;

/// Which bonded device to resolve. At least one of `alias` or `address` must
/// be configured and non-empty.
#[derive(Debug, Clone)]
pub struct DeviceSelector {
    /// Friendly name/alias, e.g. `"Solar Charger"`. Case-insensitive.
    pub alias: Option<String>,
    /// MAC address as a fallback identifier.
    pub address: Option<Address>,
}

impl DeviceSelector {
    /// Build a validated selector.
    pub fn new(alias: Option<String>, address: Option<Address>) -> Result<Self, BleError> {
        let selector = Self { alias, address };
        selector.validate()?;
        Ok(selector)
    }

    /// Pure validation: at least one of alias/address must be set; a
    /// configured alias must be non-empty after trimming and bounded in
    /// length. There is deliberately no `Default` that silently yields an
    /// unusable selector.
    pub fn validate(&self) -> Result<(), BleError> {
        let alias = self.alias.as_deref().map(str::trim).unwrap_or("");
        let has_alias = !alias.is_empty();
        if !has_alias && self.address.is_none() {
            return Err(BleError::InvalidConfig {
                detail: "device selector requires a non-empty alias and/or address",
            });
        }
        if has_alias && alias.chars().count() > MAX_ALIAS_LEN {
            return Err(BleError::InvalidConfig {
                detail: "device selector alias exceeds 64 characters",
            });
        }
        Ok(())
    }

    /// Bounded, MAC-free description for logs and errors.
    pub fn describe(&self) -> String {
        self.alias
            .clone()
            .unwrap_or_else(|| "<configured address>".to_string())
    }
}

/// A device candidate observed during resolution.
#[derive(Debug)]
struct Candidate {
    device: Device,
    matched: bool,
    bonded: bool,
    evidence: bool,
}
/// Pure: does the device identity match the configured selector?
///
/// A configured alias may match either the adapter-reported alias or the
/// (unset alias) reported name, case-insensitively and whitespace-trimmed. A
/// configured address must match exactly. **When both alias and address are
/// configured, both must match**: a device that merely shares the alias while
/// its address differs must never be selected. A configured alias that is
/// empty or whitespace matches nothing.
pub fn selector_matches(
    alias: &str,
    name: Option<&str>,
    address: &Address,
    selector: &DeviceSelector,
) -> bool {
    let alias_wanted = selector
        .alias
        .as_deref()
        .map(str::trim)
        .filter(|want| !want.is_empty());
    let alias_hit = alias_wanted.is_some_and(|want| alias.trim().eq_ignore_ascii_case(want));
    let name_hit = alias_wanted
        .is_some_and(|want| name.is_some_and(|name| name.trim().eq_ignore_ascii_case(want)));
    let address_hit = selector.address.is_some_and(|want| want == *address);
    let alias_configured = alias_wanted.is_some();
    match (alias_configured, selector.address.is_some()) {
        // **When both alias and address are configured, both must match**: a
        // device that merely shares the alias while its address differs must
        // never be selected.
        (true, true) => (alias_hit || name_hit) && address_hit,
        (true, false) => alias_hit || name_hit,
        // Address-only selector: address alone decides (an empty/whitespace
        // alias is treated as unset).
        (false, _) => address_hit,
    }
}

/// Pure: does the device carry any Victron advertisement evidence?
pub fn evidence_ok(uuids: &HashSet<Uuid>, manufacturer_data: &HashMap<u16, Vec<u8>>) -> bool {
    spec::has_victron_service_evidence(uuids)
        || spec::has_victron_manufacturer_evidence(manufacturer_data)
}

/// Read one candidate's properties and classify it against the selector.
///
/// Returns `Ok(None)` for an **unrelated** candidate whose property reads
/// fail transiently — such candidates are skipped and logged without raw
/// addresses or BlueZ messages, so one sick advertising device cannot abort
/// discovery. When the candidate's address equals the explicitly configured
/// address, property errors are propagated unchanged so the operator gets an
/// actionable error instead of a generic not-found.
async fn inspect(
    adapter: &Adapter,
    address: &Address,
    selector: &DeviceSelector,
    op_timeout: Duration,
) -> Result<Option<Candidate>, BleError> {
    let device = adapter.device(*address).map_err(|e| from_bluer(&e))?;
    let is_address_target = selector.address.is_some_and(|want| want == *address);

    // Read only identity first. Advertisement properties of unrelated BLE
    // devices can be slow or transiently unavailable; querying every property
    // for every device made one noisy neighbour consume the entire discovery
    // deadline before the bonded Victron candidate was reached.
    let alias = match bounded("device-alias", op_timeout, async {
        device.alias().await.map_err(|e| from_bluer(&e))
    })
    .await
    {
        Ok(value) => value,
        Err(err) if is_address_target => return Err(err),
        Err(err) => {
            log::debug!(
                "skipping candidate with unreadable alias (class {:?})",
                err.class()
            );
            return Ok(None);
        }
    };
    let name = match bounded("device-name", op_timeout, async {
        device.name().await.map_err(|e| from_bluer(&e))
    })
    .await
    {
        Ok(value) => value,
        Err(err) if is_address_target => return Err(err),
        // Name is optional when BlueZ already has a usable alias.
        Err(_) => None,
    };
    let matched = selector_matches(&alias, name.as_deref(), address, selector);
    if !matched {
        return Ok(None);
    }

    // Only the selector-matched device gets the remaining property reads.
    // Once identity matched, errors are target errors even for alias-only
    // selectors; do not silently degrade them into a generic not-found.
    let bonded = bounded("device-paired", op_timeout, async {
        device.is_paired().await.map_err(|e| from_bluer(&e))
    })
    .await?;
    let uuids = bounded("device-uuids", op_timeout, async {
        device.uuids().await.map_err(|e| from_bluer(&e))
    })
    .await?
    .unwrap_or_default();
    let manufacturer_data = bounded("device-manufacturer-data", op_timeout, async {
        device.manufacturer_data().await.map_err(|e| from_bluer(&e))
    })
    .await?
    .unwrap_or_default();

    Ok(Some(Candidate {
        device,
        matched,
        bonded,
        evidence: evidence_ok(&uuids, &manufacturer_data),
    }))
}

fn choose(
    candidates: &[Candidate],
    selector: &DeviceSelector,
    require_evidence: bool,
) -> Result<Device, BleError> {
    // Preferred: matched + bonded + evidence.
    if let Some(c) = candidates
        .iter()
        .find(|c| c.matched && c.bonded && c.evidence)
    {
        return Ok(c.device.clone());
    }
    // Tolerated: matched + bonded, but evidence will be re-verified after
    // connect via GATT discovery (bonded devices do not always advertise).
    if let Some(c) = candidates.iter().find(|c| c.matched && c.bonded) {
        if !require_evidence {
            return Ok(c.device.clone());
        }
        return Err(BleError::NoVictronEvidence {
            selector: selector.describe(),
        });
    }
    if candidates.iter().any(|c| c.matched && !c.bonded) {
        return Err(BleError::NotBonded {
            selector: selector.describe(),
        });
    }
    Err(BleError::DeviceNotFound {
        selector: selector.describe(),
    })
}

/// Resolve the configured bonded device.
///
/// 1. Inspect all devices already known to the adapter.
/// 2. If none resolves, run a bounded discovery scan (`scan_timeout`), inspect
///    newly seen addresses, then refresh every device currently known to BlueZ.
pub async fn resolve_device(
    adapter: &Adapter,
    selector: &DeviceSelector,
    scan_timeout: Duration,
    require_evidence: bool,
    op_timeout: Duration,
) -> Result<Device, BleError> {
    let known = bounded("device-addresses", op_timeout, async {
        adapter.device_addresses().await.map_err(|e| from_bluer(&e))
    })
    .await?;
    let mut candidates = Vec::new();
    for address in &known {
        if let Some(candidate) = inspect(adapter, address, selector, op_timeout).await? {
            candidates.push(candidate);
        }
    }
    if let Ok(device) = choose(&candidates, selector, require_evidence) {
        return Ok(device);
    }

    log::debug!("bonded device not resolved from known devices; starting bounded discovery scan");
    let mut seen: HashSet<Address> = known.into_iter().collect();
    let discovered = bounded("discovery-start", op_timeout, async {
        adapter
            .discover_devices_with_changes()
            .await
            .map_err(|e| from_bluer(&e))
    })
    .await?;
    futures::pin_mut!(discovered);
    let deadline = tokio::time::sleep(scan_timeout);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            event = discovered.next() => {
                match event {
                    Some(bluer::AdapterEvent::DeviceAdded(address)) => {
                        if seen.insert(address) {
                            if let Some(candidate) = inspect(adapter, &address, selector, op_timeout).await? {
                                candidates.push(candidate);
                                if let Ok(device) = choose(&candidates, selector, require_evidence) {
                                    return Ok(device);
                                }
                            }
                        }
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            () = &mut deadline => {
                log::debug!("discovery scan timed out after {scan_timeout:?}");
                break;
            }
        }
    }

    // Existing BlueZ device objects do not necessarily produce DeviceAdded
    // while discovery merely refreshes their advertisement properties. Build
    // a fresh candidate set after the scan so bonded devices are evaluated
    // from current UUID/manufacturer evidence rather than stale pre-scan data.
    let refreshed_addresses = bounded("device-addresses-refresh", op_timeout, async {
        adapter.device_addresses().await.map_err(|e| from_bluer(&e))
    })
    .await?;
    let mut refreshed = Vec::new();
    for address in &refreshed_addresses {
        if let Some(candidate) = inspect(adapter, address, selector, op_timeout).await? {
            refreshed.push(candidate);
        }
    }
    choose(&refreshed, selector, require_evidence)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(bytes: [u8; 6]) -> Address {
        Address::new(bytes)
    }

    #[test]
    fn selector_matches_alias_case_insensitively() {
        let sel = DeviceSelector {
            alias: Some("Solar Charger".into()),
            address: None,
        };
        assert!(selector_matches("solar charger", None, &addr([1; 6]), &sel));
        assert!(selector_matches(
            " SOLAR CHARGER ",
            None,
            &addr([1; 6]),
            &sel
        ));
        assert!(!selector_matches("other", None, &addr([1; 6]), &sel));
    }

    #[test]
    fn selector_matches_name_fallback() {
        let sel = DeviceSelector {
            alias: Some("Solar Charger".into()),
            address: None,
        };
        assert!(selector_matches(
            "",
            Some("Solar Charger"),
            &addr([1; 6]),
            &sel
        ));
    }

    #[test]
    fn selector_matches_address() {
        let sel = DeviceSelector {
            alias: None,
            address: Some(addr([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff])),
        };
        assert!(selector_matches(
            "anything",
            None,
            &addr([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]),
            &sel
        ));
        assert!(!selector_matches("anything", None, &addr([0x00; 6]), &sel));
    }

    #[test]
    fn both_alias_and_address_must_match_when_both_configured() {
        let wanted = addr([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        let sel = DeviceSelector {
            alias: Some("Solar Charger".into()),
            address: Some(wanted),
        };
        // Exact match on both.
        assert!(selector_matches("solar charger", None, &wanted, &sel));
        // Same alias, different address: must NOT match (task: never connect
        // to a device that merely shares the alias while its address differs).
        assert!(!selector_matches(
            "solar charger",
            None,
            &addr([0x00; 6]),
            &sel
        ));
        // Same address, different alias: must NOT match either.
        assert!(!selector_matches("other", None, &wanted, &sel));
        // Name fallback still applies for the alias half.
        assert!(selector_matches("", Some("solar charger"), &wanted, &sel));
        assert!(!selector_matches(
            "",
            Some("solar charger"),
            &addr([0x00; 6]),
            &sel
        ));
    }

    #[test]
    fn empty_and_whitespace_aliases_never_match() {
        let wanted = addr([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        // Empty/whitespace wanted alias is treated as *unset*: with a matching
        // address the device matches (address-only selector); the whitespace
        // never matches an alias/name half.
        let sel_ws = DeviceSelector {
            alias: Some("   ".into()),
            address: Some(wanted),
        };
        assert!(selector_matches("   ", None, &wanted, &sel_ws));
        // An empty/whitespace wanted alias can never produce an alias/name
        // hit on its own: only the configured address decides.
        assert!(!selector_matches(
            "Solar Charger",
            None,
            &addr([0x00; 6]),
            &sel_ws
        ));
        assert!(!selector_matches(
            "",
            Some("Solar Charger"),
            &addr([0x00; 6]),
            &sel_ws
        ));
        assert!(!selector_matches(
            "anything",
            Some("anything"),
            &addr([0x00; 6]),
            &sel_ws
        ));
        // A wanted alias must not be matched by an empty/whitespace report.
        let sel = DeviceSelector {
            alias: Some("Solar Charger".into()),
            address: None,
        };
        assert!(!selector_matches("   ", None, &addr([1; 6]), &sel));
        assert!(!selector_matches("", Some(""), &addr([1; 6]), &sel));
    }

    #[test]
    fn selector_requires_an_identity_field() {
        assert!(DeviceSelector::new(None, None).is_err());
        assert!(DeviceSelector::new(Some("x".into()), None).is_ok());
        assert!(DeviceSelector::new(None, Some(addr([1; 6]))).is_ok());
        assert!(DeviceSelector::new(Some("x".into()), Some(addr([1; 6]))).is_ok());
    }

    #[test]
    fn selector_rejects_empty_and_whitespace_aliases() {
        for bad in ["", "   ", "\t\n"] {
            let err = DeviceSelector::new(Some(bad.to_string()), None).unwrap_err();
            assert_eq!(
                err,
                BleError::InvalidConfig {
                    detail: "device selector requires a non-empty alias and/or address"
                }
            );
        }
        // A whitespace alias alongside a real address is an address-only
        // selector: accepted, and the alias half never matches anything.
        assert!(DeviceSelector::new(Some(" ".into()), Some(addr([1; 6]))).is_ok());
        let sel = DeviceSelector::new(Some("  ".into()), Some(addr([1; 6]))).unwrap();
        assert!(selector_matches("  ", None, &addr([1; 6]), &sel));
        assert!(!selector_matches("anything", None, &addr([0x00; 6]), &sel));
    }

    #[test]
    fn selector_rejects_overlong_aliases() {
        let long = "a".repeat(MAX_ALIAS_LEN + 1);
        let err = DeviceSelector::new(Some(long), None).unwrap_err();
        assert_eq!(
            err,
            BleError::InvalidConfig {
                detail: "device selector alias exceeds 64 characters"
            }
        );
        let ok = "a".repeat(MAX_ALIAS_LEN);
        assert!(DeviceSelector::new(Some(ok), None).is_ok());
    }

    #[test]
    fn evidence_detection_pure() {
        let mut uuids = HashSet::new();
        let mut md = HashMap::new();
        assert!(!evidence_ok(&uuids, &md));
        uuids.insert(Uuid::parse_str("306b0001-b081-4037-83dc-e59fcc3cdfd0").unwrap());
        assert!(evidence_ok(&uuids, &md));
        uuids.clear();
        md.insert(spec::VICTRON_MANUFACTURER_ID, vec![0x10, 0x01]);
        assert!(evidence_ok(&uuids, &md));
    }

    #[test]
    fn describe_never_contains_raw_mac() {
        let sel = DeviceSelector {
            alias: None,
            address: Some(addr([0xaa; 6])),
        };
        assert_eq!(sel.describe(), "<configured address>");
    }
}
