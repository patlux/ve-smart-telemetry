//! Documented conservative physical validation ranges.
//!
//! Every named range is deliberately **wider than any real Victron product in
//! the target family** so that legitimate device behavior is never hidden
//! (e.g. a 48 V system charging above 58 V, negative battery current while
//! discharging), while still rejecting values that are physically impossible
//! or the result of decoding/sentinel errors (e.g. a raw `u32` overflow
//! decoded as an absurd power, or a positive RSSI).
//!
//! Ranges are inclusive on both ends.

/// An inclusive closed interval `[min, max]` for `f64` values.
///
/// `contains` also rejects non-finite values, so range checks and finiteness
/// checks can be combined.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    min: f64,
    max: f64,
}

impl Range {
    /// Build a closed interval.
    ///
    /// # Panics
    ///
    /// Panics if `min > max` or either bound is non-finite. Because this is a
    /// `const fn`, invalid named ranges also fail at compile time.
    pub const fn new(min: f64, max: f64) -> Self {
        assert!(min.is_finite() && max.is_finite() && min <= max);
        Self { min, max }
    }

    /// Inclusive lower bound.
    pub const fn min(self) -> f64 {
        self.min
    }

    /// Inclusive upper bound.
    pub const fn max(self) -> f64 {
        self.max
    }

    /// Whether `value` is finite and within `[min, max]` (inclusive).
    pub const fn contains(self, value: f64) -> bool {
        value.is_finite() && value >= self.min && value <= self.max
    }
}

/// PV voltage in volts, `[0.0, 250.0]`.
///
/// Rationale: the largest Victron SmartSolar PV input accepts up to 250 V
/// nominal. 0 V is a valid reading with no panel / no sun. Negative PV
/// voltage is impossible (the panel is a source; the charger blocks reverse
/// current), and values above 250 V indicate a decoding/sentinel error.
pub const PV_VOLTAGE_V: Range = Range::new(0.0, 250.0);

/// PV current in amperes, `[0.0, 100.0]`.
///
/// Rationale: the largest SmartSolar charge current rating is 100 A. 0 A is
/// a valid reading at night. Negative PV current is impossible for a panel
/// input; values above 100 A indicate a decoding/sentinel error.
pub const PV_CURRENT_A: Range = Range::new(0.0, 100.0);

/// PV power in watts, `[0.0, 30_000.0]`.
///
/// Rationale: the theoretical absolute maximum PV input of the largest
/// product is `250 V × 100 A = 25 000 W`; 30 000 W adds margin while still
/// rejecting decoded garbage (a `u32`-scaled register can encode values far
/// beyond this).
pub const PV_POWER_W: Range = Range::new(0.0, 30_000.0);

/// Battery voltage in volts, `[0.0, 70.0]`.
///
/// Rationale: Victron SmartSolar products support 12/24/48 V systems. 48 V
/// absorption reaches ~58.4 V and a 16-cell LiFePO4 bank can exceed 67 V
/// transiently; 70 V covers every supported system with margin. 0 V is a
/// valid reading for an empty/disconnected battery. Values above 70 V are
/// physically impossible for these products.
pub const BATTERY_VOLTAGE_V: Range = Range::new(0.0, 70.0);

/// Battery current in amperes, `[-200.0, 200.0]`.
///
/// Rationale: the largest SmartSolar charge current is 100 A, and the load
/// output can additionally discharge the battery, so the magnitude bound of
/// 200 A covers charge plus load-discharge for the whole family. Sign
/// convention: positive = charging (into the battery), negative =
/// discharging.
pub const BATTERY_CURRENT_A: Range = Range::new(-200.0, 200.0);

/// Load output current in amperes, `[0.0, 100.0]`.
///
/// Rationale: the largest load-output rating in the family is 100 A; typical
/// units are 15–20 A. 0 A means the load is off. The load output draws from
/// the device, so negative current is not meaningful.
pub const LOAD_CURRENT_A: Range = Range::new(0.0, 100.0);

/// Load output power in watts, `[0.0, 6_000.0]`.
///
/// Rationale: theoretical maximum load power is `100 A × 48 V ≈ 4 800 W`;
/// 6 000 W covers that with margin while still rejecting the raw `u16`
/// range of a register that can otherwise decode up to 65 535 W.
pub const LOAD_POWER_W: Range = Range::new(0.0, 6_000.0);

/// Native lifetime yield counter in kWh, `[0.0, 10_000_000.0]`.
///
/// Rationale: a counter must never be negative. The absolute theoretical
/// maximum for the largest product run at continuous full output for 40
/// years is `25 kW × 24 h × 365 d × 40 y ≈ 8.76e6 kWh`, so 10 GWh covers
/// even that extreme with margin. Real deployments are orders of magnitude
/// smaller. Monotonicity itself is not checked here — it is a
/// storage/service-layer concern across samples.
pub const YIELD_TOTAL_KWH: Range = Range::new(0.0, 10_000_000.0);

