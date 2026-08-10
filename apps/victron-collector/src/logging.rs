//! Tracing setup, compact and journald-friendly (no ANSI, targets on).

use tracing_subscriber::EnvFilter;

/// Initialise logging. Falls back to `level` when `RUST_LOG` is unset.
/// Returns false if a subscriber was already installed (e.g. in tests).
pub fn init(level: &str) -> bool {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .compact()
        .with_ansi(false)
        .with_target(true)
        .try_init()
        .is_ok()
}
