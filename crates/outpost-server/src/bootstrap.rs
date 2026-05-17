//! First-boot bootstrap routines.
//!
//! On every startup we scan for users with `password_hash IS NULL` (the
//! seed pattern from `0009_seed_admin.sql`), generate a strong random
//! password for each, hash it with argon2id, persist the hash, and log
//! the cleartext password to stderr exactly once. Operators must capture
//! this line at first boot; the password is not recoverable after that.

use anyhow::{Context, Result};
use sqlx::SqlitePool;

use crate::auth;

/// Length of generated bootstrap passwords. 20 characters of alphanumeric
/// is ~120 bits of entropy — sufficient for an interactive admin pass.
const BOOTSTRAP_PASSWORD_LEN: usize = 20;

/// Detect every user with `password_hash IS NULL` and bootstrap them.
///
/// Returns the number of users bootstrapped. Idempotent — subsequent
/// calls after first boot are a no-op.
pub async fn bootstrap_pending_passwords(pool: &SqlitePool) -> Result<usize> {
    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, login FROM users WHERE password_hash IS NULL")
            .fetch_all(pool)
            .await
            .context("query users with NULL password_hash")?;

    if rows.is_empty() {
        return Ok(0);
    }

    let mut count = 0;
    for (user_id, login) in rows {
        let password = auth::generate_password(BOOTSTRAP_PASSWORD_LEN);
        let phc = auth::hash_password(&password).context("hash bootstrap password")?;

        sqlx::query(
            "UPDATE users \
             SET password_hash = ?, \
                 must_change_password = 1, \
                 updated_at = datetime('now') \
             WHERE id = ?",
        )
        .bind(&phc)
        .bind(user_id)
        .execute(pool)
        .await
        .with_context(|| format!("persist bootstrap hash for user {user_id}"))?;

        // Log the cleartext exactly once. Use eprintln! directly — the
        // tracing JSON layer would otherwise structure-log the secret and
        // it might leak into aggregation pipelines.
        eprintln!("==============================================================");
        eprintln!("  BOOTSTRAP: initial password for '{login}' (user_id={user_id})");
        eprintln!();
        eprintln!("  {password}");
        eprintln!();
        eprintln!("  Capture this NOW — it is not recoverable after this boot.");
        eprintln!("  You will be prompted to change it on first login.");
        eprintln!("==============================================================");

        count += 1;
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[tokio::test]
    async fn bootstraps_the_seed_admin_on_first_call() {
        let pool = db::open_pool(":memory:").await.unwrap();
        let n = bootstrap_pending_passwords(&pool).await.unwrap();
        assert_eq!(n, 1, "expected one seed admin to bootstrap");

        // Hash now non-NULL.
        let phc: Option<String> =
            sqlx::query_scalar("SELECT password_hash FROM users WHERE login = 'admin'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(phc.is_some());
    }

    #[tokio::test]
    async fn bootstrap_is_idempotent() {
        let pool = db::open_pool(":memory:").await.unwrap();
        let first = bootstrap_pending_passwords(&pool).await.unwrap();
        let second = bootstrap_pending_passwords(&pool).await.unwrap();
        assert_eq!(first, 1);
        assert_eq!(second, 0, "second call must be no-op");
    }
}
