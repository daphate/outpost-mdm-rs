//! Outpost MDM server binary entry point.

use anyhow::{Context, Result};
use outpost_server::{app, bootstrap, config::Config, db, shutdown, state::AppState};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = Config::from_env().context("load config from env")?;

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

    let pool = db::open_pool(&cfg.db_path).await.context("open db pool")?;
    let bootstrapped = bootstrap::bootstrap_pending_passwords(&pool)
        .await
        .context("bootstrap pending passwords")?;
    if bootstrapped > 0 {
        tracing::warn!(count = bootstrapped, "bootstrapped initial passwords");
    }

    let state = AppState::new(pool, cfg.jwt_secret, cfg.jwt_ttl_secs);

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr)
        .await
        .with_context(|| format!("bind {}", cfg.bind_addr))?;

    let actual_addr = listener.local_addr().context("local_addr")?;
    tracing::info!(addr = %actual_addr, "listening");

    axum::serve(listener, app::build_router(state))
        .with_graceful_shutdown(shutdown::signal())
        .await
        .context("axum::serve")?;

    tracing::info!("outpost-mdm-rs stopped cleanly");
    Ok(())
}
