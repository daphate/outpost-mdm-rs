//! Runtime configuration sourced from environment variables.

use anyhow::{Context, Result, bail};
use std::env;
use std::path::PathBuf;

/// Default session token TTL for `users` logins (24 h).
pub const DEFAULT_SESSION_TTL_SECS: i64 = 86_400;
pub const DEFAULT_MAX_BODY_BYTES: usize = 200 * 1024 * 1024;
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 120;
pub const DEFAULT_SECURE_COOKIES: bool = true;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: String,
    pub db_path: String,
    pub log_level: String,
    /// Symmetric secret used for HMAC-SHA256 on signed download URLs.
    /// (Session tokens are NOT signed with this — they're random and
    /// stored hashed in the `sessions` table.)
    pub app_secret: String,
    /// User session TTL in seconds. Devices use a longer TTL set at
    /// enrollment time (90 days).
    pub session_ttl_secs: i64,
    pub app_files_dir: PathBuf,
    pub max_body_bytes: usize,
    pub request_timeout_secs: u64,
    pub secure_cookies: bool,
    /// Cloud.ru read-only IAM creds для генерации presigned APK-URL'ов
    /// (см. `cloudru_signer.rs`). Если хоть одно из трёх полей `None` —
    /// сервер не рендерит APK QR на странице enrollment, только enrollment QR.
    pub cloudru_tenant_id: Option<String>,
    pub cloudru_key_id: Option<String>,
    pub cloudru_secret: Option<String>,
    /// Cloud.ru bucket с APK / моделями. Default `outpost`.
    pub cloudru_bucket: String,
    /// Object key для latest APK pointer. Default `apks/latest/app-debug.apk`.
    pub cloudru_apk_key: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        // Accept APP_SECRET (preferred) or the deprecated JWT_SECRET name.
        let app_secret = env::var("APP_SECRET")
            .or_else(|_| env::var("JWT_SECRET"))
            .map_err(|_| {
                anyhow::anyhow!(
                    "missing required environment variable APP_SECRET (or legacy JWT_SECRET)"
                )
            })?;
        if app_secret.len() < 32 {
            bail!(
                "APP_SECRET is too short ({} bytes); use at least 32 random bytes (`openssl rand -base64 48`)",
                app_secret.len()
            );
        }
        let session_ttl_secs = env::var("SESSION_TTL_SECS")
            .or_else(|_| env::var("JWT_TTL_SECS"))
            .ok()
            .map(|s| s.parse::<i64>().context("parse SESSION_TTL_SECS"))
            .transpose()?
            .unwrap_or(DEFAULT_SESSION_TTL_SECS);
        let max_body_bytes = env::var("MAX_BODY_BYTES")
            .ok()
            .map(|s| s.parse::<usize>().context("parse MAX_BODY_BYTES"))
            .transpose()?
            .unwrap_or(DEFAULT_MAX_BODY_BYTES);
        let request_timeout_secs = env::var("REQUEST_TIMEOUT_SECS")
            .ok()
            .map(|s| s.parse::<u64>().context("parse REQUEST_TIMEOUT_SECS"))
            .transpose()?
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS);
        let secure_cookies = env::var("SECURE_COOKIES")
            .ok()
            .map(|s| matches!(s.as_str(), "1" | "true" | "TRUE" | "yes"))
            .unwrap_or(DEFAULT_SECURE_COOKIES);

        // Cloud.ru creds — all-or-nothing. Если задано хоть одно но не все три,
        // лучше отказаться ещё на старте чем рендерить broken QR'ы в production.
        let cloudru_tenant_id = env::var("CLOUDRU_TENANT_ID").ok().filter(|s| !s.is_empty());
        let cloudru_key_id = env::var("CLOUDRU_KEY_ID").ok().filter(|s| !s.is_empty());
        let cloudru_secret = env::var("CLOUDRU_SECRET").ok().filter(|s| !s.is_empty());
        let partial = [
            cloudru_tenant_id.is_some(),
            cloudru_key_id.is_some(),
            cloudru_secret.is_some(),
        ];
        let some_count = partial.iter().filter(|b| **b).count();
        if some_count != 0 && some_count != 3 {
            bail!(
                "CLOUDRU_* env vars must be either ALL set or ALL absent; got tenant_id={} key_id={} secret={}",
                cloudru_tenant_id.is_some(),
                cloudru_key_id.is_some(),
                cloudru_secret.is_some(),
            );
        }
        let cloudru_bucket =
            env::var("CLOUDRU_BUCKET").unwrap_or_else(|_| "outpost".to_string());
        let cloudru_apk_key = env::var("CLOUDRU_APK_KEY")
            .unwrap_or_else(|_| "apks/latest/app-debug.apk".to_string());

        Ok(Self {
            bind_addr: env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            db_path: env::var("DB_PATH")
                .unwrap_or_else(|_| "/var/lib/outpost/outpost.db".to_string()),
            log_level: env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
            app_secret,
            session_ttl_secs,
            app_files_dir: env::var("APP_FILES_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/var/lib/outpost/files")),
            max_body_bytes,
            request_timeout_secs,
            secure_cookies,
            cloudru_tenant_id,
            cloudru_key_id,
            cloudru_secret,
            cloudru_bucket,
            cloudru_apk_key,
        })
    }

    pub fn test_default() -> Self {
        Self {
            bind_addr: "127.0.0.1:0".to_string(),
            db_path: ":memory:".to_string(),
            log_level: "warn".to_string(),
            app_secret: "test-secret-with-at-least-32-bytes-of-padding-yes".to_string(),
            session_ttl_secs: DEFAULT_SESSION_TTL_SECS,
            app_files_dir: std::env::temp_dir().join("outpost-test"),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            request_timeout_secs: DEFAULT_REQUEST_TIMEOUT_SECS,
            secure_cookies: false,
            cloudru_tenant_id: None,
            cloudru_key_id: None,
            cloudru_secret: None,
            cloudru_bucket: "outpost".to_string(),
            cloudru_apk_key: "apks/latest/app-debug.apk".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_has_long_enough_secret() {
        assert!(Config::test_default().app_secret.len() >= 32);
    }

    #[test]
    fn from_env_requires_app_secret() {
        unsafe {
            env::remove_var("APP_SECRET");
            env::remove_var("JWT_SECRET");
        }
        assert!(Config::from_env().is_err());
    }

    #[test]
    fn from_env_rejects_short_secret() {
        unsafe {
            env::remove_var("JWT_SECRET");
            env::set_var("APP_SECRET", "too-short");
        }
        assert!(Config::from_env().is_err());
        unsafe {
            env::remove_var("APP_SECRET");
        }
    }

    #[test]
    fn from_env_accepts_legacy_jwt_secret_name() {
        unsafe {
            env::remove_var("APP_SECRET");
            env::set_var(
                "JWT_SECRET",
                "long-enough-legacy-secret-for-backwards-compat",
            );
        }
        let cfg = Config::from_env().expect("legacy JWT_SECRET must still be accepted");
        assert!(cfg.app_secret.len() >= 32);
        unsafe {
            env::remove_var("JWT_SECRET");
        }
    }

    #[test]
    fn defaults_are_sensible() {
        let cfg = Config::test_default();
        assert!(cfg.max_body_bytes >= 100 * 1024 * 1024);
        assert!(cfg.request_timeout_secs >= 30);
    }
}
