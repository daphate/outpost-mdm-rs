//! Shared application state injected into every handler.

use crate::cloudru_signer::CloudRuPresigner;
use crate::rate_limit::LoginRateLimiter;
use chrono_tz::Tz;
use sqlx::SqlitePool;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// v0.18.16: выбираемый формат вывода datetime в admin UI. Хранится в
/// `settings.server.datetime_format` как короткая строка-id (`ru` /
/// `iso` / `eu` / `us`). Default — `Ru` (DD.MM.YYYY HH:MM, привычный
/// российскому пользователю).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateFormat {
    /// 19.05.2026 20:26 — российский формат
    Ru,
    /// 2026-05-19 20:26 — ISO 8601 short (debugging-friendly)
    Iso,
    /// 19/05/2026 20:26 — европейский слэш-формат
    EuShort,
    /// 05/19/2026 8:26 PM — американский формат с AM/PM
    UsShort,
}

impl DateFormat {
    /// Канонический id для хранения в БД / dropdown'ах.
    pub fn as_id(&self) -> &'static str {
        match self {
            Self::Ru => "ru",
            Self::Iso => "iso",
            Self::EuShort => "eu",
            Self::UsShort => "us",
        }
    }

    /// Человекочитаемая подпись для dropdown'а на /settings.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ru => "RU — 19.05.2026 20:26",
            Self::Iso => "ISO — 2026-05-19 20:26",
            Self::EuShort => "EU — 19/05/2026 20:26",
            Self::UsShort => "US — 05/19/2026 8:26 PM",
        }
    }

    /// Все варианты для рендеринга dropdown'а в settings.html.
    pub fn all() -> [DateFormat; 4] {
        [Self::Ru, Self::Iso, Self::EuShort, Self::UsShort]
    }

    /// Parse id из БД / form input. Unknown → fallback на Ru с warning.
    pub fn from_id(s: &str) -> Self {
        match s.trim() {
            "ru" => Self::Ru,
            "iso" => Self::Iso,
            "eu" => Self::EuShort,
            "us" => Self::UsShort,
            other => {
                tracing::warn!(value = %other, "unknown datetime_format id, fallback to ru");
                Self::Ru
            }
        }
    }

    /// chrono strftime-spec для соответствующего варианта. Без секунд —
    /// все варианты «HH:MM», без миллисекунд (юзеру они не нужны в UI).
    pub fn strftime(&self) -> &'static str {
        match self {
            Self::Ru => "%d.%m.%Y %H:%M",
            Self::Iso => "%Y-%m-%d %H:%M",
            Self::EuShort => "%d/%m/%Y %H:%M",
            Self::UsShort => "%m/%d/%Y %-I:%M %p",
        }
    }
}

