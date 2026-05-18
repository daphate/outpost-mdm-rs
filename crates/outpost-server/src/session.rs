//! Opaque DB-backed session tokens.
//!
//! Replaces the JWT machinery from v0.1.0. Tokens are 256-bit random
//! values encoded as 64-char hex; the server stores only the sha256 of
//! the token (so a DB-file leak does not expose live sessions). Verify
//! cost is one indexed lookup against `sessions.id_hash` (PRIMARY KEY).
//!
//! Why opaque instead of stateless JWT, on this project specifically:
//! - **Instant revocation** — `UPDATE sessions SET revoked_at = now()`
//!   takes effect on the next request. JWT requires rotating the global
//!   signing key, which invalidates every other session at the same time.
//! - **No `alg=none` / algorithm-confusion attack surface** — every
//!   class of JWT-library CVE evaporates.
//! - **Smaller wire footprint** — 64 bytes hex token vs ~300-500 bytes
//!   JWT, with no information leak about the payload.
//!
//! The DB hit per request is ~0.1 ms over the WAL'd SQLite pool — well
//! within budget for an admin panel + hundreds of devices.

use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

/// Token subject kinds. Keeping these as string constants in the DB
/// makes ad-hoc admin queries readable.
pub const KIND_USER: &str = "user";
pub const KIND_DEVICE: &str = "device";
/// Half-authenticated state for TOTP-gated logins. Password verified,
/// second factor pending. Short TTL (5 min); the WebUser extractor
/// rejects this kind so no protected endpoint can be reached until
/// /login/2fa upgrades it to KIND_USER by issuing a new session.
pub const KIND_PENDING_2FA: &str = "pending_2fa";

/// Result of a successful `verify` — populated from the cached columns
/// on the `sessions` row.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Session {
    /// sha256 of the bearer token; never the bearer token itself.
    pub id_hash: String,
    /// `"user"` or `"device"`.
    pub kind: String,
    /// `users.id` or `devices.id` depending on `kind`.
    pub subject_id: i64,
    pub customer_id: i64,
    /// `users.role_id` at issuance time; `0` for device sessions.
    pub role_id: i64,
    /// `users.login` or `devices.serial` at issuance time.
    pub login: String,
    pub issued_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("invalid or expired session")]
    Invalid,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Compute the storage hash for a bearer token.
fn hash(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    hex::encode(h.finalize())
}

