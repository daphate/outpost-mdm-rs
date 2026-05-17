//! Outpost MDM server binary entry point.

use std::path::Path;

use anyhow::{Context, Result};
use outpost_server::{
    apk_watcher, app, bootstrap, config::Config, db, rollout_monitor, scheduler, shutdown,
    state::AppState,
};
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
        app_files_dir = %cfg.app_files_dir.display(),
        log_level = %cfg.log_level,
        max_body_bytes = cfg.max_body_bytes,
        request_timeout_secs = cfg.request_timeout_secs,
        session_ttl_secs = cfg.session_ttl_secs,
        version = env!("CARGO_PKG_VERSION"),
        "outpost-mdm-rs starting",
    );

    // Verify both the DB parent dir and the app files dir are writable BEFORE
    // we open the SQLite pool — otherwise the container churns through a
    // restart loop while the operator stares at "Permission denied (os error 13)"
    // without any hint about which UID or which path is the problem.
    if let Some(db_parent) = Path::new(&cfg.db_path).parent() {
        if !db_parent.as_os_str().is_empty() {
            ensure_dir_writable(db_parent)
                .await
                .with_context(|| format!("db parent dir {}", db_parent.display()))?;
        }
    }
    ensure_dir_writable(&cfg.app_files_dir)
        .await
        .with_context(|| format!("app_files_dir {}", cfg.app_files_dir.display()))?;

    let pool = db::open_pool(&cfg.db_path).await.context("open db pool")?;
    let bootstrapped = bootstrap::bootstrap_pending_passwords(&pool)
        .await
        .context("bootstrap pending passwords")?;
    if bootstrapped > 0 {
        tracing::warn!(count = bootstrapped, "bootstrapped initial passwords");
    }

    let state = AppState::new(
        pool.clone(),
        cfg.app_secret,
        cfg.session_ttl_secs,
        cfg.app_files_dir,
        cfg.max_body_bytes,
        cfg.request_timeout_secs,
        cfg.secure_cookies,
    );
    let _scheduler_handle = scheduler::spawn(pool.clone());
    // v0.11: APK upstream watcher. Polls R2 mirror каждые 15 минут,
    // регистрирует свежие сборки Outpost-Android в `application_versions`.
    // No-op'ит если upstream pointer не движется.
    let _apk_watcher_handle = apk_watcher::spawn(pool.clone());
    // v0.12 Tier-2: rollout monitor. Каждые 60 секунд проходит по
    // `application_rollouts` с phase='canary' — auto-promote по
    // canary_until_at, auto-rollback по crash-rate gate (5% по умолчанию).
    let _rollout_monitor_handle = rollout_monitor::spawn(pool);

    let listener = tokio::net::TcpListener::bind(&cfg.bind_addr)
        .await
        .with_context(|| format!("bind {}", cfg.bind_addr))?;
    let actual_addr = listener.local_addr().context("local_addr")?;
    tracing::info!(addr = %actual_addr, "listening");

    axum::serve(
        listener,
        app::build_router(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown::signal())
    .await
    .context("axum::serve")?;

    tracing::info!("outpost-mdm-rs stopped cleanly");
    Ok(())
}

/// Create `dir` if missing and confirm the running process can actually write
/// to it. The probe step catches the case where the directory exists but is
/// owned by a different UID — which is what happens when the Chainguard
/// `nonroot` container (UID 65532) lands on a Docker named volume that was
/// freshly created and is therefore root-owned. The plain `create_dir_all`
/// path returns the same opaque `os error 13` for both "dir missing + no
/// permission to create" and "dir present + not writable"; this helper
/// flattens both into one actionable error message.
async fn ensure_dir_writable(dir: &Path) -> Result<()> {
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("create_dir_all {}", dir.display()))?;

    let probe = dir.join(".outpost-write-probe");
    match tokio::fs::write(&probe, b"").await {
        Ok(()) => {
            let _ = tokio::fs::remove_file(&probe).await;
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(anyhow::anyhow!(
            "data directory {dir} exists but is not writable by the running process. \
             The Chainguard runtime image runs as UID 65532; if you mounted a Docker \
             named volume, pre-chown its on-host path to 65532:65532, or switch to a \
             bind mount whose host directory is owned by 65532:65532.",
            dir = dir.display(),
        )),
        Err(e) => Err(anyhow::Error::new(e).context(format!(
            "write probe to {dir}",
            dir = dir.display()
        ))),
    }
}