/// Application state — held by axum's `with_state` and extracted via
/// `axum::extract::State<AppState>`. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    /// Symmetric secret for HMAC-SHA256 on signed download URLs.
    pub app_secret: Arc<String>,
    /// User session TTL in seconds.
    pub session_ttl_secs: i64,
    pub app_files_dir: Arc<PathBuf>,
    pub max_body_bytes: usize,
    pub request_timeout_secs: u64,
    pub secure_cookies: bool,
    pub login_limiter: LoginRateLimiter,
    /// Cloud.ru presigner для генерации APK download QR'ов на странице
    /// enrollment. `None` если соответствующие env-vars не заданы — в этом
    /// случае admin UI скрывает APK-QR блок.
    pub cloudru_signer: Option<Arc<CloudRuPresigner>>,
    /// Object key для latest APK pointer (`apks/latest/app-debug.apk` by default).
    pub cloudru_apk_key: Arc<String>,
    /// v0.18.9: server-wide timezone for rendering UTC timestamps in
    /// admin UI. Loaded from `settings.server.timezone` at startup,
    /// hot-reloaded by `settings_save` handler — no restart required.
    /// Default Europe/Moscow (MSK).
    pub server_tz: Arc<RwLock<Tz>>,
    /// v0.18.16: формат отображения дат в admin UI. Hot-reloadable
    /// аналогично server_tz. Default — DateFormat::Ru.
    pub server_dt_format: Arc<RwLock<DateFormat>>,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: SqlitePool,
        app_secret: String,
        session_ttl_secs: i64,
        app_files_dir: PathBuf,
        max_body_bytes: usize,
        request_timeout_secs: u64,
        secure_cookies: bool,
        cloudru_signer: Option<CloudRuPresigner>,
        cloudru_apk_key: String,
        server_tz: Tz,
    ) -> Self {
        Self {
            db,
            app_secret: Arc::new(app_secret),
            session_ttl_secs,
            app_files_dir: Arc::new(app_files_dir),
            max_body_bytes,
            request_timeout_secs,
            secure_cookies,
            login_limiter: LoginRateLimiter::default_login(),
            cloudru_signer: cloudru_signer.map(Arc::new),
            cloudru_apk_key: Arc::new(cloudru_apk_key),
            server_tz: Arc::new(RwLock::new(server_tz)),
            server_dt_format: Arc::new(RwLock::new(DateFormat::Ru)),
        }
    }

    /// Snapshot of current datetime format. Cheap (Copy under lock).
    pub fn dt_format(&self) -> DateFormat {
        *self
            .server_dt_format
            .read()
            .expect("server_dt_format RwLock poisoned — bug somewhere upstream")
    }

    /// Replace the active datetime format atomically.
    pub fn set_dt_format(&self, f: DateFormat) {
        if let Ok(mut guard) = self.server_dt_format.write() {
            *guard = f;
        }
    }

    /// Snapshot of the current TZ. Cheap (Copy under the lock).
    pub fn tz(&self) -> Tz {
        *self
            .server_tz
            .read()
            .expect("server_tz RwLock poisoned — bug somewhere upstream")
    }

    /// Replace the active TZ atomically. Called from `settings_save` after
    /// persisting the new value to the `settings` table.
    pub fn set_tz(&self, tz: Tz) {
        if let Ok(mut guard) = self.server_tz.write() {
            *guard = tz;
        }
    }

    /// Format a UTC timestamp string from БД as the user's selected
    /// datetime format in the configured timezone.
    ///
    /// Поддерживает несколько входных форматов (в БД могут лежать оба):
    /// - `"YYYY-MM-DD HH:MM:SS"` — SQLite `datetime('now')` (naive UTC,
    ///   без TZ marker'а).
    /// - `"YYYY-MM-DD HH:MM:SS.SSS"` — то же + миллисекунды.
    /// - `"YYYY-MM-DDTHH:MM:SS[.SSS][+00:00|Z]"` — RFC 3339 / ISO 8601
    ///   с TZ marker'ом (OTLP ingest пишет так).
    ///
    /// Falls back to the raw string verbatim if все парсеры провалились —
    /// admin UI must never crash on a stale or malformed row.
    pub fn fmt_ts(&self, s: &str) -> String {
        use chrono::TimeZone;
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        // 1. RFC 3339 / ISO 8601 — с TZ marker'ом.
        let utc_dt = if let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(trimmed) {
            parsed.with_timezone(&chrono::Utc)
        } else if let Ok(naive) =
            chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S%.f")
        {
            // 2. SQLite format с миллисекундами (treat as UTC).
            chrono::Utc.from_utc_datetime(&naive)
        } else if let Ok(naive) =
            chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S")
        {
            // 3. SQLite naive datetime, без TZ (treat as UTC).
            chrono::Utc.from_utc_datetime(&naive)
        } else {
            // 4. Fallback — отдать raw, не падать.
            return trimmed.to_string();
        };
        let local = utc_dt.with_timezone(&self.tz());
        local.format(self.dt_format().strftime()).to_string()
    }
}

/// Resolve the server's datetime format from
/// `settings.server.datetime_format`. Idempotent. Falls back to
/// DateFormat::Ru if the setting is absent / unrecognised.
pub async fn load_server_dt_format(db: &SqlitePool) -> DateFormat {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT json_extract(value_json, '$') FROM settings WHERE key = 'server.datetime_format'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten();
    match raw {
        Some(s) => DateFormat::from_id(&s),
        None => DateFormat::Ru,
    }
}

/// Resolve the server's timezone from `settings.server.timezone`.
/// Idempotent — caller can invoke at startup or after a settings edit.
/// Falls back to Europe/Moscow on parse failure, logs a warning.
pub async fn load_server_tz(db: &SqlitePool) -> Tz {
    const DEFAULT_TZ: Tz = chrono_tz::Europe::Moscow;
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT json_extract(value_json, '$') FROM settings WHERE key = 'server.timezone'",
    )
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten();
    let Some(name) = raw else {
        return DEFAULT_TZ;
    };
    match name.parse::<Tz>() {
        Ok(tz) => tz,
        Err(e) => {
            tracing::warn!(
                error = %e,
                value = %name,
                "settings.server.timezone не парсится как IANA tz, fallback на Europe/Moscow"
            );
            DEFAULT_TZ
        }
    }
}

