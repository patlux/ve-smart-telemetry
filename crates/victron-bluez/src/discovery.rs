//! Resolution of the configured bonded Victron device.
//!
//! Resolution uses the configured alias/address **plus** Victron
//! advertisement evidence (manufacturer id `0x02e1` with `0x10` byte, or a
//! VE.Smart service UUID). A bounded discovery scan refreshes advertisement
//! data before the transport gives up.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

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
            tracing::debug!(
                operation = "device-alias",
                error_class = ?err.class(),
                "skipping unrelated BLE candidate with unreadable alias"
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
    let started_at = Instant::now();
    let known = bounded("device-addresses", op_timeout, async {
        adapter.device_addresses().await.map_err(|e| from_bluer(&e))
    })
    .await?;
    tracing::debug!(
        operation = "device-resolution",
        known_devices = known.len(),
        require_evidence,
        "inspecting devices already known to BlueZ"
    );
    let mut candidates = Vec::new();
    for address in &known {
        if let Some(candidate) = inspect(adapter, address, selector, op_timeout).await? {
            candidates.push(candidate);
        }
    }
    if let Ok(device) = choose(&candidates, selector, require_evidence) {
        tracing::debug!(
            operation = "device-resolution",
            source = "known-devices",
            matched_candidates = candidates.len(),
            elapsed_ms = started_at.elapsed().as_millis() as u64,
            "resolved configured device"
        );
        return Ok(device);
    }

    tracing::debug!(
        operation = "discovery-scan",
        known_devices = known.len(),
        matched_candidates = candidates.len(),
        scan_timeout_ms = scan_timeout.as_millis() as u64,
        "configured device not resolved from known devices; starting bounded scan"
    );
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
                            tracing::trace!(
                                operation = "discovery-scan",
                                unique_devices_seen = seen.len(),
                                "observed BLE device during scan"
                            );
                            if let Some(candidate) = inspect(adapter, &address, selector, op_timeout).await? {
                                candidates.push(candidate);
                                if let Ok(device) = choose(&candidates, selector, require_evidence) {
                                    tracing::debug!(
                                        operation = "device-resolution",
                                        source = "discovery-event",
                                        unique_devices_seen = seen.len(),
                                        matched_candidates = candidates.len(),
                                        elapsed_ms = started_at.elapsed().as_millis() as u64,
                                        "resolved configured device"
                                    );
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
                tracing::debug!(
                    operation = "discovery-scan",
                    outcome = "timeout",
                    elapsed_ms = started_at.elapsed().as_millis() as u64,
                    unique_devices_seen = seen.len(),
                    matched_candidates = candidates.len(),
                    "bounded discovery scan ended"
                );
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
    let result = choose(&refreshed, selector, require_evidence);
    tracing::debug!(
        operation = "device-resolution",
        outcome = if result.is_ok() {
            "resolved"
        } else {
            "not-found"
        },
        refreshed_devices = refreshed_addresses.len(),
        matched_candidates = refreshed.len(),
        elapsed_ms = started_at.elapsed().as_millis() as u64,
        "finished device resolution after discovery refresh"
    );
    result
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
