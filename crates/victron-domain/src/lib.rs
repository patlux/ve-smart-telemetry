//! # victron-domain
//!
//! Hardware- and wire-independent domain model for the Victron BLE collector.
//!
//! This crate is the canonical place for business values that must be
//! identical no matter which wire protocol, transport, or metrics backend is
//! used. It deliberately knows nothing about VE.Smart VREG IDs, GATT UUIDs,
//! Prometheus metric names, SQLite schemas, HTTP, Tokio, or BlueZ — those
//! concerns live in sibling crates of the planned workspace
//! (`victron-protocol`, `victron-bluez`, `victron-storage`,
//! `victron-metrics`, `victron-service`).
//!
//! ## Core concepts
//!
//! - [`DeviceId`] — validated stable local device name (label-safe charset,
//!   bounded length).
//! - [`Sample`] — one observation at a point in time: a [`DeviceId`], a
//!   [`std::time::SystemTime`] timestamp, named optional [`Measurement`]
//!   fields (PV/battery/load, yields, BLE RSSI), and optional bounded state
//!   enums ([`ChargerState`], [`LoadState`], [`ConnectionHealth`]).
//! - [`Measurement`] — an `f64` value plus a [`Quality`] describing how much
//!   the value can be trusted: `ConfirmedNative`, `Candidate`, `Derived`, or
//!   `LocallyIntegrated`.
//! - [`bounds`] — documented conservative physical ranges used to reject
//!   non-finite and physically impossible values at construction time
//!   without hiding legitimate device behavior.
//!
//! ## Stable seam for other crates
//!
//! A [`Sample`] exposes `device()`, `observed_at()`, and named accessors
//! returning `Option<&Measurement>`; every [`Measurement`] exposes `value()`
//! and `quality()`. State enums are bounded and preserve unknown numeric
//! codes safely as `Unknown(u8)`. Derived PV current and load power are only
//! produced when their inputs are valid.
//!
//! ## Example
//!
//! ```
//! use std::time::Duration;
//! use victron_domain::{ChargerState, DeviceId, Quality, Sample};
//!
//! let device = DeviceId::new("solar-charger")?;
//! let sample = Sample::builder_now(device)
//!     .pv_voltage_volts(34.2, Quality::ConfirmedNative)?
//!     .pv_power_watts(96.0, Quality::ConfirmedNative)?
//!     .battery_voltage_volts(12.6, Quality::Candidate)?
//!     .charger_state(ChargerState::Bulk)
//!     .build();
//!
//! assert!(sample.is_fresh(Duration::from_secs(300)));
//! assert_eq!(sample.measurement_count(), 3);
//! assert_eq!(
//!     sample.derived_pv_current().map(|m| (m.value(), m.quality())),
//!     Some((96.0 / 34.2, Quality::Derived))
//! );
//! # Ok::<(), victron_domain::DomainError>(())
//! ```

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![doc(html_root_url = "https://docs.rs/victron-domain")]

pub mod bounds;
pub mod builder;
pub mod derived;
pub mod device;
pub mod error;
pub mod measurement;
pub mod quality;
pub mod sample;
pub mod state;

pub use bounds::Range;
pub use builder::SampleBuilder;
pub use device::DeviceId;
pub use error::DomainError;
pub use measurement::{Measurement, SampleField};
pub use quality::Quality;
pub use sample::{Sample, DEFAULT_MAX_SAMPLE_AGE};
pub use state::{ChargerState, ConnectionHealth, LoadState};
