//! Graceful shutdown signal handling.
//!
//! Resolves on Ctrl+C (cross-platform) or SIGTERM (Unix only). On Windows
//! only Ctrl+C is wired — Docker stops containers with SIGTERM which the
//! Chainguard static image runtime delivers as a Ctrl+C-equivalent
//! interrupt.

use tokio::signal;

/// Future that resolves when the process is asked to shut down.
///
/// Use with `axum::serve(...).with_graceful_shutdown(signal())`.
pub async fn signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => tracing::info!("received Ctrl+C; initiating graceful shutdown"),
        () = terminate => tracing::info!("received SIGTERM; initiating graceful shutdown"),
    }
}
