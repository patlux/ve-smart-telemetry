//! Validated TOML configuration for the collector daemon.
//!
//! No PIN, PUK, bond key or secret ever lives here: pairing material belongs
//! to BlueZ. A `[device] pin` key is explicitly rejected by
//! `deny_unknown_fields` (see tests).
//!
//! `victoria_metrics.url` accepts **only** the plaintext `http://` URL shape
//! supported by `victron-metrics` (no TLS on ARMv6): `https://`, userinfo,
//! whitespace/control injection, query/fragment and invalid ports/paths are
//! rejected at parse time.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use victron_service::{ConfigError as ServiceConfigError, ServiceConfig};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub device: DeviceSection,
    pub poll: PollSection,
    pub victoria_metrics: VictoriaMetricsSection,
    pub storage: StorageSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSection {
    /// Stable local label used for metrics and logs.
    pub name: String,
    /// BlueZ device alias used during discovery (bonded identity).
    pub bluez_alias: String,
    /// VE.Smart instance to subscribe to (>= 1; 0 is the keep-alive
    /// pseudo-instance and is not supported).
    pub instance: u16,
    /// BlueZ adapter name, normally `hci0`.
    pub adapter: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PollSection {
    pub active_interval_seconds: u64,
    pub idle_interval_seconds: u64,
    pub response_timeout_seconds: u64,
    pub phase_timeout_seconds: u64,
    pub maximum_energy_gap_seconds: u64,
    pub spool_claim_ttl_seconds: u64,
    pub spool_max_attempts: u32,
    pub backoff_base_seconds: u64,
    pub backoff_factor: u32,
    pub backoff_cap_seconds: u64,
    /// Confirmed PV power (watts) at or above this value counts as solar
    /// activity and selects the active poll cadence.
    #[serde(default = "default_solar_threshold")]
    pub solar_active_threshold_watts: f64,
}

fn default_solar_threshold() -> f64 {
    5.0
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VictoriaMetricsSection {
    pub url: String,
    pub request_timeout_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageSection {
    pub path: PathBuf,
    pub maximum_spool_batches: u64,
    /// Retained by the storage adapter for spool pruning.
    #[allow(dead_code)]
    pub maximum_spool_age_days: u64,
}

impl Config {
    /// Parse and validate a TOML document.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        let config: Config = toml::from_str(text).map_err(ConfigError::Toml)?;
        config.validate()?;
        Ok(config)
    }

    /// Load and validate from a file.
    pub fn load(path: &std::path::Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Read(path.display().to_string(), e.to_string()))?;
        Self::from_toml(&text)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.device.name.trim().is_empty() {
            return Err(ConfigError::EmptyDeviceName);
        }
        if self.device.instance == 0 {
            return Err(ConfigError::InstanceZero);
        }
        if self.victoria_metrics.request_timeout_seconds == 0 {
            return Err(ConfigError::TimeoutZero(
                "victoria_metrics.request_timeout_seconds",
            ));
        }
        validate_metrics_url(&self.victoria_metrics.url)?;
        if !self.poll.solar_active_threshold_watts.is_finite()
            || self.poll.solar_active_threshold_watts < 0.0
        {
            return Err(ConfigError::SolarThresholdInvalid(
                self.poll.solar_active_threshold_watts,
            ));
        }
        if self.storage.maximum_spool_batches == 0 {
            return Err(ConfigError::SpoolBatchLimitZero);
        }
        // Range checks for everything the service consumes.
        self.service_config().map_err(ConfigError::Service)?;
        Ok(())
    }

    /// Convert to the service config. Assumes `validate()` already ran, but
    /// maps remaining problems to errors rather than panicking.
    pub fn service_config(&self) -> Result<ServiceConfig, ServiceConfigError> {
        let cfg = ServiceConfig {
            device_name: self.device.name.clone(),
            instance: self.device.instance,
            active_interval: Duration::from_secs(self.poll.active_interval_seconds),
            idle_interval: Duration::from_secs(self.poll.idle_interval_seconds),
            solar_active_threshold_watts: self.poll.solar_active_threshold_watts,
            response_timeout: Duration::from_secs(self.poll.response_timeout_seconds),
            phase_timeout: Duration::from_secs(self.poll.phase_timeout_seconds),
            maximum_energy_gap: Duration::from_secs(self.poll.maximum_energy_gap_seconds),
            spool_claim_ttl: Duration::from_secs(self.poll.spool_claim_ttl_seconds),
            spool_max_attempts: self.poll.spool_max_attempts,
            backoff_base: Duration::from_secs(self.poll.backoff_base_seconds),
            backoff_factor: self.poll.backoff_factor,
            backoff_cap: Duration::from_secs(self.poll.backoff_cap_seconds),
        };
        cfg.validate()?;
        Ok(cfg)
    }
}

/// Validates a VictoriaMetrics import URL against the exact shape the
/// `victron-metrics` client supports: plaintext `http://`, IPv4/DNS host
/// without userinfo or brackets, valid non-zero port, absolute safe path, no
/// query/fragment, no whitespace or control characters anywhere.
fn validate_metrics_url(url: &str) -> Result<(), ConfigError> {
    let rest = url.strip_prefix("http://").ok_or(ConfigError::BadUrl {
        reason: "only plaintext http:// is supported (no TLS on ARMv6)",
    })?;
    if rest.contains('?') || rest.contains('#') {
        return Err(ConfigError::BadUrl {
            reason: "query strings and fragments are never sent",
        });
    }
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/api/v1/import/prometheus"),
    };
    if authority.is_empty() {
        return Err(ConfigError::BadUrl {
            reason: "missing host",
        });
    }
    if authority.contains('@') {
        return Err(ConfigError::BadUrl {
            reason: "userinfo is not supported",
        });
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => {
            if h.is_empty() || h.contains(':') || h.starts_with('[') {
                return Err(ConfigError::BadUrl {
                    reason: "unsupported host (IPv6/brackets not supported)",
                });
            }
            let port: u16 = p
                .parse()
                .ok()
                .filter(|&p| p != 0)
                .ok_or(ConfigError::BadUrl {
                    reason: "invalid or zero port",
                })?;
            (h, port)
        }
        None => (authority, 80),
    };
    if !is_valid_host(host) {
        return Err(ConfigError::BadUrl {
            reason: "host must be a plain IPv4 address or DNS name without whitespace or control characters",
        });
    }
    if !is_valid_path(path) {
        return Err(ConfigError::BadUrl {
            reason: "path must start with '/' and contain only safe request-line bytes",
        });
    }
    let _ = port;
    Ok(())
}

