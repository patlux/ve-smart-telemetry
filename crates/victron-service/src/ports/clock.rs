//! Wall-clock port.
//!
//! The service uses `Clock` for persistence timestamps, energy integration and
//! spool retry deadlines. Async waiting itself uses `tokio::time` (tests drive
//! it deterministically with `start_paused` + `advance`).

use std::time::SystemTime;

/// Injectable wall clock.
pub trait Clock: Send + Sync {
    fn now(&self) -> SystemTime;
}

/// Real system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}
