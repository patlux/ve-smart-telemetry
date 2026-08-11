//! Bounded stderr diagnostics for opt-in CLI investigation.

use tracing_subscriber::EnvFilter;

#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    #[error("invalid RUST_LOG filter")]
    InvalidFilter,
    #[error("failed to install diagnostic logger")]
    Install,
}

fn parse_filter(configured: &str) -> Result<EnvFilter, LoggingError> {
    EnvFilter::try_new(configured).map_err(|_| LoggingError::InvalidFilter)
}

pub fn init() -> Result<(), LoggingError> {
    let configured = std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_owned());
    let filter = parse_filter(&configured)?;
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .compact()
        .with_ansi(false)
        .with_target(true)
        .with_writer(std::io::stderr)
        .try_init()
        .map_err(|_| LoggingError::Install)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_filter_is_rejected_without_echoing_it() {
        assert!(matches!(
            parse_filter("["),
            Err(LoggingError::InvalidFilter)
        ));
        assert_eq!(
            LoggingError::InvalidFilter.to_string(),
            "invalid RUST_LOG filter"
        );
    }

    #[test]
    fn bounded_module_filters_are_accepted() {
        parse_filter("warn,victron_bluez=debug,victron_client=debug").unwrap();
    }
}