/// In-memory DB + per-test temp file dir + bootstrapped admin. Always
/// available so out-of-crate integration tests can call without ceremony.
pub async fn test_state() -> AppState {
    let cfg = crate::config::Config::test_default();
    let pool = crate::db::open_pool(":memory:")
        .await
        .expect("open in-memory test pool");
    crate::bootstrap::bootstrap_pending_passwords(&pool)
        .await
        .expect("bootstrap test admin");
    AppState::new(
        pool,
        cfg.app_secret,
        cfg.session_ttl_secs,
        make_test_dir(),
        cfg.max_body_bytes,
        cfg.request_timeout_secs,
        cfg.secure_cookies,
        None,
        cfg.cloudru_apk_key,
        chrono_tz::UTC,
    )
}

fn make_test_dir() -> PathBuf {
    use rand::Rng;
    use rand::distributions::Alphanumeric;
    let suffix: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(12)
        .map(char::from)
        .collect();
    let mut p = std::env::temp_dir();
    p.push(format!("outpost-test-{suffix}"));
    std::fs::create_dir_all(&p).expect("create test files dir");
    p
}

#[cfg(test)]
mod tests {
    //! v0.18.10: tests for timezone-aware formatting + settings loader.
    //! Покрытие — пропущенное в v0.18.9.
    use super::*;
    use sqlx::Executor;

    /// Helper — собираем минимальный AppState с заданным tz, без bootstrap'а.
    async fn state_with_tz(tz: Tz) -> AppState {
        let pool = crate::db::open_pool(":memory:").await.unwrap();
        AppState::new(
            pool,
            "test-secret-with-at-least-32-bytes-of-padding-yes".to_string(),
            86_400,
            std::env::temp_dir().join("outpost-tz-test"),
            crate::config::DEFAULT_MAX_BODY_BYTES,
            crate::config::DEFAULT_REQUEST_TIMEOUT_SECS,
            false,
            None,
            "apks/outpost-latest-debug.apk".to_string(),
            tz,
        )
    }

    #[tokio::test]
    async fn fmt_ts_converts_utc_to_msk_with_plus3_offset() {
        let state = state_with_tz(chrono_tz::Europe::Moscow).await;
        // v0.18.16: default dt_format = Ru → "DD.MM.YYYY HH:MM".
        // UTC 2026-05-19 00:00:00 → MSK 2026-05-19 03:00.
        let result = state.fmt_ts("2026-05-19 00:00:00");
        assert_eq!(result, "19.05.2026 03:00", "UTC→MSK conversion broken (Ru format)");
    }

    #[tokio::test]
    async fn fmt_ts_converts_utc_to_pacific_with_negative_offset() {
        let state = state_with_tz(chrono_tz::America::Los_Angeles).await;
        // UTC 2026-05-19 07:00:00 в Los_Angeles (DST = UTC-7) → 2026-05-19 00:00.
        let result = state.fmt_ts("2026-05-19 07:00:00");
        assert_eq!(result, "19.05.2026 00:00", "UTC→LA conversion broken (Ru format)");
    }

    #[tokio::test]
    async fn fmt_ts_passes_through_malformed_input() {
        let state = state_with_tz(chrono_tz::Europe::Moscow).await;
        // UI не должен крашиться на stale/garbage row.
        assert_eq!(state.fmt_ts("not-a-timestamp"), "not-a-timestamp");
        assert_eq!(state.fmt_ts(""), "");
        assert_eq!(state.fmt_ts("—"), "—");
    }

    #[tokio::test]
    async fn fmt_ts_utc_tz_is_identity_format() {
        let state = state_with_tz(chrono_tz::UTC).await;
        // Ru default.
        assert_eq!(state.fmt_ts("2026-05-19 12:34:56"), "19.05.2026 12:34");
    }

    #[tokio::test]
    async fn set_tz_atomically_replaces_current() {
        let state = state_with_tz(chrono_tz::Europe::Moscow).await;
        // Изначально MSK + Ru format.
        assert_eq!(state.fmt_ts("2026-05-19 00:00:00"), "19.05.2026 03:00");
        // Переключаемся на UTC
        state.set_tz(chrono_tz::UTC);
        assert_eq!(state.fmt_ts("2026-05-19 00:00:00"), "19.05.2026 00:00");
    }

