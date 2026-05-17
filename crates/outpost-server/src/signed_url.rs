//! HMAC-SHA256 signed download URLs.
//!
//! Devices receive short-lived signed tokens that prove the server
//! authorised the download. Token format:
//!
//! ```text
//! v1.{file_id}.{expires_unix}.{hex_hmac_sha256(file_id|expires|nonce)}
//! ```
//!
//! `expires_unix` is checked against wall-clock at verify time. The
//! HMAC key comes from `AppState::jwt_secret` (reused — single rotation
//! point for all server-side signing).

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Produce a signed token authorising download of `file_id` for `ttl_secs`
/// seconds. `nonce` defends against replay across token rotations.
pub fn sign(file_id: i64, ttl_secs: i64, secret: &str) -> String {
    let expires = Utc::now().timestamp() + ttl_secs;
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let payload = format!("{file_id}.{expires}.{nonce}");
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
    mac.update(payload.as_bytes());
    let tag = hex::encode(mac.finalize().into_bytes());
    format!("v1.{file_id}.{expires}.{nonce}.{tag}")
}

/// Recovered identity of a valid signed URL.
#[derive(Debug, Clone)]
pub struct Verified {
    pub file_id: i64,
}

/// Verify a token. Returns the inner `file_id` if signature + expiry pass.
pub fn verify(token: &str, secret: &str) -> Result<Verified> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 5 || parts[0] != "v1" {
        bail!("malformed token");
    }
    let file_id: i64 = parts[1].parse().context("file_id parse")?;
    let expires: i64 = parts[2].parse().context("expires parse")?;
    let nonce = parts[3];
    let tag = parts[4];

    if Utc::now().timestamp() > expires {
        bail!("token expired");
    }

    let payload = format!("{file_id}.{expires}.{nonce}");
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key");
    mac.update(payload.as_bytes());
    let expected = mac.finalize().into_bytes();
    let provided = hex::decode(tag).map_err(|_| anyhow!("tag not hex"))?;
    if expected.ct_eq(&provided).unwrap_u8() != 1 {
        bail!("signature mismatch");
    }

    Ok(Verified { file_id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_then_verify_succeeds_within_ttl() {
        let token = sign(42, 3600, "secret-key");
        let v = verify(&token, "secret-key").unwrap();
        assert_eq!(v.file_id, 42);
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let token = sign(1, 60, "key-a");
        assert!(verify(&token, "key-b").is_err());
    }

    #[test]
    fn verify_rejects_expired_token() {
        let token = sign(1, -1, "k");
        assert!(verify(&token, "k").is_err());
    }

    #[test]
    fn verify_rejects_tampered_file_id() {
        let token = sign(1, 60, "k");
        // Hand-mangle the file id in place
        let mut parts: Vec<&str> = token.split('.').collect();
        parts[1] = "999";
        let tampered = parts.join(".");
        assert!(verify(&tampered, "k").is_err());
    }

    #[test]
    fn verify_rejects_garbage() {
        assert!(verify("not-a-token", "k").is_err());
        assert!(verify("v1.x.y.z", "k").is_err());
    }
}
