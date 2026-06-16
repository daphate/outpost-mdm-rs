//! Outpost-Android APK upstream watcher.
//!
//! Poll-based discovery: каждые `apk.watch.interval_secs` (default 900s
//! = 15 min) хитим upstream manifest (по умолчанию `apks/outpost-latest-debug.version.txt`
//! на публичном R2 mirror'е — b44 schema 2026-05-18) и регистрируем
//! новые сборки в `application_versions`. Без auto-push на устройства —
//! admin сам решает когда катить.
//!
//! ## b44 schema fallback
//!
//! Если новая (b44) schema возвращает 404 на manifest URL — watcher
//! автоматически делает второй запрос на legacy URL `apks/latest/version.txt`.
//! Это позволяет переключить deployment'ы постепенно: AR Hud команда
//! заливает APK по новой schema, старые ещё работают через legacy.
//!
//! ## Contract
//!
//! Watcher ожидает upstream manifest вида:
//!
//! ```text
//! tag=rc42-b33
//! sha256=5eb1ff4cea252eb8...
//! size=177187065
//! version_code=174        (optional; если отсутствует — derive from tag)
//! version_name=1.0.0-rc42-b33  (optional; default = tag)
//! ```
//!
//! Простой key=value формат специально, чтобы не тащить serde_json на
//! upstream side и не плодить версионирование схемы. Если в будущем
//! понадобится richer payload — переключимся на `apks/latest/manifest.json`
//! и parser scheme-version-fallback.
//!
//! ## Что watcher НЕ делает
//!
//! - **Не скачивает APK локально** — `file_path = ''` для watcher-rows.
//!   `source_url` указывает на upstream blob. Admin UI кладёт link на этот
//!   URL для скачивания.
//! - **Не пушит на устройства** — `application_assignments` остаются под
//!   контролем админа.
//! - **Не валидирует подпись APK** — sha256 fingerprint single source of
//!   truth; integrity-проверки на стороне устройства.
//!
//! ## Configuration (settings table)
//!
//! | Key | Default |
//! |---|---|
//! | `apk.watch.url` | `https://pub-ef0219f0ecf84d0e8e44497adfe9ceb0.r2.dev/apks/latest/version.txt` |
//! | `apk.watch.interval_secs` | `900` (15 min, clamp 60..3600) |
//! | `apk.watch.package_name` | `ru.tacticalar.outpost` |
//! | `apk.watch.customer_id` | `1` (default tenant) |

use anyhow::{Context, Result, anyhow};
use sqlx::SqlitePool;
use std::time::Duration;
use tokio::time;

/// Default cadence between upstream checks.
pub const DEFAULT_INTERVAL_SECS: u64 = 900;
/// Hard floor on interval — protects upstream from accidental DoS.
const MIN_INTERVAL_SECS: u64 = 60;
/// Hard ceiling — beyond this, watcher feels broken to operator.
const MAX_INTERVAL_SECS: u64 = 3600;

/// Default upstream URL (b44 schema 2026-05-18). R2 anonymous mirror is the
/// primary source; admin can override via `settings.apk.watch.url` to point
/// at Cloud.ru / a custom CDN / `gh api repos/.../releases/latest`.
pub const DEFAULT_UPSTREAM_URL: &str =
    "https://pub-ef0219f0ecf84d0e8e44497adfe9ceb0.r2.dev/apks/outpost-latest-debug.version.txt";

/// Legacy upstream URL fallback (pre-b44 schema). При 404 на новый URL
/// watcher retry'ит на старый, чтобы оба deployment'а сосуществовали
/// пока AR Hud команда переключается на новую schema.
pub const LEGACY_UPSTREAM_URL: &str =
    "https://pub-ef0219f0ecf84d0e8e44497adfe9ceb0.r2.dev/apks/latest/version.txt";

/// Default Android package name for the watched application.
pub const DEFAULT_PACKAGE_NAME: &str = "ru.tacticalar.outpost";

/// Default tenant for watcher-tracked rows.
const DEFAULT_CUSTOMER_ID: i64 = 1;

/// Spawn the watcher. Returns the join handle for graceful drain.
pub fn spawn(pool: SqlitePool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let interval = read_interval_secs(&pool).await;
        tracing::info!(
            interval_secs = interval,
            "apk watcher started (mode: discovery-only, no auto-push)"
        );
        // Skip the first immediate tick — give the rest of the app a moment.
        let mut ticker = time::interval(Duration::from_secs(interval));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if let Err(e) = tick_once(&pool).await {
                tracing::warn!(error = ?e, "apk watcher tick failed");
            }
        }
    })
}

async fn read_setting(pool: &SqlitePool, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT json_extract(value_json, '$') FROM settings WHERE key = ?",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .flatten()
}

