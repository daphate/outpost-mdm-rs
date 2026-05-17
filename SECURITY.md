# Security Policy

## Supported versions

Outpost MDM is in **pre-1.0 development**. Only the current `main` branch
receives security fixes. Released container tags (`ghcr.io/daphate/outpost-mdm-rs:sha-*`)
are not back-patched; redeploy from the latest image to pick up fixes.

| Version       | Supported          |
| ------------- | ------------------ |
| `main` (HEAD) | ✅ |
| anything else | ❌ |

## Reporting a vulnerability

**Do not open public GitHub issues for security problems.**

Send a private report to
[security@frontier.capital](mailto:security@frontier.capital) (or to the
maintainer's email until that alias is staffed). Include:

1. Affected commit SHA or container tag.
2. A reproducer — minimal HTTP request, payload, or shell command that
   exercises the bug.
3. Observed vs. expected behaviour.
4. Your assessment of impact (data exposure, RCE, DoS, auth bypass…).
5. Whether you've discussed this with anyone else.

We aim to acknowledge within **72 hours**, ship a fix to `main` within
**14 days** for critical/high severity, and disclose publicly via a
CHANGELOG entry once a fix is deployed. We will credit you in the
release notes if you wish.

## What's in scope

- Authentication / authorisation bypass (`/api/v1/*`)
- Privilege escalation across role boundaries (viewer → operator → admin)
- Tenant isolation violations (cross-`customer_id` read or write)
- Token forgery / replay (JWT signing, signed-URL HMAC)
- SQL injection, path traversal, multipart bypass
- Insecure cryptographic defaults (argon2id parameters, JWT algorithm)
- Container escape from `cgr.dev/chainguard/static`
- Supply-chain advisories in pinned `Cargo.lock` (file
  [`cargo audit`](https://github.com/rustsec/rustsec) reports)

## Out of scope

- Denial-of-service via overwhelming a 1 vCPU / 512 MB droplet without
  authentication — this is a sizing concern, not a CVE. Place a rate-
  limiter (e.g. `nginx limit_req_zone`) in front per
  [`docs/DEPLOY.md`](docs/DEPLOY.md).
- Issues that require an attacker to already control the host OS, the
  `JWT_SECRET`, or a privileged session.
- Theoretical attacks against the underlying SQLite library that have
  not been demonstrated against this application.
- Best-practice nits without an exploit (e.g. "you should use a
  different cipher suite"). Open a regular issue with a benchmark.

## Cryptographic posture

| Concern             | Choice                                                                                          |
| ------------------- | ----------------------------------------------------------------------------------------------- |
| Password hashing    | argon2id (RustCrypto, default parameters tuned for interactive auth)                            |
| Session tokens      | **Opaque 256-bit random hex, stored as sha256 in `sessions` table** (DB leak ≠ token leak)      |
| Token revocation    | `UPDATE sessions SET revoked_at = now()` — takes effect on next request, no global rekey needed |
| Device tokens       | Same `sessions` table, `kind = "device"`, 90-day TTL                                            |
| Signed download URL | HMAC-SHA256 over `file_id\|expires\|nonce` with `APP_SECRET`, constant-time verify via `subtle` |
| TLS                 | Terminated by nginx + certbot per `docs/DEPLOY.md`; the binary itself speaks plain HTTP         |

JWT was replaced with DB-backed opaque sessions in v0.2.0 (Phase 16).
Rationale: a stolen device needs to be locked out _instantly_, not at
the next signing-key rotation. See [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
"Auth model" section for the lifecycle diagram.

## Hardening already enabled

- Static musl binary on `cgr.dev/chainguard/static`, runs as `USER nonroot`
- WAL journaling + `foreign_keys = ON`
- Body-size limit (`MAX_BODY_BYTES`, default 200 MiB)
- Per-request timeout (`REQUEST_TIMEOUT_SECS`, default 120 s)
- Response headers: `X-Content-Type-Options`, `X-Frame-Options: DENY`,
  `Referrer-Policy: no-referrer`, `Strict-Transport-Security`,
  `X-Robots-Tag: noindex`, `Permissions-Policy`
- Cross-tenant scoping enforced via `WHERE customer_id = ?` on every
  read and write
- Permission gates from `user_role_permissions` checked before each
  mutation
- Bootstrap admin password printed once to stderr and forces
  `must_change_password` on first login
- CI: `cargo deny check`, `cargo audit`, Trivy scan of every pushed
  container image