/// Generate a fresh 256-bit random token, hex-encoded.
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Create a session for a user. Returns the bearer token (the original
/// random hex) — the caller MUST hand it to the client; the server
/// retains only its sha256.
pub async fn create_user_session(
    db: &SqlitePool,
    user_id: i64,
    customer_id: i64,
    role_id: i64,
    login: &str,
    ttl_secs: i64,
) -> Result<String, sqlx::Error> {
    create(
        db,
        KIND_USER,
        user_id,
        customer_id,
        role_id,
        login,
        ttl_secs,
    )
    .await
}

/// Issue a short-lived pending-2FA session. Used between password verify
/// and TOTP verify. Cannot reach any protected endpoint — see
/// `WebUser::from_request_parts` rejecting any kind other than KIND_USER.
pub async fn create_pending_2fa_session(
    db: &SqlitePool,
    user_id: i64,
    customer_id: i64,
    role_id: i64,
    login: &str,
) -> Result<String, sqlx::Error> {
    // 5-minute TTL — long enough to fish out the phone, short enough that
    // a forgotten browser tab doesn't sit half-authenticated forever.
    create(db, KIND_PENDING_2FA, user_id, customer_id, role_id, login, 300).await
}

/// Create a session for an enrolled device. `role_id` is forced to `0`.
pub async fn create_device_session(
    db: &SqlitePool,
    device_id: i64,
    customer_id: i64,
    serial: &str,
    ttl_secs: i64,
) -> Result<String, sqlx::Error> {
    create(db, KIND_DEVICE, device_id, customer_id, 0, serial, ttl_secs).await
}

async fn create(
    db: &SqlitePool,
    kind: &str,
    subject_id: i64,
    customer_id: i64,
    role_id: i64,
    login: &str,
    ttl_secs: i64,
) -> Result<String, sqlx::Error> {
    let token = random_token();
    let id_hash = hash(&token);
    // Compute expires_at in Rust (so negative TTLs work for "already-expired"
    // test fixtures) and format it as SQLite's canonical
    // "YYYY-MM-DD HH:MM:SS" so that lex-string comparison against
    // `datetime('now')` in verify queries is well-defined.
    let expires_at = (chrono::Utc::now() + chrono::Duration::seconds(ttl_secs))
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    sqlx::query(
        "INSERT INTO sessions \
            (id_hash, kind, subject_id, customer_id, role_id, login, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id_hash)
    .bind(kind)
    .bind(subject_id)
    .bind(customer_id)
    .bind(role_id)
    .bind(login)
    .bind(&expires_at)
    .execute(db)
    .await?;
    Ok(token)
}

/// Verify a bearer token against the live session row. Returns the
/// session metadata on success, `Invalid` on revoked / expired / unknown.
pub async fn verify(token: &str, db: &SqlitePool) -> Result<Session, SessionError> {
    let id_hash = hash(token);
    let session: Option<Session> = sqlx::query_as::<_, Session>(
        "SELECT id_hash, kind, subject_id, customer_id, role_id, login, issued_at, expires_at \
         FROM sessions \
         WHERE id_hash = ? AND revoked_at IS NULL AND expires_at > datetime('now')",
    )
    .bind(&id_hash)
    .fetch_optional(db)
    .await?;
    session.ok_or(SessionError::Invalid)
}

/// v0.17 sliding refresh: extend the active session expiry for a subject
/// (kind+subject_id) up to `full_ttl_secs` from now, but only if the remaining
/// lifetime is less than `threshold_pct%` of full TTL.
///
/// Returns `Ok(true)` if any row was updated.
///
/// **Purpose: keep month-offline devices working without re-enroll.** While a
/// device is *online* (each /sync call hits this code path), its session
/// effectively never expires — bumped each time remaining < threshold%. Once
/// the device stays offline longer than `full_ttl_secs` (default 90 days)
/// without a single sync, the session lapses and a full re-enroll cycle
/// (admin generates new payload + device scans QR) is required.
///
/// **Safety:** `WHERE revoked_at IS NULL AND expires_at > now()` guarantees
/// we never resurrect revoked or already-expired sessions. If admin called
/// `revoke-enrollment` or `DELETE /devices/{id}`, refresh is a no-op.
pub async fn refresh_if_aging_for_subject(
    db: &SqlitePool,
    kind: &str,
    subject_id: i64,
    full_ttl_secs: i64,
    threshold_pct: i64,
) -> Result<bool, sqlx::Error> {
    let aging_seconds = full_ttl_secs * threshold_pct / 100;
    let modifier_now = format!("+{full_ttl_secs} seconds");
    let modifier_aging = format!("+{aging_seconds} seconds");
    let res = sqlx::query(
        "UPDATE sessions \
         SET expires_at = datetime('now', ?) \
         WHERE kind = ? AND subject_id = ? \
           AND revoked_at IS NULL \
           AND expires_at > datetime('now') \
           AND expires_at < datetime('now', ?)",
    )
    .bind(&modifier_now)
    .bind(kind)
    .bind(subject_id)
    .bind(&modifier_aging)
    .execute(db)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// Revoke a session by its bearer token (sets `revoked_at` to now).
/// Idempotent — revoking an already-revoked session is a no-op.
pub async fn revoke(token: &str, db: &SqlitePool) -> Result<bool, sqlx::Error> {
    let id_hash = hash(token);
    let res = sqlx::query(
        "UPDATE sessions SET revoked_at = datetime('now') \
         WHERE id_hash = ? AND revoked_at IS NULL",
    )
    .bind(&id_hash)
    .execute(db)
    .await?;
    Ok(res.rows_affected() > 0)
}

/// Bulk-revoke all sessions for one subject (e.g. after password reset
/// or a "log out everywhere" admin action).
pub async fn revoke_all_for_subject(
    db: &SqlitePool,
    kind: &str,
    subject_id: i64,
) -> Result<u64, sqlx::Error> {
    let res = sqlx::query(
        "UPDATE sessions SET revoked_at = datetime('now') \
         WHERE kind = ? AND subject_id = ? AND revoked_at IS NULL",
    )
    .bind(kind)
    .bind(subject_id)
    .execute(db)
    .await?;
    Ok(res.rows_affected())
}

/// Garbage-collect sessions expired or revoked more than `keep_days`
/// days ago. Called from the scheduler tick.
pub async fn cleanup(db: &SqlitePool, keep_days: i64) -> Result<u64, sqlx::Error> {
    let modifier = format!("-{keep_days} days");
    let res = sqlx::query(
        "DELETE FROM sessions \
         WHERE (expires_at < datetime('now', ?)) \
            OR (revoked_at IS NOT NULL AND revoked_at < datetime('now', ?))",
    )
    .bind(&modifier)
    .bind(&modifier)
    .execute(db)
    .await?;
    Ok(res.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[tokio::test]
    async fn create_then_verify_round_trip() {
        let pool = db::open_pool(":memory:").await.unwrap();
        let token = create_user_session(&pool, 1, 1, 1, "admin", 3600)
            .await
            .unwrap();
        assert_eq!(token.len(), 64, "token must be 64-char hex");
        let s = verify(&token, &pool).await.unwrap();
        assert_eq!(s.subject_id, 1);
        assert_eq!(s.kind, KIND_USER);
        assert_eq!(s.login, "admin");
    }

    #[tokio::test]
    async fn revoked_session_fails_verify() {
        let pool = db::open_pool(":memory:").await.unwrap();
        let token = create_user_session(&pool, 1, 1, 1, "admin", 3600)
            .await
            .unwrap();
        let revoked = revoke(&token, &pool).await.unwrap();
        assert!(revoked);
        let err = verify(&token, &pool).await.unwrap_err();
        assert!(matches!(err, SessionError::Invalid));
    }

    #[tokio::test]
    async fn expired_session_fails_verify() {
        let pool = db::open_pool(":memory:").await.unwrap();
        // -1 second TTL → already expired
        let token = create_user_session(&pool, 1, 1, 1, "admin", -1)
            .await
            .unwrap();
        let err = verify(&token, &pool).await.unwrap_err();
        assert!(matches!(err, SessionError::Invalid));
    }

    #[tokio::test]
    async fn unknown_token_fails_verify() {
        let pool = db::open_pool(":memory:").await.unwrap();
        let err = verify("00".repeat(32).as_str(), &pool).await.unwrap_err();
        assert!(matches!(err, SessionError::Invalid));
    }

    #[tokio::test]
    async fn db_stores_only_hash_not_token() {
        let pool = db::open_pool(":memory:").await.unwrap();
        let token = create_user_session(&pool, 1, 1, 1, "admin", 3600)
            .await
            .unwrap();
        // Token must NOT appear verbatim anywhere in the sessions table.
        let rows: Vec<(String,)> = sqlx::query_as("SELECT id_hash FROM sessions")
            .fetch_all(&pool)
            .await
            .unwrap();
        for (id_hash,) in rows {
            assert_ne!(
                id_hash, token,
                "sessions table must not store the raw token"
            );
            assert_eq!(id_hash.len(), 64);
        }
    }

    #[tokio::test]
    async fn revoke_all_for_subject_invalidates_every_session() {
        let pool = db::open_pool(":memory:").await.unwrap();
        let t1 = create_user_session(&pool, 1, 1, 1, "admin", 3600)
            .await
            .unwrap();
        let t2 = create_user_session(&pool, 1, 1, 1, "admin", 3600)
            .await
            .unwrap();
        let t3 = create_user_session(&pool, 2, 1, 2, "alice", 3600)
            .await
            .unwrap();

        let count = revoke_all_for_subject(&pool, KIND_USER, 1).await.unwrap();
        assert_eq!(count, 2);
        assert!(verify(&t1, &pool).await.is_err());
        assert!(verify(&t2, &pool).await.is_err());
        // Other user's session must remain valid.
        assert!(verify(&t3, &pool).await.is_ok());
    }

    #[tokio::test]
    async fn device_session_uses_kind_device() {
        let pool = db::open_pool(":memory:").await.unwrap();
        let token = create_device_session(&pool, 42, 1, "ULF-042", 90 * 24 * 3600)
            .await
            .unwrap();
        let s = verify(&token, &pool).await.unwrap();
        assert_eq!(s.kind, KIND_DEVICE);
        assert_eq!(s.role_id, 0);
        assert_eq!(s.subject_id, 42);
        assert_eq!(s.login, "ULF-042");
    }

    #[tokio::test]
    async fn cleanup_drops_old_expired_rows() {
        let pool = db::open_pool(":memory:").await.unwrap();
        // Insert an expired session by setting expires_at manually
        let id_hash = hash("test-token");
        sqlx::query(
            "INSERT INTO sessions (id_hash, kind, subject_id, customer_id, role_id, login, expires_at) \
             VALUES (?, 'user', 1, 1, 1, 'admin', datetime('now', '-100 days'))",
        )
        .bind(&id_hash)
        .execute(&pool)
        .await
        .unwrap();
        let deleted = cleanup(&pool, 30).await.unwrap();
        assert_eq!(deleted, 1);
    }

    #[tokio::test]
    async fn refresh_aging_session_extends_expiry() {
        let pool = db::open_pool(":memory:").await.unwrap();
        // Создаём session со сроком всего 10 секунд при TTL 90 дней → aging
        // (remaining 10s << 50% от 90 дней).
        let _ = create_device_session(&pool, 7, 1, "DEV-007", 10)
            .await
            .unwrap();
        let updated = refresh_if_aging_for_subject(
            &pool,
            KIND_DEVICE,
            7,
            /* full_ttl */ 90 * 24 * 3600,
            /* threshold_pct */ 50,
        )
        .await
        .unwrap();
        assert!(updated, "aging session must be refreshed");
        // После refresh — expires_at в far-future (90 дней).
        let (remaining,): (i64,) = sqlx::query_as(
            "SELECT CAST((julianday(expires_at) - julianday('now')) * 86400 AS INTEGER) \
             FROM sessions WHERE kind = 'device' AND subject_id = 7",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        // ≥ 89 дней (даём 1 день запаса на test latency).
        assert!(
            remaining > 89 * 24 * 3600,
            "expected ~90 days remaining, got {remaining} seconds"
        );
    }

    #[tokio::test]
    async fn refresh_fresh_session_is_noop() {
        let pool = db::open_pool(":memory:").await.unwrap();
        // Свежая session — full TTL, remaining 100% → выше threshold → no-op.
        let _ = create_device_session(&pool, 8, 1, "DEV-008", 90 * 24 * 3600)
            .await
            .unwrap();
        let updated = refresh_if_aging_for_subject(
            &pool,
            KIND_DEVICE,
            8,
            90 * 24 * 3600,
            50,
        )
        .await
        .unwrap();
        assert!(!updated, "fresh session must NOT be refreshed");
    }

    #[tokio::test]
    async fn refresh_revoked_session_is_noop() {
        let pool = db::open_pool(":memory:").await.unwrap();
        let token = create_device_session(&pool, 9, 1, "DEV-009", 10)
            .await
            .unwrap();
        revoke(&token, &pool).await.unwrap();
        let updated = refresh_if_aging_for_subject(
            &pool,
            KIND_DEVICE,
            9,
            90 * 24 * 3600,
            50,
        )
        .await
        .unwrap();
        assert!(!updated, "revoked session must NEVER be resurrected");
    }

    #[tokio::test]
    async fn refresh_expired_session_is_noop() {
        let pool = db::open_pool(":memory:").await.unwrap();
        // -1 second TTL → already expired.
        let _ = create_device_session(&pool, 10, 1, "DEV-010", -1)
            .await
            .unwrap();
        let updated = refresh_if_aging_for_subject(
            &pool,
            KIND_DEVICE,
            10,
            90 * 24 * 3600,
            50,
        )
        .await
        .unwrap();
        assert!(!updated, "expired session must NOT be refreshed — require full re-enroll");
    }
}
