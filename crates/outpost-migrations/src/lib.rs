//! Outpost MDM SQLite migrations.
//!
//! Migrations are embedded into the binary via the `sqlx::migrate!()` macro
//! pointed at the sibling `migrations/` directory. The build artifact ships
//! the schema as compile-time-embedded data; no separate file deployment is
//! required.
//!
//! Conventions:
//! - One concern per file (auth, devices, applications, …)
//! - Filenames are numbered `NNNN_topic.sql` and applied in lexicographic order
//! - Boolean values are stored as `INTEGER NOT NULL DEFAULT 0/1`
//! - Timestamps are stored as ISO-8601 TEXT (`datetime('now')`) and surface
//!   as `chrono::DateTime<Utc>` in Rust via `sqlx`'s chrono integration
//! - PRAGMAs (`foreign_keys`, `journal_mode = WAL`, `synchronous = NORMAL`)
//!   are applied at **connection** open by the server crate's pool builder —
//!   they are connection-level, not stored in the schema.

/// Compile-time-embedded migrator for the Outpost MDM schema.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Apply all pending migrations to a connection pool. Idempotent.
pub async fn run(pool: &sqlx::SqlitePool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}
