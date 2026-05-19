//! Cargo rerun-if-changed wiring for migrations/.
//!
//! `sqlx::migrate!("./migrations")` is a compile-time macro that embeds the
//! contents of every `migrations/*.sql` file into the binary. Cargo's
//! default `rerun-if-changed` policy only watches the .rs sources, so
//! adding or editing a SQL file alone does NOT trigger a rebuild —
//! `lib.rs` is treated as unchanged, the old MIGRATOR is reused, and the
//! freshly added migration is silently absent at runtime.
//!
//! Prior to this build.rs, that bit us on v0.18.8 deploy: migration 0019
//! shipped to the host filesystem (rsync'd into WSL) but the compiled
//! binary still contained only migrations 1..18, so the
//! `customers.default_configuration_id` column never appeared.
//!
//! Solution: emit explicit `cargo:rerun-if-changed=migrations` so any
//! mtime change inside that directory forces a rebuild of this crate
//! (and transitively `outpost-server`).
fn main() {
    println!("cargo:rerun-if-changed=migrations");
}
