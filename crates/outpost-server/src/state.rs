//! Shared application state injected into every handler.

use sqlx::SqlitePool;
use std::sync::Arc;

/// Application state — held by axum's `with_state` and extracted via
/// `axum::extract::State<AppState>`.
///
/// `Arc<String>` for `jwt_secret` keeps clones cheap and prevents the
/// secret from being copied into every request handler stack.
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub jwt_secret: Arc<String>,
    pub jwt_ttl_secs: i64,
}

impl AppState {
    pub fn new(db: SqlitePool, jwt_secret: String, jwt_ttl_secs: i64) -> Self {
        Self {
            db,
            jwt_secret: Arc::new(jwt_secret),
            jwt_ttl_secs,
        }
    }
}

/// Open an in-memory pool with migrations applied + bootstrap and return a
/// fully-wired `AppState`. Handy for integration tests.
pub async fn test_state() -> AppState {
    let cfg = crate::config::Config::test_default();
    let pool = crate::db::open_pool(":memory:")
        .await
        .expect("open in-memory test pool");
    crate::bootstrap::bootstrap_pending_passwords(&pool)
        .await
        .expect("bootstrap test admin");
    AppState::new(pool, cfg.jwt_secret, cfg.jwt_ttl_secs)
}
