//! Shared application state injected into every handler.

use sqlx::SqlitePool;

/// Application state — held by axum's `with_state` and extracted via
/// `axum::extract::State<AppState>`.
///
/// Cheap to clone (Arc<...> inside): a fresh clone per request is normal.
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
}

impl AppState {
    pub fn new(db: SqlitePool) -> Self {
        Self { db }
    }
}

/// Open an in-memory pool with migrations applied — handy for integration
/// tests. Always available (not gated behind a feature) so out-of-crate
/// tests can call it without ceremony.
pub async fn test_state() -> AppState {
    let pool = crate::db::open_pool(":memory:")
        .await
        .expect("open in-memory test pool");
    AppState::new(pool)
}
