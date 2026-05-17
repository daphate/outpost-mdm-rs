//! Outpost MDM HTTP server library.
//!
//! Exposed as a library so integration tests can build the same `Router` the
//! binary serves, without spinning up a real network listener.
//!
//! Module layout follows the phases defined in the project plan:
//! - [`config`]  — environment-driven typed configuration (P1)
//! - [`app`]     — `Router` factory + handlers (P1 baseline, extended each phase)
//! - [`shutdown`] — graceful shutdown signal handling (P1)

pub mod app;
pub mod config;
pub mod shutdown;
