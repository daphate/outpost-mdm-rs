//! HTTP route modules.
//!
//! One sub-module per logical resource family. Each module owns its
//! handlers + request/response DTOs and exposes a `router()` returning an
//! `axum::Router<AppState>` to be merged into the top-level builder.

pub mod auth;
