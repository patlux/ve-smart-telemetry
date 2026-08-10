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
