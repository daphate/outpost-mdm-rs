//! Authentication primitives — argon2id password hashing and HS512 JWT.
//!
//! The module isolates all crypto so the rest of the codebase only sees
//! `hash_password`, `verify_password`, `issue_token`, `verify_token`, plus
//! a typed `Claims` struct.
//!
//! Algorithm choices (May 2026):
//! - **argon2id** — OWASP-recommended; defaults from the `argon2` crate
//!   are tuned for interactive auth. We use them as-is.
//! - **HS512** — symmetric JWT signing. Outpost ships a single trusted
//!   server, so asymmetric keys would add operational cost without value.
//!   The secret is held in `AppState::jwt_secret` and sourced from the
//!   `JWT_SECRET` env var, required at startup.

use anyhow::{Context, Result, anyhow};
use argon2::{
    Argon2, PasswordHasher, PasswordVerifier,
    password_hash::{PasswordHash, SaltString, rand_core::OsRng},
};
use chrono::Utc;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

/// JWT claim set issued by `issue_token` and recovered by `verify_token`.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    /// Subject — primary key of the `users` row.
    pub sub: i64,
    /// Tenant of the user.
    pub customer_id: i64,
    /// Role id of the user.
    pub role_id: i64,
    /// Login (display only — do not authorise from this).
    pub login: String,
    /// Issued-at (unix seconds).
    pub iat: i64,
    /// Expires-at (unix seconds).
    pub exp: i64,
    /// JWT id (UUID v4) — handy for revocation and audit.
    pub jti: String,
}

/// Argon2id-hash the password using a fresh salt.
///
/// Returns the PHC-encoded string suitable for direct storage in the
/// `users.password_hash` column.
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow!("argon2 hash: {e}"))?;
    Ok(hash.to_string())
}

/// Constant-time verify the password against the PHC-encoded hash.
pub fn verify_password(password: &str, phc: &str) -> Result<bool> {
    let parsed = PasswordHash::new(phc).map_err(|e| anyhow!("argon2 parse: {e}"))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

/// Issue an HS512 JWT for the given user.
///
/// `ttl_secs` controls the `exp` claim relative to "now"; pass the value
/// configured via `Config::jwt_ttl_secs`.
pub fn issue_token(
    user_id: i64,
    customer_id: i64,
    role_id: i64,
    login: &str,
    secret: &str,
    ttl_secs: i64,
) -> Result<String> {
    let now = Utc::now().timestamp();
    let claims = Claims {
        sub: user_id,
        customer_id,
        role_id,
        login: login.to_string(),
        iat: now,
        exp: now + ttl_secs,
        jti: uuid::Uuid::new_v4().to_string(),
    };
    let header = Header::new(Algorithm::HS512);
    let key = EncodingKey::from_secret(secret.as_bytes());
    encode(&header, &claims, &key).context("jwt encode")
}

/// Verify and decode the JWT, returning its claims.
///
/// Rejects expired tokens, signature mismatches, and any future
/// algorithm-confusion attacks (validator pinned to HS512).
pub fn verify_token(token: &str, secret: &str) -> Result<Claims> {
    let key = DecodingKey::from_secret(secret.as_bytes());
    let validation = Validation::new(Algorithm::HS512);
    decode::<Claims>(token, &key, &validation)
        .map(|t| t.claims)
        .context("jwt decode")
}

/// Generate a cryptographically-strong alphanumeric password.
///
/// Used by the first-boot bootstrap path to seed the initial admin's
/// password (then logged to stderr exactly once).
pub fn generate_password(len: usize) -> String {
    use rand::Rng;
    use rand::distributions::Alphanumeric;
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_succeeds_for_correct_password() {
        let phc = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &phc).unwrap());
    }

    #[test]
    fn verify_fails_for_wrong_password() {
        let phc = hash_password("correct horse battery staple").unwrap();
        assert!(!verify_password("wrong password", &phc).unwrap());
    }

    #[test]
    fn each_hash_uses_fresh_salt() {
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(a, b, "hashes must differ when salt is fresh");
    }

    #[test]
    fn jwt_round_trip_recovers_claims() {
        let secret = "test-secret-32-bytes-of-padding-zzzzz";
        let token = issue_token(42, 1, 2, "alice", secret, 3600).unwrap();
        let claims = verify_token(&token, secret).unwrap();
        assert_eq!(claims.sub, 42);
        assert_eq!(claims.customer_id, 1);
        assert_eq!(claims.role_id, 2);
        assert_eq!(claims.login, "alice");
        assert!(claims.exp > claims.iat);
    }

    #[test]
    fn jwt_rejects_tampered_signature() {
        let token = issue_token(1, 1, 1, "x", "secret-a", 60).unwrap();
        assert!(verify_token(&token, "secret-b").is_err());
    }

    #[test]
    fn jwt_rejects_expired_token() {
        // jsonwebtoken validation has a 60-second leeway by default; use
        // a generously-past expiry so we are unambiguously expired.
        let token = issue_token(1, 1, 1, "x", "secret", -3600).unwrap();
        assert!(verify_token(&token, "secret").is_err());
    }

    #[test]
    fn generated_password_is_correct_length_and_charset() {
        let p = generate_password(24);
        assert_eq!(p.len(), 24);
        assert!(p.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
