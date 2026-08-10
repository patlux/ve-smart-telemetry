//! Tracing setup, compact and journald-friendly (no ANSI, targets on).

use tracing_subscriber::EnvFilter;

/// Logging initialization failure. The daemon must not run silently.
#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("invalid log filter '{filter}': {reason}")]
    InvalidFilter { filter: String, reason: String },
    #[error("failed to install tracing subscriber: {0}")]
    Install(String),
}

fn parse_filter(configured: &str) -> Result<EnvFilter, LoggingError> {
    EnvFilter::try_new(configured).map_err(|error| LoggingError::InvalidFilter {
        filter: configured.to_owned(),
        reason: error.to_string(),
    })
}

/// Initialise logging. `RUST_LOG` overrides `level` when set.
pub fn init(level: &str) -> Result<(), LoggingError> {
    let configured = std::env::var("RUST_LOG").unwrap_or_else(|_| level.to_owned());
    let filter = parse_filter(&configured)?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .compact()
        .with_ansi(false)
        .with_target(true)
        .try_init()
        .map_err(|error| LoggingError::Install(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_filter_is_rejected() {
        assert!(matches!(
            parse_filter("["),
            Err(LoggingError::InvalidFilter { filter, .. }) if filter == "["
        ));
    }

    #[test]
    fn module_directives_are_accepted() {
        parse_filter("info,victron_bluez=debug,victron_collector=trace").unwrap();
    }
}
