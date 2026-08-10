//! Bounded execution for BlueZ operations.
//!
//! BlueZ D-Bus method calls have an internal ceiling (bluer proxies use a
//! 120s timeout), which is far too long for a collector that must react to a
//! dead controller or unreachable device. Every potentially hanging operation
//! is wrapped with [`bounded`] using the single coherent
//! [`operation_timeout`](crate::session::TransportConfig::operation_timeout).

use std::future::Future;
use std::time::Duration;

use crate::error::BleError;

/// Run `fut` with a deadline.
///
/// On expiry returns [`BleError::Timeout`] tagged with `operation`. The inner
/// future must already map its own errors to [`BleError`], which is the only
/// error type this crate's I/O paths produce.
// Only the bluer backend calls this helper; in pure (`--no-default-features`)
// builds it is a test-only utility.
#[cfg_attr(not(feature = "bluer"), allow(dead_code))]
pub(crate) async fn bounded<T, E, F>(
    operation: &'static str,
    timeout: Duration,
    fut: F,
) -> Result<T, E>
where
    E: From<BleError>,
    F: Future<Output = Result<T, E>>,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(result) => result,
        Err(_) => Err(BleError::Timeout { operation }.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn bounded_surfaces_timeout_with_operation_label() {
        let never: std::future::Pending<Result<(), BleError>> = std::future::pending();
        let err = bounded("adapter-powered", Duration::from_millis(10), never)
            .await
            .unwrap_err();
        assert_eq!(
            err,
            BleError::Timeout {
                operation: "adapter-powered"
            }
        );
        assert_eq!(err.class(), crate::error::BleErrorClass::Timeout);
    }

    #[tokio::test]
    async fn bounded_passes_through_success() {
        let out = bounded("op", Duration::from_secs(5), async {
            Ok::<_, BleError>(42)
        })
        .await
        .unwrap();
        assert_eq!(out, 42);
    }

    #[tokio::test]
    async fn bounded_passes_through_errors() {
        let out: Result<(), BleError> = bounded("op", Duration::from_secs(5), async {
            Err(BleError::Other { detail: "x" })
        })
        .await;
        assert_eq!(out.unwrap_err(), BleError::Other { detail: "x" });
    }
}
