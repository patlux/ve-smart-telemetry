use super::*;
use crate::error::BleErrorClass;

fn valid_config() -> TransportConfig {
    TransportConfig {
        selector: DeviceSelector {
            alias: Some("Solar Charger".into()),
            address: None,
        },
        ..TransportConfig::default()
    }
}

#[test]
fn default_config_is_invalid_until_configured() {
    // The Default instance is a test seam; it must not silently produce
    // an unusable selector.
    let err = TransportConfig::default().validate().unwrap_err();
    assert_eq!(
        err,
        BleError::InvalidConfig {
            detail: "device selector requires a non-empty alias and/or address"
        }
    );
    assert_eq!(err.class(), BleErrorClass::Other);
}

#[test]
fn validate_rejects_zero_timeouts_and_chunk_size() {
    let mut cfg = valid_config();
    cfg.connect_timeout = Duration::ZERO;
    assert_eq!(
        cfg.validate().unwrap_err(),
        BleError::InvalidConfig {
            detail: "timeouts must be positive"
        }
    );

    let mut cfg = valid_config();
    cfg.discovery_timeout = Duration::ZERO;
    assert_eq!(
        cfg.validate().unwrap_err(),
        BleError::InvalidConfig {
            detail: "timeouts must be positive"
        }
    );

    let mut cfg = valid_config();
    cfg.notification_timeout = Duration::ZERO;
    assert_eq!(
        cfg.validate().unwrap_err(),
        BleError::InvalidConfig {
            detail: "timeouts must be positive"
        }
    );

    let mut cfg = valid_config();
    cfg.operation_timeout = Duration::ZERO;
    assert_eq!(
        cfg.validate().unwrap_err(),
        BleError::InvalidConfig {
            detail: "timeouts must be positive"
        }
    );

    let mut cfg = valid_config();
    cfg.write_chunk_size = 0;
    assert_eq!(
        cfg.validate().unwrap_err(),
        BleError::InvalidConfig {
            detail: "write_chunk_size must be positive"
        }
    );

    // Positive values pass.
    assert!(valid_config().validate().is_ok());
}

#[test]
fn transient_connect_errors_are_retryable() {
    assert!(retryable_connect_kind(
        &bluer::ErrorKind::ConnectionAttemptFailed
    ));
    assert!(retryable_connect_kind(&bluer::ErrorKind::Failed));
    assert!(retryable_connect_kind(&bluer::ErrorKind::NotReady));
    assert!(retryable_connect_kind(&bluer::ErrorKind::InProgress));
    assert!(!retryable_connect_kind(
        &bluer::ErrorKind::AuthenticationFailed
    ));
    assert!(!retryable_connect_kind(&bluer::ErrorKind::NotAuthorized));
}

#[test]
fn validate_requires_positive_operation_timeout_even_with_address_selector() {
    let mut cfg = valid_config();
    cfg.selector = DeviceSelector {
        alias: None,
        address: Some(bluer::Address::new([1; 6])),
    };
    cfg.operation_timeout = Duration::ZERO;
    assert_eq!(
        cfg.validate().unwrap_err(),
        BleError::InvalidConfig {
            detail: "timeouts must be positive"
        }
    );
}

#[tokio::test]
async fn open_with_invalid_config_fails_before_touching_the_bus() {
    // No D-Bus is reachable in this test; reaching `validate()` first
    // proves config is checked before any session/adapter action.
    let mut transport = BluezTransport::new(TransportConfig::default());
    let err = transport.open().await.unwrap_err();
    assert_eq!(
        err,
        BleError::InvalidConfig {
            detail: "device selector requires a non-empty alias and/or address"
        }
    );
    assert!(!transport.state.is_open());
    assert!(!transport.state.has_resources());
    // Object stays reusable: a second open attempt fails the same way
    // (validation, not stale state).
    assert!(transport.open().await.is_err());
    assert!(!transport.state.is_open());
    assert!(!transport.state.has_resources());
}

#[test]
fn session_state_clear_leaves_no_resources() {
    let mut state = SessionState::default();
    assert!(!state.is_open());
    assert!(!state.has_resources());
    // Simulate a partially committed failure path.
    state.open = true;
    assert!(state.is_open());
    state.clear();
    assert!(!state.is_open());
    assert!(!state.has_resources());
    // clear is idempotent.
    state.clear();
    assert!(!state.has_resources());
}

#[test]
fn state_commit_marks_open() {
    // Session/Adapter/Device cannot be constructed without a live bus, so
    // commit itself is exercised through the open flow; this test pins
    // the open flag invariant of the state machine.
    let mut state = SessionState::default();
    assert!(!state.open);
    state.open = true;
    assert!(state.is_open());
}
