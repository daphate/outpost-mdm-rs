//! Outpost MDM HTTP server library.
//!
//! Exposed as a library so integration tests can build the same `Router`
//! the binary serves, without spinning up a real network listener.

pub mod app;
pub mod auth;
pub mod auth_extract;
pub mod bootstrap;
pub mod config;
pub mod db;
pub mod error;
pub mod page;
pub mod permission;
pub mod routes;
pub mod shutdown;
pub mod state;