async fn read_interval_secs(pool: &SqlitePool) -> u64 {
    read_setting(pool, "apk.watch.interval_secs")
        .await
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| (MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(n))
        .unwrap_or(DEFAULT_INTERVAL_SECS)
}

async fn read_upstream_url(pool: &SqlitePool) -> String {
    read_setting(pool, "apk.watch.url")
        .await
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_UPSTREAM_URL.to_string())
}

async fn read_package_name(pool: &SqlitePool) -> String {
    read_setting(pool, "apk.watch.package_name")
        .await
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_PACKAGE_NAME.to_string())
}

async fn read_customer_id(pool: &SqlitePool) -> i64 {
    read_setting(pool, "apk.watch.customer_id")
        .await
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(DEFAULT_CUSTOMER_ID)
}

/// One pass of the watcher loop. Idempotent: re-running with the same upstream
/// state is a no-op (dedupe by `application_id + sha256`).
async fn tick_once(pool: &SqlitePool) -> Result<()> {
    let primary_url = read_upstream_url(pool).await;
    let pkg = read_package_name(pool).await;
    let customer_id = read_customer_id(pool).await;
    tracing::debug!(url = %primary_url, pkg = %pkg, "apk watcher: poll upstream");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("build reqwest client")?;

    // b44 schema: пробуем сначала новый URL, при 404 fallback на legacy.
    // Это позволяет server'у и AR Hud-builder'у переключиться независимо.
    let (effective_url, body) = match fetch_manifest(&client, &primary_url).await {
        Ok(b) => (primary_url.clone(), b),
        Err(e) if is_404(&e) && primary_url != LEGACY_UPSTREAM_URL => {
            tracing::info!(
                primary = %primary_url,
                legacy = LEGACY_UPSTREAM_URL,
                "apk watcher: primary URL 404, fallback to legacy schema"
            );
            let b = fetch_manifest(&client, LEGACY_UPSTREAM_URL)
                .await
                .with_context(|| {
                    format!(
                        "both primary ({primary_url}) and legacy ({LEGACY_UPSTREAM_URL}) failed"
                    )
                })?;
            (LEGACY_UPSTREAM_URL.to_string(), b)
        }
        Err(e) => return Err(e),
    };
    let url = effective_url;

    let manifest =
        parse_version_txt(&body).with_context(|| format!("parse version.txt from {url}"))?;
    let sha = manifest.sha256.clone();

    // Find or create the application row. Watcher is single-tenant by
    // default (customer_id=1); если в будущем хочется per-tenant cohorts —
    // создадим `apk.watch.customer_id_per_url` mapping.
    let app_id = upsert_application(pool, customer_id, &pkg).await?;

    // Deduplicate by sha256 — if we've seen this exact byte stream before,
    // even via a different tag (rebuild с теми же source), skip.
    let already: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM application_versions WHERE application_id = ? AND sha256 = ?",
    )
    .bind(app_id)
    .bind(&sha)
    .fetch_optional(pool)
    .await?;
    if already.is_some() {
        tracing::debug!(sha = %sha, "apk watcher: sha already known, skip");
        return Ok(());
    }

    // Derive the integer version_code if upstream didn't supply one.
    let version_code = manifest
        .version_code
        .or_else(|| version_code_from_tag(&manifest.tag))
        .ok_or_else(|| anyhow!("upstream did not supply version_code and tag is not parsable"))?;

    // Mark previous active row inactive — only one is_active=1 per
    // application_id is the convention (admin UI sorts by latest version_code
    // for "current", but is_active is the marker for device push policy).
    sqlx::query(
        "UPDATE application_versions SET is_active = 0 WHERE application_id = ? AND is_active = 1",
    )
    .bind(app_id)
    .execute(pool)
    .await?;

    let version_name = manifest
        .version_name
        .clone()
        .unwrap_or_else(|| manifest.tag.clone());
    let blob_url = manifest.upstream_blob_url(&url);
    let inserted_id: i64 = sqlx::query_scalar(
        "INSERT INTO application_versions \
           (application_id, version_code, version_name, file_path, file_size_bytes, sha256, \
            source_url, is_active, notes, uploaded_by, uploaded_at) \
         VALUES (?, ?, ?, '', ?, ?, ?, 1, ?, NULL, datetime('now')) \
         RETURNING id",
    )
    .bind(app_id)
    .bind(version_code)
    .bind(version_name)
    .bind(manifest.size_bytes)
    .bind(&sha)
    .bind(&blob_url)
    .bind(format!(
        "Auto-discovered by apk watcher; tag={}",
        manifest.tag
    ))
    .fetch_one(pool)
    .await?;

    tracing::info!(
        application_id = app_id,
        application_version_id = inserted_id,
        tag = %manifest.tag,
        version_code = version_code,
        sha256 = %sha,
        size_bytes = manifest.size_bytes,
        "apk watcher: registered new version"
    );
    Ok(())
}

