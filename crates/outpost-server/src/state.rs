//! Shared application state injected into every handler.

use crate::cloudru_signer::CloudRuPresigner;
use crate::rate_limit::LoginRateLimiter;
use sqlx::SqlitePool;
use std::path::PathBuf;
use std::sync::Arc;

/// Application state — held by axum's `with_state` and extracted via
/// `axum::extract::State<AppState>`. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    /// Symmetric secret for HMAC-SHA256 on signed download URLs.
    pub app_secret: Arc<String>,
    /// User session TTL in seconds.
    pub session_ttl_secs: i64,
    pub app_files_dir: Arc<PathBuf>,
    pub max_body_bytes: usize,
    pub request_timeout_secs: u64,
    pub secure_cookies: bool,
    pub login_limiter: LoginRateLimiter,
    /// Cloud.ru presigner для генерации APK download QR'ов на странице
    /// enrollment. `None` если соответствующие env-vars не заданы — в этом
    /// случае admin UI скрывает APK-QR блок.
    pub cloudru_signer: Option<Arc<CloudRuPresigner>>,
    /// Object key для latest APK pointer (`apks/latest/app-debug.apk` by default).
    pub cloudru_apk_key: Arc<String>,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: SqlitePool,
        app_secret: String,
        session_ttl_secs: i64,
        app_files_dir: PathBuf,
        max_body_bytes: usize,
        request_timeout_secs: u64,
        secure_cookies: bool,
        cloudru_signer: Option<CloudRuPresigner>,
        cloudru_apk_key: String,
    ) -> Self {
        Self {
            db,
            app_secret: Arc::new(app_secret),
            session_ttl_secs,
            app_files_dir: Arc::new(app_files_dir),
            max_body_bytes,
            request_timeout_secs,
            secure_cookies,
            login_limiter: LoginRateLimiter::default_login(),
            cloudru_signer: cloudru_signer.map(Arc::new),
            cloudru_apk_key: Arc::new(cloudru_apk_key),
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
        cfg.app_secret,
        cfg.session_ttl_secs,
        make_test_dir(),
        cfg.max_body_bytes,
        cfg.request_timeout_secs,
        cfg.secure_cookies,
        None,
        cfg.cloudru_apk_key,
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
