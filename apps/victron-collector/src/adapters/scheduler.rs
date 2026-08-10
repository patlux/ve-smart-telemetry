//! Interval policy: solar-activity switching (fully wired, no sibling
//! dependency).
//!
//! The service owns [`SolarActivityPolicy`]: the active poll cadence applies
//! while the last successfully committed sample reports confirmed PV power at
//! or above the configured threshold; otherwise the idle cadence applies. The
//! first cycle uses the active cadence. The old UTC hour-window stopgap is
//! gone.

pub use victron_service::SolarActivityPolicy;
