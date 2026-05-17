//! RFC 6238 TOTP — backs admin 2FA on the web login.
//!
//! Standard parameters: SHA-1 HMAC, 6-digit code, 30-second time step,
//! ±1-step (±30 s) clock-skew tolerance. These are the values
//! Google Authenticator / Authy / 1Password / Bitwarden default to,
//! so secrets enrolled here work in any of those apps.
//!
//! Secret format: 20 random bytes, base32-encoded (no `=` padding) for the
//! `otpauth://` URI. The receiver stores the base32 string verbatim — when
//! verifying we decode it once per attempt; cost is negligible.

use base32::Alphabet;
use rand::RngCore;

const DIGITS: u32 = 6;
const STEP_SECONDS: u64 = 30;
const SKEW_STEPS: i64 = 1;

/// Generate a fresh 160-bit (20-byte) secret, base32-encoded with the
/// RFC 3548 alphabet and **no** padding (per the otpauth URI convention).
pub fn generate_secret() -> String {
    let mut bytes = [0u8; 20];
    rand::thread_rng().fill_bytes(&mut bytes);
    base32::encode(Alphabet::Rfc4648 { padding: false }, &bytes)
}

/// Build an `otpauth://totp/...` URI for QR encoding. `issuer` shows up
/// as the account-label prefix in authenticator apps.
pub fn otpauth_uri(secret: &str, issuer: &str, account: &str) -> String {
    // Both issuer and account need URL-percent-encoding for spaces / ':' etc.
    let issuer_enc = percent_encode(issuer);
    let account_enc = percent_encode(account);
    let secret_enc = percent_encode(secret);
    format!(
        "otpauth://totp/{issuer_enc}:{account_enc}?secret={secret_enc}&issuer={issuer_enc}&digits={DIGITS}&period={STEP_SECONDS}&algorithm=SHA1"
    )
}

/// Verify a 6-digit user-supplied code against the secret, allowing ±1
/// 30-second step of clock skew (so 90 seconds of valid window total).
///
/// Returns `true` if the code matches the current, previous, or next step.
pub fn verify(secret_b32: &str, code: &str) -> bool {
    if code.len() != DIGITS as usize || !code.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let Some(secret) = base32::decode(Alphabet::Rfc4648 { padding: false }, secret_b32) else {
        return false;
    };
    let Ok(expected_num) = code.parse::<u32>() else {
        return false;
    };
    let now = match std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
    {
        Ok(d) => d.as_secs(),
        Err(_) => return false,
    };
    let current_step = (now / STEP_SECONDS) as i64;
    for offset in -SKEW_STEPS..=SKEW_STEPS {
        let step = (current_step + offset) as u64;
        let candidate = compute_code(&secret, step);
        // Constant-time compare on the 4-byte numeric value.
        if subtle::ConstantTimeEq::ct_eq(
            &candidate.to_le_bytes()[..],
            &expected_num.to_le_bytes()[..],
        )
        .into()
        {
            return true;
        }
    }
    false
}

fn compute_code(secret: &[u8], step: u64) -> u32 {
    use totp_lite::{totp_custom, Sha1};
    // totp-lite returns the formatted N-digit string. Re-parse to u32
    // for constant-time compare. This is fine — `totp_custom` is the
    // standard RFC 6238 path.
    let s = totp_custom::<Sha1>(STEP_SECONDS, DIGITS, secret, step * STEP_SECONDS);
    s.parse::<u32>().unwrap_or(u32::MAX)
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_decodes_to_20_bytes() {
        let s = generate_secret();
        let bytes =
            base32::decode(Alphabet::Rfc4648 { padding: false }, &s).expect("decode");
        assert_eq!(bytes.len(), 20);
    }

    #[test]
    fn otpauth_uri_includes_issuer_and_secret() {
        let s = "JBSWY3DPEHPK3PXP";
        let uri = otpauth_uri(s, "Outpost MDM", "admin");
        assert!(uri.contains("secret=JBSWY3DPEHPK3PXP"));
        assert!(uri.contains("issuer=Outpost%20MDM"));
        assert!(uri.contains("digits=6"));
        assert!(uri.contains("period=30"));
    }

    #[test]
    fn verify_rejects_garbage() {
        let s = generate_secret();
        assert!(!verify(&s, "abc123"));
        assert!(!verify(&s, "12345"));
        assert!(!verify(&s, "1234567"));
        assert!(!verify(&s, ""));
    }

    #[test]
    fn verify_accepts_current_step_code() {
        use totp_lite::{totp_custom, Sha1};
        let s = generate_secret();
        let secret = base32::decode(Alphabet::Rfc4648 { padding: false }, &s).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let code = totp_custom::<Sha1>(30, 6, &secret, now);
        assert!(verify(&s, &code), "current-step code must verify");
    }

    #[test]
    fn verify_accepts_previous_step_code_for_clock_skew() {
        use totp_lite::{totp_custom, Sha1};
        let s = generate_secret();
        let secret = base32::decode(Alphabet::Rfc4648 { padding: false }, &s).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let prev_step_time = now.saturating_sub(30);
        let code = totp_custom::<Sha1>(30, 6, &secret, prev_step_time);
        assert!(verify(&s, &code), "previous-step code must verify");
    }
}