/// Yield since local midnight in kWh, `[0.0, 2_000.0]`.
///
/// Rationale: a day counter must never be negative. The theoretical maximum
/// for the largest product is `25 kW × 24 h = 600 kWh/day`; 2 000 kWh covers
/// that with margin while rejecting unit/scale errors that would otherwise
/// decode a `u16`/`u32` register into absurd per-day values.
pub const YIELD_TODAY_KWH: Range = Range::new(0.0, 2_000.0);

/// BLE RSSI in dBm, `[-128.0, 0.0]`.
///
/// Rationale: -128 dBm is the physical noise floor of a 1 MHz BLE channel;
/// 0 dBm is the highest meaningful received power. BlueZ normally reports
/// -100…-30 dBm for real links. Positive RSSI indicates a broken stack value
/// and is rejected. 0 dBm is kept valid because some stacks report it when
/// the measurement is unavailable.
pub const BLE_RSSI_DBM: Range = Range::new(-128.0, 0.0);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_named_ranges_are_valid() {
        for range in [
            PV_VOLTAGE_V,
            PV_CURRENT_A,
            PV_POWER_W,
            BATTERY_VOLTAGE_V,
            BATTERY_CURRENT_A,
            LOAD_CURRENT_A,
            LOAD_POWER_W,
            YIELD_TOTAL_KWH,
            YIELD_TODAY_KWH,
            BLE_RSSI_DBM,
        ] {
            assert!(range.min().is_finite(), "{:?}", range);
            assert!(range.max().is_finite(), "{:?}", range);
            assert!(range.min() <= range.max(), "{:?}", range);
        }
    }

    #[test]
    fn contains_is_inclusive() {
        assert!(PV_VOLTAGE_V.contains(0.0));
        assert!(PV_VOLTAGE_V.contains(250.0));
        assert!(BLE_RSSI_DBM.contains(-128.0));
        assert!(BLE_RSSI_DBM.contains(0.0));
        assert!(BATTERY_CURRENT_A.contains(-200.0));
        assert!(BATTERY_CURRENT_A.contains(200.0));
    }

    #[test]
    fn contains_rejects_non_finite() {
        assert!(!PV_VOLTAGE_V.contains(f64::NAN));
        assert!(!PV_VOLTAGE_V.contains(f64::INFINITY));
        assert!(!PV_VOLTAGE_V.contains(f64::NEG_INFINITY));
    }

    #[test]
    fn representative_rejections() {
        assert!(!PV_VOLTAGE_V.contains(-0.1));
        assert!(!PV_VOLTAGE_V.contains(250.01));
        assert!(!PV_CURRENT_A.contains(-0.1));
        assert!(!PV_CURRENT_A.contains(100.1));
        assert!(!PV_POWER_W.contains(-0.1));
        assert!(!PV_POWER_W.contains(30_001.0));
        assert!(!BATTERY_VOLTAGE_V.contains(-0.1));
        assert!(!BATTERY_VOLTAGE_V.contains(70.01));
        assert!(!BATTERY_CURRENT_A.contains(-200.1));
        assert!(!BATTERY_CURRENT_A.contains(200.1));
        assert!(!LOAD_CURRENT_A.contains(-0.1));
        assert!(!LOAD_CURRENT_A.contains(100.1));
        assert!(!LOAD_POWER_W.contains(-0.1));
        assert!(!LOAD_POWER_W.contains(6_000.1));
        assert!(!YIELD_TOTAL_KWH.contains(-0.1));
        assert!(!YIELD_TOTAL_KWH.contains(10_000_001.0));
        assert!(!YIELD_TODAY_KWH.contains(-0.1));
        assert!(!YIELD_TODAY_KWH.contains(2_000.1));
        assert!(!BLE_RSSI_DBM.contains(-128.1));
        assert!(!BLE_RSSI_DBM.contains(0.1));
    }

    #[test]
    fn range_new_rejects_invalid_bounds() {
        assert!(std::panic::catch_unwind(|| Range::new(5.0, 1.0)).is_err());
        assert!(std::panic::catch_unwind(|| Range::new(f64::NAN, 1.0)).is_err());
        assert!(std::panic::catch_unwind(|| Range::new(1.0, f64::INFINITY)).is_err());
    }

    #[test]
    fn range_is_copy_and_partial_eq() {
        assert_eq!(PV_VOLTAGE_V, Range::new(0.0, 250.0));
        let copied = PV_VOLTAGE_V;
        assert_eq!(copied.min(), 0.0);
        assert_eq!(copied.max(), 250.0);
    }
}
