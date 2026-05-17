//! Shared application state injected into every handler.

use sqlx::SqlitePool;
use std::path::PathBuf;
use std::sync::Arc;

/// Application state — held by axum's `with_state` and extracted via
/// `axum::extract::State<AppState>`.
///
/// Cheap to clone (everything mutable-shared wrapped in `Arc`).
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub jwt_secret: Arc<String>,
    pub jwt_ttl_secs: i64,
    pub app_files_dir: Arc<PathBuf>,
    pub max_body_bytes: usize,
    pub request_timeout_secs: u64,
    pub secure_cookies: bool,
}

impl AppState {
    pub fn new(
        db: SqlitePool,
        jwt_secret: String,
        jwt_ttl_secs: i64,
        app_files_dir: PathBuf,
        max_body_bytes: usize,
        request_timeout_secs: u64,
        secure_cookies: bool,
    ) -> Self {
        Self {
            db,
            jwt_secret: Arc::new(jwt_secret),
            jwt_ttl_secs,
            app_files_dir: Arc::new(app_files_dir),
            max_body_bytes,
            request_timeout_secs,
            secure_cookies,
        }
    }
}

/// In-memory DB + per-test temp file dir + bootstrapped admin. Always
/// available so out-of-crate integration tests can call without ceremony.
pub async fn test_state() -> AppState {
    let cfg = crate::config::Config::test_default();
    let pool = crate::db::open_pool(":memory:")
        .await
        .expect("open in-memory test pool");
    crate::bootstrap::bootstrap_pending_passwords(&pool)
        .await
        .expect("bootstrap test admin");
    AppState::new(
        pool,
        cfg.jwt_secret,
        cfg.jwt_ttl_secs,
        make_test_dir(),
        cfg.max_body_bytes,
        cfg.request_timeout_secs,
        cfg.secure_cookies,
    )
}

fn make_test_dir() -> PathBuf {
    use rand::Rng;
    use rand::distributions::Alphanumeric;
    let suffix: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(12)
        .map(char::from)
        .collect();
    let mut p = std::env::temp_dir();
    p.push(format!("outpost-test-{suffix}"));
    std::fs::create_dir_all(&p).expect("create test files dir");
    p
}