/// HTTP GET текстового manifest'а; возвращает body string или anyhow Err
/// (с context'ом про status code). `is_404` ниже определяет worth fallback'а.
async fn fetch_manifest(client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = client
        .get(url)
        .header(
            "User-Agent",
            concat!("outpost-mdm-rs/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("non-2xx ({status}) from {url}"));
    }
    resp.text().await.context("read body")
}

/// Грубая проверка «это 404». reqwest::Error не даёт удобного API для status,
/// поэтому ловим в строке anyhow context'а. Достаточно для control-flow:
/// false-positive (другой 4xx) тоже триггернёт fallback — это safe,
/// результат legacy URL либо успешен, либо тоже fail, итог тот же.
fn is_404(err: &anyhow::Error) -> bool {
    let s = format!("{err:#}");
    s.contains("404") || s.contains("Not Found")
}

async fn upsert_application(pool: &SqlitePool, customer_id: i64, package: &str) -> Result<i64> {
    if let Some(id) = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM applications WHERE customer_id = ? AND package_name = ?",
    )
    .bind(customer_id)
    .bind(package)
    .fetch_optional(pool)
    .await?
    {
        return Ok(id);
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO applications (customer_id, package_name, display_name, description, kind) \
         VALUES (?, ?, ?, ?, 'apk') RETURNING id",
    )
    .bind(customer_id)
    .bind(package)
    .bind("Outpost-Android")
    .bind("Auto-managed by apk watcher. Releases discovered from upstream R2 / Cloud.ru / GH manifest.")
    .fetch_one(pool)
    .await?;
    tracing::info!(application_id = id, package = %package, "apk watcher: created application row");
    Ok(id)
}

/// Parsed key=value manifest body.
#[derive(Debug, Clone)]
pub struct UpstreamManifest {
    pub tag: String,
    pub sha256: String,
    pub size_bytes: i64,
    pub version_code: Option<i64>,
    pub version_name: Option<String>,
}

impl UpstreamManifest {
    /// Derive the canonical APK download URL relative to the manifest URL.
    ///
    /// Поддерживает обе схемы конвенции `tools/upload_apk.py`:
    ///   - **b44 (current)**: `apks/outpost-latest-debug.version.txt` →
    ///     `apks/outpost-<tag>-debug.apk` (и pointer `apks/outpost-latest-debug.apk`
    ///     указывает на latest).
    ///   - **Legacy (pre-b44)**: `apks/latest/version.txt` →
    ///     `apks/<tag>/app-debug.apk`.
    ///
    /// Detection — по NEEDLE-substring'у в manifest_url; первый match выигрывает.
    pub fn upstream_blob_url(&self, manifest_url: &str) -> String {
        const NEW_NEEDLE: &str = "/apks/outpost-latest-debug.version.txt";
        const LEGACY_NEEDLE: &str = "/apks/latest/version.txt";

        if let Some(idx) = manifest_url.rfind(NEW_NEEDLE) {
            let base = &manifest_url[..idx];
            return format!(
                "{base}/apks/outpost-{tag}-debug.apk",
                base = base,
                tag = self.tag
            );
        }
        if let Some(idx) = manifest_url.rfind(LEGACY_NEEDLE) {
            let base = &manifest_url[..idx];
            return format!(
                "{base}/apks/{tag}/app-debug.apk",
                base = base,
                tag = self.tag
            );
        }
        // Best-effort fallback: same dir as manifest, tag suffix.
        let dir = manifest_url
            .rsplit_once('/')
            .map(|(d, _)| d)
            .unwrap_or(manifest_url);
        format!("{dir}/{tag}-app-debug.apk", dir = dir, tag = self.tag)
    }
}

/// Parse `key=value` lines, tolerant of comments / blank lines.
pub fn parse_version_txt(body: &str) -> Result<UpstreamManifest> {
    let mut tag: Option<String> = None;
    let mut sha: Option<String> = None;
    let mut size: Option<i64> = None;
    let mut version_code: Option<i64> = None;
    let mut version_name: Option<String> = None;

    for raw in body.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim();
        let value = v.trim();
        match key {
            "tag" => tag = Some(value.to_string()),
            "sha256" => sha = Some(value.to_ascii_lowercase()),
            "size" | "size_bytes" => size = value.parse::<i64>().ok(),
            "version_code" | "versionCode" => version_code = value.parse::<i64>().ok(),
            "version_name" | "versionName" => version_name = Some(value.to_string()),
            _ => { /* ignore unknown keys for forward-compat */ }
        }
    }

    Ok(UpstreamManifest {
        tag: tag.ok_or_else(|| anyhow!("missing tag"))?,
        sha256: sha.ok_or_else(|| anyhow!("missing sha256"))?,
        size_bytes: size.ok_or_else(|| anyhow!("missing size"))?,
        version_code,
        version_name,
    })
}

