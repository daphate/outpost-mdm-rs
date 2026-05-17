//! Runtime configuration sourced from environment variables.

use anyhow::{Context, Result, bail};
use std::env;
use std::path::PathBuf;

pub const DEFAULT_JWT_TTL_SECS: i64 = 86_400;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_addr: String,
    pub db_path: String,
    pub log_level: String,
    pub jwt_secret: String,
    pub jwt_ttl_secs: i64,
    pub app_files_dir: PathBuf,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let jwt_secret = env::var("JWT_SECRET")
            .map_err(|_| anyhow::anyhow!("missing required environment variable JWT_SECRET"))?;
        if jwt_secret.len() < 32 {
            bail!(
                "JWT_SECRET is too short ({} bytes); use at least 32 random bytes (`openssl rand -base64 48`)",
                jwt_secret.len()
            );
        }
        let jwt_ttl_secs = env::var("JWT_TTL_SECS")
            .ok()
            .map(|s| s.parse::<i64>().context("parse JWT_TTL_SECS"))
            .transpose()?
            .unwrap_or(DEFAULT_JWT_TTL_SECS);

        Ok(Self {
            bind_addr: env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            db_path: env::var("DB_PATH")
                .unwrap_or_else(|_| "/var/lib/outpost/outpost.db".to_string()),
            log_level: env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
            jwt_secret,
            jwt_ttl_secs,
            app_files_dir: env::var("APP_FILES_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/var/lib/outpost/files")),
        })
    }

    pub fn test_default() -> Self {
        Self {
            bind_addr: "127.0.0.1:0".to_string(),
            db_path: ":memory:".to_string(),
            log_level: "warn".to_string(),
            jwt_secret: "test-secret-with-at-least-32-bytes-of-padding-yes".to_string(),
            jwt_ttl_secs: DEFAULT_JWT_TTL_SECS,
            app_files_dir: std::env::temp_dir().join("outpost-test"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_has_long_enough_secret() {
        assert!(Config::test_default().jwt_secret.len() >= 32);
    }

    #[test]
    fn from_env_requires_jwt_secret() {
        unsafe {
            env::remove_var("JWT_SECRET");
        }
        assert!(Config::from_env().is_err());
    }

    #[test]
    fn from_env_rejects_short_jwt_secret() {
        unsafe {
            env::set_var("JWT_SECRET", "too-short");
        }
        assert!(Config::from_env().is_err());
        unsafe {
            env::remove_var("JWT_SECRET");
        }
    }
}
