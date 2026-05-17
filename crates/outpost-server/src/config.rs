//! Runtime configuration sourced from environment variables.
//!
//! The server is designed to be 12-factor: every knob ships through ENV.
//! On a 512 MB Ubuntu droplet we deliberately avoid config-file parsers
//! (no figment / no toml) — `std::env::var` plus a few defaults is enough
//! and keeps the binary lean.

use std::env;

/// Server configuration resolved at process start.
///
/// Use [`Config::from_env`] to read every knob from the environment, falling
/// back to defaults sensible for local development. Production deployments
/// override every field via the container env.
#[derive(Debug, Clone)]
pub struct Config {
    /// Socket address the HTTP server binds to. Default `0.0.0.0:8080`.
    pub bind_addr: String,
    /// On-disk path to the SQLite database file. Default `/var/lib/outpost/outpost.db`.
    pub db_path: String,
    /// `tracing_subscriber::EnvFilter` directive. Default `info`.
    pub log_level: String,
}

impl Config {
    /// Read configuration from environment variables, using defaults for any
    /// missing value. Never panics; never blocks.
    pub fn from_env() -> Self {
        Self {
            bind_addr: env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            db_path: env::var("DB_PATH")
                .unwrap_or_else(|_| "/var/lib/outpost/outpost.db".to_string()),
            log_level: env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
        }
    }
}

impl Default for Config {
    /// Defaults aimed at integration tests: bind to ephemeral local port,
    /// in-memory database, quiet logs.
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:0".to_string(),
            db_path: ":memory:".to_string(),
            log_level: "warn".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_test_friendly() {
        let cfg = Config::default();
        assert_eq!(cfg.bind_addr, "127.0.0.1:0");
        assert_eq!(cfg.db_path, ":memory:");
        assert_eq!(cfg.log_level, "warn");
    }

    #[test]
    fn from_env_falls_back_to_defaults_when_unset() {
        // SAFETY: removing env vars is safe in single-threaded test harness.
        // SAFETY: This is unsafe in Rust 2024 because env mutation is non-thread-safe.
        unsafe {
            env::remove_var("BIND_ADDR");
            env::remove_var("DB_PATH");
            env::remove_var("RUST_LOG");
        }
        let cfg = Config::from_env();
        assert_eq!(cfg.bind_addr, "0.0.0.0:8080");
        assert!(cfg.db_path.ends_with("outpost.db"));
        assert_eq!(cfg.log_level, "info");
    }
}
