# Contributing to Outpost MDM

Welcome. This is a Frontier Capital project; until it has more than one
maintainer, expect a pragmatic, low-ceremony workflow.

## Development setup

1. Install the latest stable Rust toolchain:
   ```sh
   rustup toolchain install stable
   rustup default stable
   rustup component add rustfmt clippy
   ```
   The repo also ships a `rust-toolchain.toml` so `cargo` will pick the
   right channel automatically.

2. Run the local pre-flight checks before pushing:
   ```sh
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   cargo deny check        # optional but matches CI
   cargo audit             # optional but matches CI
   ```

3. Run the server locally:
   ```sh
   export JWT_SECRET=$(openssl rand -base64 48)
   cargo run -p outpost-server
   ```
   Capture the bootstrap admin password from stderr on first launch.

## Repository layout

See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the module map.
The short version: each REST resource lives in
`crates/outpost-server/src/routes/{name}.rs`, paired with a
`crates/outpost-server/tests/{name}.rs` integration test file. The
shared test harness is `tests/common/mod.rs` — import it via
`mod common;` from any new test file.

## Coding conventions

- **`rustfmt` is law.** CI runs `cargo fmt --all -- --check`. Use the
  defaults; we don't customise `rustfmt.toml`.
- **`clippy -D warnings` is law.** Don't `#[allow(...)]` to silence
  clippy unless there's a concrete reason in a comment.
- **No `unsafe`** unless it's wrapping a `std::env` mutation in a unit
  test — that's the only unsafe block in the codebase today and we'd
  like to keep it that way.
- **Errors** flow through `crate::error::ApiError`. Add a new variant
  with a stable `code`/`http_status` mapping; don't bypass with raw
  `(StatusCode, String)` tuples.
- **Permissions** flow through `crate::permission::require_permission`.
  Add the named permission to `0002_users_auth.sql` if it's new.
- **Tenant scoping** — every read/write that selects user-owned rows
  must filter on `customer_id = ?`. Skipping this is a CVE-class bug.
- **No invented numbers.** Don't write "we expect ~50 MB RSS" without
  measuring it; either measure and cite, or omit. (See the Reality
  Check notes in `docs/DEPLOY.md`.)
- **Doc comments**: module-level (`//!`) at the top of every file
  covers intent. Item-level (`///`) only when the WHY is non-obvious;
  don't restate the type signature.

## Testing conventions

- **Unit tests** live in `mod tests` inside each module under
  `#[cfg(test)]`. Use `tokio::test` for async.
- **Integration tests** live in `crates/outpost-server/tests/{name}.rs`
  and import the shared `mod common;`. Each integration test creates a
  fresh `TestApp` so they're independent.
- **Schema tests** live in `crates/outpost-migrations/tests/migrate.rs`
  and exercise the migration set against an in-memory SQLite.
- **Don't depend on test ordering.** Tests run in parallel.
- **No flaky retries.** If a test is flaky, fix the root cause (timing
  assumption, leaky env var, shared global) rather than `sleep(...)`-ing
  longer.

## Migration conventions

- Migrations are **append-only**. Never edit a shipped file under
  `crates/outpost-migrations/migrations/` — add a new numbered file.
- The file name pattern is `NNNN_topic.sql`, applied in lexicographic
  order. Match the existing four-digit zero-padded numbering.
- Use SQLite syntax (no `SERIAL`, no `JSONB`, no `TIMESTAMP WITH TIME
  ZONE`). `INTEGER PRIMARY KEY AUTOINCREMENT`, `TEXT` for ISO-8601
  timestamps, `BLOB` for binary.
- Foreign keys: every `REFERENCES` should declare `ON DELETE` policy
  explicitly (CASCADE / SET NULL / RESTRICT).
- Indexes: every FK column gets an index unless there's a deliberate
  reason not to.

## Commit messages

We use the multi-line commit style visible in `git log --oneline`:

```
P14: short imperative title (≤ 72 chars)

Optional 1-3 paragraph body explaining the change. Bullet lists are
welcome; wrap text at ~72 chars.

Verified at commit time: cargo fmt --check clean, cargo clippy
--workspace -D warnings clean, cargo test --workspace: N passing.

Co-Authored-By: ...
```

`Co-Authored-By` lines are encouraged when AI tools contribute.

## Pull-request checklist

- [ ] `cargo fmt --all -- --check` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` passes — both new and existing tests
- [ ] New endpoints have a corresponding integration test in `tests/`
- [ ] New permission strings are seeded in `0002_users_auth.sql`
- [ ] `CHANGELOG.md` updated under `[Unreleased]`
- [ ] Doc comments on any new public function / struct / module
- [ ] If config changed, `Config::from_env`, `test_default`, and
      `docs/DEPLOY.md` env-var table are all updated

## Reporting security issues

See [`SECURITY.md`](SECURITY.md) — do not open public issues for
security problems.
