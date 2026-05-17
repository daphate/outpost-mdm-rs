//! Outpost MDM HTTP server library.
//!
//! Exposed as a library so integration tests can build the same `Router`
//! the binary serves, without spinning up a real network listener.
//!
//! Module layout follows the phases defined in the project plan:
//! - [`config`]   — environment-driven typed configuration (P1)
//! - [`shutdown`] — graceful shutdown signal handling (P1)
//! - [`db`]       — SQLite connection pool with WAL pragmas (P2)
//! - [`state`]    — `AppState` shared across handlers (P2)
//! - [`app`]      — `Router` factory + handlers (P1 baseline, extended each phase)

pub mod app;
pub mod config;
pub mod db;
pub mod shutdown;
pub mod state;
