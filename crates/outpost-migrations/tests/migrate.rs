//! Integration tests for the SQLite migration set.
//!
//! Boots an in-memory database, applies every migration in `migrations/`,
//! and asserts the resulting schema matches what the rest of the project
//! relies on. This is the canonical safety net for P2 — every later
//! schema-touching commit must keep these tests green.

use sqlx::Row;
use sqlx::sqlite::SqlitePoolOptions;

async fn fresh_pool() -> sqlx::SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("open in-memory sqlite");
    outpost_migrations::run(&pool)
        .await
        .expect("apply migrations");
    pool
}

#[tokio::test]
async fn migrations_apply_cleanly() {
    let _pool = fresh_pool().await;
}

#[tokio::test]
async fn migrations_are_idempotent() {
    let pool = fresh_pool().await;
    // Running again should be a no-op (sqlx_migrations tracks applied versions).
    outpost_migrations::run(&pool)
        .await
        .expect("re-apply migrations is idempotent");
}

#[tokio::test]
async fn expected_core_tables_exist() {
    let pool = fresh_pool().await;
    let expected = [
        "customers",
        "user_roles",
        "permissions",
        "user_role_permissions",
        "users",
        "groups",
        "devices",
        "device_groups",
        "applications",
        "application_versions",
        "configurations",
        "configuration_applications",
        "uploaded_files",
        "push_messages",
        "push_schedule",
        "settings",
    ];
    for table in expected {
        let row = sqlx::query("SELECT name FROM sqlite_master WHERE type='table' AND name=?")
            .bind(table)
            .fetch_optional(&pool)
            .await
            .unwrap();
        assert!(row.is_some(), "missing table: {table}");
    }
}

#[tokio::test]
async fn seed_customer_exists() {
    let pool = fresh_pool().await;
    let row = sqlx::query("SELECT name FROM customers WHERE id = 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    let name: String = row.get(0);
    assert_eq!(name, "default");
}

#[tokio::test]
async fn seed_user_roles_are_complete() {
    let pool = fresh_pool().await;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM user_roles")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 4, "expected 4 seeded roles");

    // super-admin should have every permission assigned.
    let total_perms: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM permissions")
        .fetch_one(&pool)
        .await
        .unwrap();
    let super_admin_perms: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_role_permissions WHERE role_id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(super_admin_perms, total_perms);
}

#[tokio::test]
async fn seed_admin_user_exists_with_null_password() {
    let pool = fresh_pool().await;
    let row = sqlx::query(
        "SELECT login, password_hash, must_change_password FROM users WHERE login = 'admin'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let login: String = row.get(0);
    let password_hash: Option<String> = row.get(1);
    let must_change: i64 = row.get(2);
    assert_eq!(login, "admin");
    assert!(
        password_hash.is_none(),
        "seed admin must start with NULL password_hash for first-boot bootstrap"
    );
    assert_eq!(
        must_change, 1,
        "seed admin must have must_change_password = 1"
    );
}

#[tokio::test]
async fn foreign_keys_are_enforced_when_pragma_enabled() {
    let pool = fresh_pool().await;
    // PRAGMA foreign_keys is OFF by default in a fresh connection; the
    // server's `db::open_pool` turns it on. For this test we enable it
    // explicitly on the test pool's connection.
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .unwrap();
    // Inserting a user with a non-existent customer_id should fail.
    let result =
        sqlx::query("INSERT INTO users (customer_id, role_id, login) VALUES (9999, 1, 'orphan')")
            .execute(&pool)
            .await;
    assert!(result.is_err(), "expected FK violation");
}

#[tokio::test]
async fn settings_are_seeded() {
    let pool = fresh_pool().await;
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM settings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(n >= 5, "expected at least 5 seeded settings, got {n}");
}
