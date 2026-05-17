<!--
Thanks for the contribution. Fill in what's relevant; delete what isn't.
See CONTRIBUTING.md for the longer-form expectations.
-->

## Summary

<!-- One paragraph explaining the change. Why, then what. -->

## Verification

- [ ] `cargo fmt --all -- --check` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` passes (N total, M new)
- [ ] New endpoints have integration tests under `tests/`
- [ ] New permission strings seeded in `0002_users_auth.sql`
- [ ] `CHANGELOG.md` updated under `[Unreleased]`
- [ ] `docs/DEPLOY.md` updated if any env var or deploy step changed

## Notes for reviewers

<!-- Anything subtle: timing assumptions, schema impact, security-sensitive paths. -->
