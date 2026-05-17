//! Outpost MDM server binary entry point.
//!
//! Reads configuration from the environment, wires structured tracing,
//! builds the application router, and serves it on the configured bind
//! address with graceful shutdown.

use anyhow::{Context, Result};
use outpost_server::{app, config::Config, shutdown};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = Config::from_env();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(&cfg.log_level))
        .json()
        .init();

    tracing::info!(
        bind_addr = %cfg.bind_addr,
        db_path = %cfg.db_path,
        log_level = %cfg.log_level,
        version = env!("CARGO_PKG_VERSION"),
        "outpost-mdm-rs starting",
    );

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr)
        .await
        .with_context(|| format!("bind {}", cfg.bind_addr))?;

    let actual_addr = listener.local_addr().context("local_addr")?;
    tracing::info!(addr = %actual_addr, "listening");

    axum::serve(listener, app::build_router())
        .with_graceful_shutdown(shutdown::signal())
        .await
        .context("axum::serve")?;

    tracing::info!("outpost-mdm-rs stopped cleanly");
    Ok(())
}
