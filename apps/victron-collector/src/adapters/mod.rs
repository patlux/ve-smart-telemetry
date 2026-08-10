//! Concrete port adapters.
//!
//! The sibling crates (`victron-bluez`, `victron-protocol`, `victron-domain`,
//! `victron-storage`, `victron-metrics`) are being built in parallel and are
//! not yet available in this lane. Every adapter here is an honest placeholder:
//! it implements the port trait and returns a precise `NotWired` error naming
//! the missing wiring instead of faking success. Wiring each adapter means
//! replacing the placeholder body with the sibling crate call and deleting
//! the `NotWired` marker — no port trait changes required.
//!
//! `clock.rs` and `scheduler.rs` are already fully concrete (no sibling
//! dependency needed).

pub mod clock;
pub mod delivery;
pub mod protocol;
pub mod scheduler;
pub mod storage;

pub use clock::SystemClock;
pub use scheduler::SolarActivityPolicy;