/// Charset check for the host part: plain IPv4 or DNS names only.
fn is_valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
}

/// Charset check for the request-target path: absolute (`/`-prefixed) and
/// free of any byte that could corrupt the request line.
fn is_valid_path(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    path.bytes()
        .all(|b| (0x21..=0x7e).contains(&b) && !b" \"<>\\^`{|}#?".contains(&b))
}

/// Configuration error with a fixed exit-code mapping (2 = usage/config).
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config {0}: {1}")]
    Read(String, String),
    #[error("invalid TOML: {0}")]
    Toml(toml::de::Error),
    #[error("device name must not be empty")]
    EmptyDeviceName,
    #[error("instance 0 is the keep-alive pseudo-instance and is not supported")]
    InstanceZero,
    #[error("{0} must be greater than zero")]
    TimeoutZero(&'static str),
    #[error("victoria_metrics.url is not a valid plaintext http:// URL: {reason}")]
    BadUrl { reason: &'static str },
    #[error("solar_active_threshold_watts must be finite and >= 0, got {0}")]
    SolarThresholdInvalid(f64),
    #[error("storage.maximum_spool_batches must be greater than zero")]
    SpoolBatchLimitZero,
    #[error("invalid service configuration: {0}")]
    Service(ServiceConfigError),
    #[error("invalid VictoriaMetrics delivery configuration: {0}")]
    Delivery(String),
    #[error("invalid device identity: {0}")]
    DeviceIdentity(String),
    #[error("invalid Bluetooth configuration: {0}")]
    Bluetooth(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
[device]
name = "solar-charger"
bluez_alias = "Solar Charger"
instance = 3
adapter = "hci0"

[poll]
active_interval_seconds = 15
idle_interval_seconds = 60
response_timeout_seconds = 8
phase_timeout_seconds = 12
maximum_energy_gap_seconds = 300
spool_claim_ttl_seconds = 120
spool_max_attempts = 5
backoff_base_seconds = 5
backoff_factor = 2
backoff_cap_seconds = 300

[victoria_metrics]
url = "http://127.0.0.1:8429/api/v1/import/prometheus"
request_timeout_seconds = 10

[storage]
path = "/var/lib/victron-collector/state.sqlite3"
maximum_spool_batches = 10000
maximum_spool_age_days = 7
"#;

    #[test]
    fn parses_valid_config() {
        let c = Config::from_toml(VALID).expect("valid config parses");
        assert_eq!(c.device.name, "solar-charger");
        assert_eq!(c.poll.active_interval_seconds, 15);
        assert_eq!(c.poll.solar_active_threshold_watts, 5.0);
        let s = c.service_config().expect("service config valid");
        s.validate().expect("validated");
    }

    #[test]
    fn rejects_unknown_fields() {
        // A PIN key must not silently slip through: no secrets in config.
        let text = VALID.replace("adapter = \"hci0\"", "adapter = \"hci0\"\npin = \"1234\"");
        assert!(matches!(
            Config::from_toml(&text),
            Err(ConfigError::Toml(_))
        ));
    }

    #[test]
    fn rejects_missing_required_sections() {
        assert!(matches!(
            Config::from_toml("[device]\nname = \"x\"\n"),
            Err(ConfigError::Toml(_))
        ));
    }

    #[test]
    fn rejects_instance_zero_and_bad_url() {
        let text = VALID.replace("instance = 3", "instance = 0");
        assert!(matches!(
            Config::from_toml(&text),
            Err(ConfigError::InstanceZero)
        ));
        let text = VALID.replace(
            "url = \"http://127.0.0.1:8429/api/v1/import/prometheus\"",
            "url = \"127.0.0.1:8429\"",
        );
        assert!(matches!(
            Config::from_toml(&text),
            Err(ConfigError::BadUrl { .. })
        ));
    }

    #[test]
    fn rejects_https_and_other_schemes() {
        for bad in [
            "https://127.0.0.1:8429/api/v1/import/prometheus",
            "ftp://127.0.0.1:8429/",
            "127.0.0.1:8429",
        ] {
            let text = VALID.replace(
                "url = \"http://127.0.0.1:8429/api/v1/import/prometheus\"",
                &format!("url = \"{bad}\""),
            );
            assert!(
                matches!(Config::from_toml(&text), Err(ConfigError::BadUrl { .. })),
                "{bad} must be rejected"
            );
        }
    }

    #[test]
    fn rejects_userinfo_query_fragment_and_injection() {
        for bad in [
            "http://user:pass@127.0.0.1:8429/",
            "http://127.0.0.1:8429/api?x=1",
            "http://127.0.0.1:8429/api#frag",
            // TOML-escaped control characters: the parsed URL value carries a
            // real newline/tab/CR, which the URL validator must reject.
            "http://127.0.0.1:8429/api/v1/import/prometheus\\nX-Evil: 1",
            "http://127.0.0.1:8429/api/v1/import/prometheus\\t",
            "http://127.0.0.1:8429/api/v1/import/prometheus\\r",
            "http://127.0.0.1:8429/api/v1/import/prometheus ",
            "http://[::1]:8429/",
            "http://127.0.0.1:0/",
            "http://127.0.0.1:99999/",
            "http://127.0.0.1:8429/api/v1/import/prometheus<",
        ] {
            let text = VALID.replace(
                "url = \"http://127.0.0.1:8429/api/v1/import/prometheus\"",
                &format!("url = \"{bad}\""),
            );
            assert!(
                matches!(Config::from_toml(&text), Err(ConfigError::BadUrl { .. })),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn literal_control_bytes_are_rejected_at_the_toml_layer() {
        // A literal CR/LF inside a basic TOML string is a TOML parse error:
        // the injection cannot even reach the URL validator.
        let text = VALID.replace(
            "url = \"http://127.0.0.1:8429/api/v1/import/prometheus\"",
            "url = \"http://127.0.0.1:8429/api/v1/import/prometheus\r\nX-Evil: 1\"",
        );
        assert!(matches!(
            Config::from_toml(&text),
            Err(ConfigError::Toml(_))
        ));
    }

    #[test]
    fn accepts_plain_host_and_default_path() {
        let text = VALID.replace(
            "url = \"http://127.0.0.1:8429/api/v1/import/prometheus\"",
            "url = \"http://victoria.internal:8429\"",
        );
        let c = Config::from_toml(&text).expect("plain host accepted");
        assert_eq!(c.victoria_metrics.url, "http://victoria.internal:8429");
    }

    #[test]
    fn rejects_bad_solar_threshold() {
        let text = VALID.replace(
            "backoff_cap_seconds = 300",
            "backoff_cap_seconds = 300\nsolar_active_threshold_watts = -1.0",
        );
        assert!(matches!(
            Config::from_toml(&text),
            Err(ConfigError::SolarThresholdInvalid(_))
        ));
    }

    #[test]
    fn rejects_service_level_bounds() {
        let text = VALID.replace("backoff_factor = 2", "backoff_factor = 0");
        assert!(matches!(
            Config::from_toml(&text),
            Err(ConfigError::Service(ServiceConfigError::BackoffFactorZero))
        ));
    }

    #[test]
    fn explicit_solar_threshold_is_used() {
        let text = VALID.replace(
            "backoff_cap_seconds = 300",
            "backoff_cap_seconds = 300\nsolar_active_threshold_watts = 12.5",
        );
        let c = Config::from_toml(&text).expect("threshold accepted");
        assert_eq!(c.poll.solar_active_threshold_watts, 12.5);
    }
}