    #[tokio::test]
    async fn fmt_ts_parses_rfc3339_with_timezone_offset() {
        // v0.18.16: OTLP ingest пишет ts в формате "2026-05-19T17:26:44.966+00:00".
        // Раньше fmt_ts падал на этот формат → возвращал raw. Теперь парсит как RFC3339.
        let state = state_with_tz(chrono_tz::Europe::Moscow).await;
        let result = state.fmt_ts("2026-05-19T17:26:44.966+00:00");
        // UTC 17:26 → MSK 20:26.
        assert_eq!(result, "19.05.2026 20:26");
    }

    #[tokio::test]
    async fn fmt_ts_parses_sqlite_datetime_with_fractional_seconds() {
        let state = state_with_tz(chrono_tz::UTC).await;
        let result = state.fmt_ts("2026-05-19 12:34:56.789");
        assert_eq!(result, "19.05.2026 12:34");
    }

    #[tokio::test]
    async fn fmt_ts_respects_dt_format_iso() {
        let state = state_with_tz(chrono_tz::UTC).await;
        state.set_dt_format(DateFormat::Iso);
        assert_eq!(state.fmt_ts("2026-05-19 12:34:56"), "2026-05-19 12:34");
    }

    #[tokio::test]
    async fn fmt_ts_respects_dt_format_eu_short() {
        let state = state_with_tz(chrono_tz::UTC).await;
        state.set_dt_format(DateFormat::EuShort);
        assert_eq!(state.fmt_ts("2026-05-19 12:34:56"), "19/05/2026 12:34");
    }

    #[tokio::test]
    async fn date_format_from_id_handles_unknown_with_fallback() {
        assert_eq!(DateFormat::from_id("ru"), DateFormat::Ru);
        assert_eq!(DateFormat::from_id("iso"), DateFormat::Iso);
        assert_eq!(DateFormat::from_id("eu"), DateFormat::EuShort);
        assert_eq!(DateFormat::from_id("us"), DateFormat::UsShort);
        // Unknown → fallback Ru
        assert_eq!(DateFormat::from_id("zzz"), DateFormat::Ru);
        assert_eq!(DateFormat::from_id(""), DateFormat::Ru);
    }

    /// Helper — миграция 0020 seed'ит `server.timezone = "Europe/Moscow"` в
    /// каждый свежий pool. Тесты этого подгруппой DELETE'ом сначала
    /// прибирают сидированное значение чтобы изолировать желаемое
    /// состояние БД.
    async fn reset_tz_setting(pool: &SqlitePool) {
        pool.execute("DELETE FROM settings WHERE key='server.timezone'")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn load_server_tz_returns_default_when_setting_absent() {
        let pool = crate::db::open_pool(":memory:").await.unwrap();
        reset_tz_setting(&pool).await;
        let tz = load_server_tz(&pool).await;
        assert_eq!(tz, chrono_tz::Europe::Moscow);
    }

    #[tokio::test]
    async fn load_server_tz_returns_default_when_value_invalid() {
        let pool = crate::db::open_pool(":memory:").await.unwrap();
        reset_tz_setting(&pool).await;
        pool.execute(
            r#"INSERT INTO settings(key, value_json) VALUES('server.timezone', '"NotAValidTz"')"#,
        )
        .await
        .unwrap();
        // Bad value → fallback на Europe/Moscow (плюс warning в logs).
        let tz = load_server_tz(&pool).await;
        assert_eq!(tz, chrono_tz::Europe::Moscow);
    }

    #[tokio::test]
    async fn load_server_tz_returns_configured_value() {
        let pool = crate::db::open_pool(":memory:").await.unwrap();
        reset_tz_setting(&pool).await;
        pool.execute(
            r#"INSERT INTO settings(key, value_json) VALUES('server.timezone', '"America/Los_Angeles"')"#,
        )
        .await
        .unwrap();
        let tz = load_server_tz(&pool).await;
        assert_eq!(tz, chrono_tz::America::Los_Angeles);
    }

    #[tokio::test]
    async fn migration_0020_seeds_europe_moscow_by_default() {
        // Sanity-check самой миграции: фрезровый pool с миграциями должен
        // прийти сразу с server.timezone = "Europe/Moscow" — это и есть
        // intent миграции 0020.
        let pool = crate::db::open_pool(":memory:").await.unwrap();
        let tz = load_server_tz(&pool).await;
        assert_eq!(tz, chrono_tz::Europe::Moscow);
    }
}
