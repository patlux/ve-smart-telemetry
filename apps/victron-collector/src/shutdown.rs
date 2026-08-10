//! Shutdown signal handling: SIGTERM and SIGINT both request a graceful stop.
//!
//! Handler installation failure is a **startup error**: the daemon must not
//! run without a working shutdown path (that would leave an immortal process
//! that can only be SIGKILLed).

use tokio::sync::watch;

/// Failure to install the shutdown signal handlers.
#[derive(Debug, thiserror::Error)]
pub enum ShutdownError {
    #[error("failed to install SIGTERM handler: {0}")]
    SignalInstall(std::io::Error),
}

/// Install handlers and return the shutdown watch. The receiver flips to
/// `true` once SIGTERM or SIGINT arrives.
pub async fn install() -> Result<watch::Receiver<bool>, ShutdownError> {
    let (tx, rx) = watch::channel(false);

    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(ShutdownError::SignalInstall)?;
        let tx = tx.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            tracing::info!("shutdown signal received");
            let _ = tx.send(true);
        });
    }

    #[cfg(not(unix))]
    {
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = tx.send(true);
        });
    }

    Ok(rx)
}
