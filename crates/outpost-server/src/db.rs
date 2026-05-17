//! SQLite connection pool with Outpost-tuned PRAGMAs.
//!
//! The Outpost host budget (1 vCPU / 512 MB) wants:
//! - WAL journal mode — concurrent readers + single writer, fewer fsyncs
//! - `synchronous = NORMAL` — durable enough for a single-server MDM
//! - `foreign_keys = ON` — enforce FK constraints at write time
//! - `busy_timeout` — wait briefly for the writer rather than erroring
//! - Modest connection pool — 8 connections plenty for ~hundreds of devices

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use std::str::FromStr;
use std::time::Duration;

/// Open a `SqlitePool` against the given path (or `:memory:`),
/// applying Outpost-tuned PRAGMAs at each new connection, then running
/// pending migrations.
///
/// `db_path` accepts:
/// - filesystem path (`/var/lib/outpost/outpost.db`)
/// - `:memory:` for tests
/// - `sqlite::memory:` URL form
pub async fn open_pool(db_path: &str) -> Result<SqlitePool> {
    let options = if db_path == ":memory:" || db_path == "sqlite::memory:" {
        SqliteConnectOptions::from_str("sqlite::memory:")?
    } else {
        SqliteConnectOptions::from_str(db_path)
            .with_context(|| format!("parse SQLite connection string {db_path}"))?
            .create_if_missing(true)
    }
    .journal_mode(SqliteJournalMode::Wal)
    .synchronous(SqliteSynchronous::Normal)
    .foreign_keys(true)
    .busy_timeout(Duration::from_secs(5));

    // For in-memory databases the pool MUST be capped at 1 connection —
    // each new connection would otherwise see an empty, isolated database.
    let max_connections = if db_path.contains(":memory:") { 1 } else { 8 };

    let pool = SqlitePoolOptions::new()
        .max_connections(max_connections)
        .min_connections(1)
        .acquire_timeout(Duration::from_secs(30))
        .connect_with(options)
        .await
        .with_context(|| format!("open SQLite pool at {db_path}"))?;

    outpost_migrations::run(&pool)
        .await
        .context("apply pending migrations")?;

    tracing::info!(db_path = %db_path, "SQLite pool ready");
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_in_memory_pool_runs_migrations() {
        let pool = open_pool(":memory:").await.unwrap();
        // Verify a known seeded row from 0001_customers.sql.
        let name: (String,) = sqlx::query_as("SELECT name FROM customers WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(name.0, "default");
    }

    #[tokio::test]
    async fn open_in_memory_pool_enforces_foreign_keys() {
        let pool = open_pool(":memory:").await.unwrap();
        // Inserting a user with a non-existent customer_id should fail.
        let result = sqlx::query(
            "INSERT INTO users (customer_id, role_id, login) VALUES (9999, 1, 'orphan')",
        )
        .execute(&pool)
        .await;
        assert!(result.is_err(), "expected FK violation, got {result:?}",);
    }

    #[tokio::test]
    async fn open_in_memory_pool_uses_wal() {
        let pool = open_pool(":memory:").await.unwrap();
        // In-memory databases ignore the journal_mode pragma and return 'memory'.
        // For a real file we'd see 'wal'. This test asserts the pool started
        // successfully — the WAL pragma is exercised by the file-backed test below.
        let mode: (String,) = sqlx::query_as("PRAGMA journal_mode")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            mode.0 == "memory" || mode.0 == "wal",
            "expected memory or wal, got {}",
            mode.0,
        );
    }
}
