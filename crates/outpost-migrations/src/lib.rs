//! Outpost MDM SQLite migrations. Populated in P2 — SQL files live under `migrations/`.
//!
//! The migrations are applied at server startup via `sqlx::migrate!()` macro from
//! outpost-server's main; this crate exists to keep schema artifacts and migration
//! tests co-located.
