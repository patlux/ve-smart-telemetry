//! Pinned metric contract: names, label keys, and the bounded charger-state
//! vocabulary.
//!
//! These constants are the single source of truth for the wire names agreed in
//! `analysis/grafana-integration-plan.md`. Names are lowercase
//! `snake_case`; counters carry the Prometheus `_total` suffix and are created
//! with [`crate::MetricBatchBuilder::counter`], which enforces the suffix.

/// Metric name: PV voltage in volts.
pub const PV_VOLTAGE_VOLTS: &str = "victron_pv_voltage_volts";
/// Metric name: PV current in amperes.
pub const PV_CURRENT_AMPERES: &str = "victron_pv_current_amperes";
/// Metric name: PV power in watts.
pub const PV_POWER_WATTS: &str = "victron_pv_power_watts";
/// Metric name: battery voltage in volts.
pub const BATTERY_VOLTAGE_VOLTS: &str = "victron_battery_voltage_volts";
/// Metric name: battery current in amperes.
pub const BATTERY_CURRENT_AMPERES: &str = "victron_battery_current_amperes";
/// Metric name: load power in watts.
pub const LOAD_POWER_WATTS: &str = "victron_load_power_watts";
/// Metric name: load current in amperes.
pub const LOAD_CURRENT_AMPERES: &str = "victron_load_current_amperes";
/// Metric name: cumulative (lifetime) yield in kWh.
///
/// This is a cumulative value meant to be read as a monotonic series on the
/// query side (`delta()`/`increase()` in PromQL). The name deliberately
/// lacks the `_total` suffix (kept from the established contract), so this
/// crate constructs it as a [`crate::MetricKind::Gauge`] — the text format
/// carries no TYPE metadata, and `MetricKind::Counter` is only used for names
/// that end in `_total`. Prefer the native Victron lifetime counter.
pub const YIELD_TOTAL_KWH: &str = "victron_yield_total_kwh";
/// Metric name: today's yield in kWh. Diagnostic only; resets daily and must
/// not be used as the canonical Grafana cumulative series.
pub const YIELD_TODAY_KWH: &str = "victron_yield_today_kwh";
/// Metric name: charger state, encoded as a bounded `state` label with value 1.
pub const CHARGER_STATE: &str = "victron_charger_state";
/// Metric name: BLE link up (1) or down (0).
pub const BLE_UP: &str = "victron_ble_up";
/// Metric name: BLE RSSI in dBm.
pub const BLE_RSSI_DBM: &str = "victron_ble_rssi_dbm";
/// Metric name: Unix time (seconds) of the last successful acquisition.
pub const LAST_SUCCESS_UNIXTIME: &str = "victron_last_success_unixtime";
/// Metric name: age of the last sample in seconds.
pub const SAMPLE_AGE_SECONDS: &str = "victron_sample_age_seconds";
/// Counter: cumulative BLE connect failures.
pub const BLE_CONNECT_FAILURES_TOTAL: &str = "victron_ble_connect_failures_total";
/// Counter: cumulative protocol decode errors.
pub const PROTOCOL_ERRORS_TOTAL: &str = "victron_protocol_errors_total";
/// Counter: cumulative dropped samples. Reasons are logged, never labels
/// (keeps cardinality bounded).
pub const SAMPLES_DROPPED_TOTAL: &str = "victron_samples_dropped_total";
/// Counter: cumulative seconds skipped by local energy integration (gaps).
pub const ENERGY_INTEGRATION_GAP_SECONDS_TOTAL: &str =
    "victron_energy_integration_gap_seconds_total";
/// Gauge: current spool depth (batches waiting for delivery).
pub const SPOOL_BATCHES: &str = "victron_spool_batches";
/// Gauge: age of the oldest spooled batch in seconds (omitted when the spool
/// is empty).
pub const SPOOL_OLDEST_AGE_SECONDS: &str = "victron_spool_oldest_age_seconds";

/// Label key for the configured device identity (always present).
pub const DEVICE_LABEL: &str = "device";
/// Label key for bounded state (e.g. charger state).
pub const STATE_LABEL: &str = "state";

/// Bounded vocabulary for the `state` label of state metrics.
///
/// Values are lowercase ASCII identifiers; [`crate::MetricBatchBuilder::state`]
/// enforces that any emitted value matches `[a-z0-9_]`, is non-empty, and is at
/// most 32 bytes, so cardinality is bounded by construction. The list below is
/// the advisory charger-state mapping (Victron VE.Smart charger states); new
/// bounded values may be added as long as they satisfy the same charset rule.
pub mod states {
    /// Charger off.
    pub const OFF: &str = "off";
    /// Charger fault.
    pub const FAULT: &str = "fault";
    /// Bulk charging.
    pub const BULK: &str = "bulk";
    /// Absorption charging.
    pub const ABSORPTION: &str = "absorption";
    /// Float charging.
    pub const FLOAT: &str = "float";
    /// Storage.
    pub const STORAGE: &str = "storage";
    /// Equalize / recondition.
    pub const EQUALIZE: &str = "equalize";
    /// Starting up (domain code 245).
    pub const STARTING_UP: &str = "starting_up";
    /// Auto equalize / recondition (domain code 247).
    pub const AUTO_RECONDITION: &str = "auto_recondition";
    /// External control (domain code 252).
    pub const EXTERNAL_CONTROL: &str = "external_control";
    /// Passthru.
    pub const PASSTHRU: &str = "passthru";
    /// State unknown / not mapped.
    pub const UNKNOWN: &str = "unknown";

    /// All advisory charger-state values.
    pub const ALL: &[&str] = &[
        OFF,
        FAULT,
        BULK,
        ABSORPTION,
        FLOAT,
        STORAGE,
        EQUALIZE,
        STARTING_UP,
        AUTO_RECONDITION,
        EXTERNAL_CONTROL,
        PASSTHRU,
        UNKNOWN,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_names_are_valid_metric_names() {
        for name in [
            PV_VOLTAGE_VOLTS,
            PV_CURRENT_AMPERES,
            PV_POWER_WATTS,
            BATTERY_VOLTAGE_VOLTS,
            BATTERY_CURRENT_AMPERES,
            LOAD_POWER_WATTS,
            LOAD_CURRENT_AMPERES,
            YIELD_TOTAL_KWH,
            YIELD_TODAY_KWH,
            CHARGER_STATE,
            BLE_UP,
            BLE_RSSI_DBM,
            LAST_SUCCESS_UNIXTIME,
            SAMPLE_AGE_SECONDS,
            BLE_CONNECT_FAILURES_TOTAL,
            PROTOCOL_ERRORS_TOTAL,
            SAMPLES_DROPPED_TOTAL,
            ENERGY_INTEGRATION_GAP_SECONDS_TOTAL,
            SPOOL_BATCHES,
            SPOOL_OLDEST_AGE_SECONDS,
        ] {
            assert!(
                crate::MetricName::new(name).is_ok(),
                "contract name {name:?} must be a valid metric name"
            );
        }
    }

    #[test]
    fn counters_carry_total_suffix() {
        for name in [
            BLE_CONNECT_FAILURES_TOTAL,
            PROTOCOL_ERRORS_TOTAL,
            SAMPLES_DROPPED_TOTAL,
            ENERGY_INTEGRATION_GAP_SECONDS_TOTAL,
        ] {
            assert!(name.ends_with("_total"), "{name} must end in _total");
        }
    }

    #[test]
    fn state_vocabulary_is_bounded_and_lowercase() {
        for s in states::ALL {
            assert!(crate::MetricBatchBuilder::is_valid_state_value(s));
        }
    }
}
