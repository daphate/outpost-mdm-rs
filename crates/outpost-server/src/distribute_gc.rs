//! GC task для encrypted_distributions blob'ов.
//!
//! Раз в сутки (settings `distribute.gc_tick_secs`) сканирует
//! `encrypted_distributions` где:
//!   - `expires_at IS NOT NULL`
//!   - `expires_at < now() - <grace>`
//!   - `purged_at IS NULL`
//!
//! Помечает rows `purged_at = now()`. Если последняя distribution-row
//! для blob'а (sha256) ушла в purged → удаляет файл с диска.
//!
//! Grace period: 7 дней (settings `distribute.gc_grace_days`). Это позволяет
//! устройствам успеть скачать blob если они offline пару дней.
//!
//! См. MDM-DEVICE-CONTROL-CONTRACT.md §6 Open question 2.

use sqlx::SqlitePool;
use std::path::Path;
use std::time::Duration;
use tokio::time;

/// 24 часа default tick.
pub const DEFAULT_TICK_SECS: u64 = 24 * 3600;
const MIN_TICK_SECS: u64 = 60;
const MAX_TICK_SECS: u64 = 7 * 24 * 3600;

/// 7-дневный grace period после `expires_at`.
pub const DEFAULT_GRACE_DAYS: i64 = 7;
const MIN_GRACE_DAYS: i64 = 0;
const MAX_GRACE_DAYS: i64 = 90;

pub fn spawn(
    pool: SqlitePool,
    app_files_dir: std::sync::Arc<std::path::PathBuf>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let tick = read_setting_u64(&pool, "distribute.gc_tick_secs", DEFAULT_TICK_SECS)
            .await
            .clamp(MIN_TICK_SECS, MAX_TICK_SECS);
        let grace_days = read_setting_i64(&pool, "distribute.gc_grace_days", DEFAULT_GRACE_DAYS)
            .await
            .clamp(MIN_GRACE_DAYS, MAX_GRACE_DAYS);
        tracing::info!(
            tick_secs = tick,
            grace_days,
            "encrypted-distribution GC started"
        );
        let mut interval = time::interval(Duration::from_secs(tick));
        // Skip first immediate tick — пусть рантайм инициализируется.
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(e) = tick_once(&pool, app_files_dir.as_path(), grace_days).await {
                tracing::warn!(error = ?e, "distribute GC tick failed");
            }
        }
    })
}

async fn read_setting_u64(pool: &SqlitePool, key: &str, default: u64) -> u64 {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT json_extract(value_json, '$') FROM settings WHERE key = ?",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .flatten()
    .and_then(|s| s.parse::<u64>().ok())
    .unwrap_or(default)
}

async fn read_setting_i64(pool: &SqlitePool, key: &str, default: i64) -> i64 {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT json_extract(value_json, '$') FROM settings WHERE key = ?",
    )
    .bind(key)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .flatten()
    .and_then(|s| s.parse::<i64>().ok())
    .unwrap_or(default)
}

async fn tick_once(
    pool: &SqlitePool,
    app_files_dir: &Path,
    grace_days: i64,
) -> Result<(), sqlx::Error> {
    // 1. Найти rows для purge.
    let candidates: Vec<(i64, String)> = sqlx::query_as(
        "SELECT id, ciphertext_sha256 FROM encrypted_distributions \
         WHERE expires_at IS NOT NULL \
           AND expires_at < datetime('now', '-' || ? || ' days') \
           AND purged_at IS NULL",
    )
    .bind(grace_days)
    .fetch_all(pool)
    .await?;

    if candidates.is_empty() {
        return Ok(());
    }
    tracing::info!(candidates = candidates.len(), "GC: распределений на purge");

    // 2. Mark all rows purged_at; group by sha256 для удаления файлов.
    let mut shas_to_check = std::collections::HashSet::new();
    for (id, sha) in &candidates {
        let res = sqlx::query(
            "UPDATE encrypted_distributions SET purged_at = datetime('now') WHERE id = ?",
        )
        .bind(id)
        .execute(pool)
        .await;
        if let Err(e) = res {
            tracing::warn!(id, error = ?e, "GC: UPDATE purged_at failed");
        } else {
            shas_to_check.insert(sha.clone());
        }
    }

    // 3. Для каждого sha: если БОЛЬШЕ нет non-purged rows c этим sha — file
    //    можно удалить (никто из текущих recipient'ов больше не качает).
    for sha in shas_to_check {
        let still_active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM encrypted_distributions \
             WHERE ciphertext_sha256 = ? AND purged_at IS NULL",
        )
        .bind(&sha)
        .fetch_one(pool)
        .await
        .unwrap_or(1);
        if still_active > 0 {
            tracing::debug!(
                sha = %sha,
                still_active,
                "GC: blob держится active row'ами, файл оставлен"
            );
            continue;
        }
        let blob_path = app_files_dir.join("encrypted").join(format!("{sha}.bin"));
        if blob_path.exists() {
            match tokio::fs::remove_file(&blob_path).await {
                Ok(_) => tracing::info!(
                    sha = %sha,
                    path = %blob_path.display(),
                    "GC: blob удалён с диска"
                ),
                Err(e) => tracing::warn!(
                    sha = %sha,
                    path = %blob_path.display(),
                    error = %e,
                    "GC: remove blob failed"
                ),
            }
        } else {
            tracing::debug!(
                sha = %sha,
                path = %blob_path.display(),
                "GC: blob уже отсутствует на диске"
            );
        }
    }
    Ok(())
}
