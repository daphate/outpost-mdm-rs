//! Permission helper — backed by the `user_role_permissions` table.
//!
//! Roles are looked up once per request; cache hit is the common path
//! (sqlx connection cache + prepared statement). For a fleet-management
//! server handling ~thousands of requests/min this is fine; if it
//! becomes hot we'll cache the role->permissions map in `AppState`.

use crate::error::ApiError;
use sqlx::SqlitePool;

/// Return `Ok(())` if the role has the named permission, else
/// [`ApiError::Forbidden`].
pub async fn require_permission(
    db: &SqlitePool,
    role_id: i64,
    permission: &str,
) -> Result<(), ApiError> {
    let row: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM user_role_permissions rp \
         JOIN permissions p ON p.id = rp.permission_id \
         WHERE rp.role_id = ? AND p.name = ?",
    )
    .bind(role_id)
    .bind(permission)
    .fetch_optional(db)
    .await
    .map_err(ApiError::from)?;

    if row.is_some() {
        Ok(())
    } else {
        tracing::warn!(role_id, permission, "permission denied");
        Err(ApiError::Forbidden)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[tokio::test]
    async fn super_admin_passes_every_permission() {
        let pool = db::open_pool(":memory:").await.unwrap();
        // Role 1 = super-admin (per 0002_users_auth.sql seed).
        for perm in [
            "devices.read",
            "devices.write",
            "applications.read",
            "applications.write",
            "push.send",
            "users.write",
            "files.write",
        ] {
            require_permission(&pool, 1, perm).await.unwrap();
        }
    }

    #[tokio::test]
    async fn viewer_role_is_read_only() {
        let pool = db::open_pool(":memory:").await.unwrap();
        // Role 4 = viewer.
        require_permission(&pool, 4, "devices.read").await.unwrap();
        let err = require_permission(&pool, 4, "devices.write")
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Forbidden));
    }

    #[tokio::test]
    async fn operator_can_push_but_not_manage_users() {
        let pool = db::open_pool(":memory:").await.unwrap();
        // Role 3 = operator.
        require_permission(&pool, 3, "push.send").await.unwrap();
        require_permission(&pool, 3, "users.read").await.unwrap();
        let err = require_permission(&pool, 3, "users.write")
            .await
            .unwrap_err();
        assert!(matches!(err, ApiError::Forbidden));
    }
}
