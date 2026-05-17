//! Runtime configuration sourced from environment variables.
//!
//! Twelve-factor approach: every knob ships through `ENV`. On a 512 MB
//! Ubuntu droplet we deliberately avoid config-file parsers — plain
//! `std::env::var` plus a few defaults is enough and keeps the binary lean.

use anyhow::{Context, Result, bail};
use std::env;

/// Default JWT lifetime in seconds (24 hours).
pub const DEFAULT_JWT_TTL_SECS: i64 = 86_400;

/// Server configuration resolved at process start.
#[derive(Debug, Clone)]
pub struct Config {
    /// Socket address the HTTP server binds to. Default `0.0.0.0:8080`.
    pub bind_addr: String,
    /// On-disk path to the SQLite database file. Default `/var/lib/outpost/outpost.db`.
    pub db_path: String,
    /// `tracing_subscriber::EnvFilter` directive. Default `info`.
    pub log_level: String,
    /// Symmetric secret used for HS512 JWT signing. **Required**.
    ///
    /// In tests pre-populate via `Config::test_default`; production starts
    /// must set `JWT_SECRET` in the environment.
    pub jwt_secret: String,
    /// Session token TTL, seconds.
    pub jwt_ttl_secs: i64,
}

impl Config {
    /// Read configuration from environment variables.
    ///
    /// Returns an error if a required variable is unset or malformed.
    /// `JWT_SECRET` is the only required variable; everything else has a
    /// sensible default.
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
        })
    }

    /// Test-only defaults — fixed JWT secret + in-memory DB.
    pub fn test_default() -> Self {
        Self {
            bind_addr: "127.0.0.1:0".to_string(),
            db_path: ":memory:".to_string(),
            log_level: "warn".to_string(),
            jwt_secret: "test-secret-with-at-least-32-bytes-of-padding-yes".to_string(),
            jwt_ttl_secs: DEFAULT_JWT_TTL_SECS,
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
        // SAFETY: env mutation is unsafe in 2024 edition.
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