/// Fallback for upstream manifests without explicit version_code.
///
/// Convention: tag `rc<N>-b<M>` → version_code = `N * 1000 + M`. Monotonic
/// within reasonable rc-numbers (N up to ~999, M up to 999 → safe for Android's
/// 32-bit signed versionCode). Когда apk-builder научится класть `version_code`
/// в upstream manifest напрямую — этот helper становится best-effort fallback.
pub fn version_code_from_tag(tag: &str) -> Option<i64> {
    let lower = tag.trim().to_ascii_lowercase();
    let rest = lower.strip_prefix("rc")?;
    let (rc_n, b_part) = rest.split_once('-')?;
    let rc_n = rc_n.parse::<i64>().ok()?;
    let b_m = b_part.strip_prefix('b')?.parse::<i64>().ok()?;
    Some(rc_n * 1000 + b_m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_version_txt() {
        let body = "tag=rc42-b33\nsha256=5eb1ff4cea\nsize=177187065\n";
        let m = parse_version_txt(body).unwrap();
        assert_eq!(m.tag, "rc42-b33");
        assert_eq!(m.sha256, "5eb1ff4cea");
        assert_eq!(m.size_bytes, 177_187_065);
        assert!(m.version_code.is_none());
    }

    #[test]
    fn parses_with_optional_version_code() {
        let body =
            "tag=rc42-b33\nsha256=abc\nsize=1\nversion_code=174\nversion_name=1.0.0-rc42-b33\n";
        let m = parse_version_txt(body).unwrap();
        assert_eq!(m.version_code, Some(174));
        assert_eq!(m.version_name.as_deref(), Some("1.0.0-rc42-b33"));
    }

    #[test]
    fn parses_comments_and_blanks() {
        let body = "# header\n\ntag=foo\nsha256=AbCdEf\nsize=42\n";
        let m = parse_version_txt(body).unwrap();
        assert_eq!(m.tag, "foo");
        // sha256 normalised to lowercase
        assert_eq!(m.sha256, "abcdef");
    }

    #[test]
    fn version_code_from_tag_works() {
        assert_eq!(version_code_from_tag("rc42-b33"), Some(42_033));
        assert_eq!(version_code_from_tag("RC42-B33"), Some(42_033));
        assert_eq!(version_code_from_tag("rc1-b1"), Some(1_001));
        assert_eq!(version_code_from_tag("rc999-b999"), Some(999_999));
        assert_eq!(version_code_from_tag("not-a-tag"), None);
        assert_eq!(version_code_from_tag("rc42"), None);
        assert_eq!(version_code_from_tag("rc42-snapshot"), None);
    }

    #[test]
    fn upstream_blob_url_derived_legacy_schema() {
        let m = UpstreamManifest {
            tag: "rc42-b33".into(),
            sha256: "x".into(),
            size_bytes: 0,
            version_code: None,
            version_name: None,
        };
        let url = "https://pub-ef0219.r2.dev/apks/latest/version.txt";
        assert_eq!(
            m.upstream_blob_url(url),
            "https://pub-ef0219.r2.dev/apks/rc42-b33/app-debug.apk"
        );
    }

    #[test]
    fn upstream_blob_url_derived_new_b44_schema() {
        let m = UpstreamManifest {
            tag: "rc42-b44".into(),
            sha256: "x".into(),
            size_bytes: 0,
            version_code: None,
            version_name: None,
        };
        let url = "https://pub-ef0219.r2.dev/apks/outpost-latest-debug.version.txt";
        assert_eq!(
            m.upstream_blob_url(url),
            "https://pub-ef0219.r2.dev/apks/outpost-rc42-b44-debug.apk"
        );
    }

    #[test]
    fn upstream_blob_url_fallback_for_unrecognized_path() {
        let m = UpstreamManifest {
            tag: "rc42-b44".into(),
            sha256: "x".into(),
            size_bytes: 0,
            version_code: None,
            version_name: None,
        };
        // Если URL не соответствует ни одной из двух конвенций — берём same-dir + tag-suffix.
        let url = "https://custom.example.com/build-channel/manifest.txt";
        assert_eq!(
            m.upstream_blob_url(url),
            "https://custom.example.com/build-channel/rc42-b44-app-debug.apk"
        );
    }

    #[test]
    fn is_404_detects_status() {
        let e = anyhow!("non-2xx (404 Not Found) from https://example/x");
        assert!(is_404(&e));
        let e2 = anyhow!("non-2xx (503 Service Unavailable) from https://x");
        assert!(!is_404(&e2));
        let e3 = anyhow!("connection refused");
        assert!(!is_404(&e3));
    }
}
