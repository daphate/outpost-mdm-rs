//! Password hashing primitives.
//!
//! After P16 (replacing JWT with DB-backed sessions in [`crate::session`])
//! this module is reduced to argon2id helpers — no more JWT machinery,
//! no more `Claims` struct, no more `KIND_*` constants (those moved to
//! [`crate::session`]).
//!
//! - **argon2id** is OWASP-recommended for password hashing. We use the
//!   defaults from the `argon2` crate, which are tuned for interactive
//!   auth.
//! - **`generate_password`** is used by the first-boot bootstrap and by
//!   the device-enrollment flow to produce alphanumeric secrets with
//!   ~120 bits of entropy at length 20.

use anyhow::{Result, anyhow};
use argon2::{
    Argon2, PasswordHasher, PasswordVerifier,
    password_hash::{PasswordHash, SaltString, rand_core::OsRng},
};

/// Argon2id-hash the password using a fresh salt. Returns the
/// PHC-encoded string suitable for direct storage in
/// `users.password_hash`.
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

/// Async wrapper around [`verify_password`]: runs the CPU-bound argon2 verify
/// (tens–hundreds of ms by design) on the blocking pool so it doesn't stall
/// the async runtime. Use on request hot-paths (login); the sync version is
/// fine for one-off startup/admin actions.
pub async fn verify_password_async(password: String, phc: String) -> Result<bool> {
    tokio::task::spawn_blocking(move || verify_password(&password, &phc))
        .await
        .map_err(|e| anyhow!("verify_password join: {e}"))?
}

/// Async wrapper around [`hash_password`] — runs argon2 hashing on the blocking
/// pool. Prefer for request paths that hash (e.g. batch recovery-code hashing).
pub async fn hash_password_async(password: String) -> Result<String> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|e| anyhow!("hash_password join: {e}"))?
}

/// Generate a cryptographically-strong alphanumeric password of `len`
/// characters. ~6 bits of entropy per character.
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
    fn generated_password_is_correct_length_and_charset() {
        let p = generate_password(24);
        assert_eq!(p.len(), 24);
        assert!(p.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
