//! Outpost MDM HTTP server library.
//!
//! Exposed as a library so integration tests can build the same `Router`
//! the binary serves, without spinning up a real network listener.
//!
//! Module layout follows the phases defined in the project plan:
//! - [`config`]       — environment-driven typed configuration (P1)
//! - [`shutdown`]     — graceful shutdown signal handling (P1)
//! - [`db`]           — SQLite connection pool with WAL pragmas (P2)
//! - [`state`]        — `AppState` shared across handlers (P2/P3)
//! - [`auth`]         — argon2id password hashing and HS512 JWT (P3)
//! - [`auth_extract`] — `AuthUser` HTTP extractor (P3)
//! - [`bootstrap`]    — first-boot bootstrap routines (P3)
//! - [`error`]        — unified HTTP error type (P3)
//! - [`routes`]       — REST handler modules (P3, extended each phase)
//! - [`app`]          — top-level `Router` factory (P1 baseline)

pub mod app;
pub mod auth;
pub mod auth_extract;
pub mod bootstrap;
pub mod config;
pub mod db;
pub mod error;
pub mod routes;
pub mod shutdown;
pub mod state;
