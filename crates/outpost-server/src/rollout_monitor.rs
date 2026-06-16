//! Tier-2 rollout monitor: auto-promote и auto-rollback фазированных раскаток.
//!
//! Каждый tick (default 60s) проходит по `application_rollouts` с
//! `phase='canary'` и решает:
//!
//!  * **Auto-promote**: если `canary_until_at < now()` — переключает на
//!    `phase='fleet'`. С этого момента /api/v1/sync будет отдавать target
//!    всем устройствам (fleet-wide).
//!
//!  * **Auto-rollback**: считает crash-rate (доля устройств в `group_id`
//!    со severity≥17 логом за последний час) среди тех, кто уже подхватил
//!    target версию. Если rate > `crash_threshold_pct` — переключает на
//!    `phase='rolled_back'` с пометкой в `rolled_back_reason`. Команда
//!    остаётся в БД для audit-trail; для устройств она перестаёт быть
//!    target'ом и /sync на следующем тике перестанет её отдавать.
//!
//! Monitor не трогает `phase='paused'` и `'rolled_back'` строки.

use sqlx::SqlitePool;
use std::time::Duration;
use tokio::time;

pub const DEFAULT_TICK_SECS: u64 = 60;
const MIN_TICK_SECS: u64 = 10;
const MAX_TICK_SECS: u64 = 600;

/// Минимум устройств в canary прежде чем crash-rate gate активируется.
/// На малом sample'е (1-2 устройства) одна крашнувшаяся симка
/// даст 50%+ rate ложно. Tier-3 может вынести это в setting.
const CANARY_MIN_DEVICES_FOR_AUTOROLLBACK: i64 = 3;

/// Окно подсчёта crash-rate — последний час сообщений с устройств.
const CRASH_RATE_WINDOW_HOURS: i64 = 1;

pub fn spawn(pool: SqlitePool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let tick = read_tick_secs(&pool).await;
        tracing::info!(tick_secs = tick, "rollout monitor started");
        let mut interval = time::interval(Duration::from_secs(tick));
        interval.tick().await; // skip immediate first
        loop {
            interval.tick().await;
            if let Err(e) = tick_once(&pool).await {
                tracing::warn!(error = ?e, "rollout monitor tick failed");
            }
        }
    })
}

async fn read_tick_secs(pool: &SqlitePool) -> u64 {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT json_extract(value_json, '$') FROM settings WHERE key = 'rollout.monitor_tick_secs'",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .flatten();
    raw.and_then(|s| s.parse::<u64>().ok())
        .filter(|n| (MIN_TICK_SECS..=MAX_TICK_SECS).contains(n))
        .unwrap_or(DEFAULT_TICK_SECS)
}

async fn tick_once(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let rollouts: Vec<CanaryRow> = sqlx::query_as(
        "SELECT r.id, r.application_id, r.target_version_id, r.group_id, \
                r.canary_until_at, r.crash_threshold_pct \
         FROM application_rollouts r \
         WHERE r.phase = 'canary'",
    )
    .fetch_all(pool)
    .await?;

    for rollout in rollouts {
        // 1. Auto-promote по дедлайну.
        if let Some(deadline) = &rollout.canary_until_at {
            let due: Option<i64> =
                sqlx::query_scalar("SELECT CASE WHEN ? <= datetime('now') THEN 1 ELSE 0 END")
                    .bind(deadline)
                    .fetch_optional(pool)
                    .await?;
            if due.unwrap_or(0) == 1 {
                // Crash-rate gate ещё успеваем проверить — может уже плохо.
                if let Some(reason) = check_crash_rollback(pool, &rollout).await? {
                    set_phase(pool, rollout.id, "rolled_back", Some(&reason)).await?;
                    tracing::warn!(
                        rollout_id = rollout.id,
                        reason = %reason,
                        "rollout auto-rolled back at canary deadline (crash threshold)"
                    );
                    continue;
                }
                set_phase(pool, rollout.id, "fleet", None).await?;
                tracing::info!(
                    rollout_id = rollout.id,
                    application_id = rollout.application_id,
                    target_version_id = rollout.target_version_id,
                    "rollout auto-promoted canary → fleet"
                );
                continue;
            }
        }
        // 2. Auto-rollback независимо от deadline.
        if let Some(reason) = check_crash_rollback(pool, &rollout).await? {
            set_phase(pool, rollout.id, "rolled_back", Some(&reason)).await?;
            tracing::warn!(
                rollout_id = rollout.id,
                reason = %reason,
                "rollout auto-rolled back (crash threshold)"
            );
        }
    }
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct CanaryRow {
    id: i64,
    application_id: i64,
    target_version_id: i64,
    group_id: Option<i64>,
    canary_until_at: Option<String>,
    crash_threshold_pct: f64,
}

/// Считает crash-rate среди устройств group_id на target_version.
/// Возвращает `Some(reason)` если порог превышен — caller должен делать
/// rollback. Возвращает `None` если ОК (или sample слишком мал).
async fn check_crash_rollback(
    pool: &SqlitePool,
    rollout: &CanaryRow,
) -> Result<Option<String>, sqlx::Error> {
    let Some(group_id) = rollout.group_id else {
        // Canary без group — нечего проверять (это вырожденный случай —
        // canary должна быть scoped). Auto-promote'нится по deadline.
        return Ok(None);
    };
    // target version code — для фильтрации device.app_version_code = target.
    let target_code: Option<i64> =
        sqlx::query_scalar("SELECT version_code FROM application_versions WHERE id = ?")
            .bind(rollout.target_version_id)
            .fetch_optional(pool)
            .await?;
    let Some(target_code) = target_code else {
        return Ok(None);
    };

    // Сколько устройств в group_id уже на target?
    let on_target: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM devices d \
         JOIN device_groups dg ON dg.device_id = d.id \
         WHERE dg.group_id = ? AND d.app_version_code = ?",
    )
    .bind(group_id)
    .bind(target_code)
    .fetch_one(pool)
    .await?;
    if on_target < CANARY_MIN_DEVICES_FOR_AUTOROLLBACK {
        return Ok(None);
    }
    // Сколько из них прислали ERROR-лог за последний час?
    let with_errors: i64 = sqlx::query_scalar(
        "SELECT COUNT(DISTINCT d.id) FROM devices d \
         JOIN device_groups dg ON dg.device_id = d.id \
         JOIN device_logs dl ON dl.device_id = d.id \
         WHERE dg.group_id = ? \
           AND d.app_version_code = ? \
           AND dl.severity_number >= 17 \
           AND dl.ts > datetime('now', '-' || ? || ' hour')",
    )
    .bind(group_id)
    .bind(target_code)
    .bind(CRASH_RATE_WINDOW_HOURS)
    .fetch_one(pool)
    .await?;

    let rate_pct = (with_errors as f64) / (on_target as f64) * 100.0;
    if rate_pct > rollout.crash_threshold_pct {
        return Ok(Some(format!(
            "crash-rate {:.1}% > threshold {:.1}% \
             ({}/{} устройств с ERROR-логом за {}ч)",
            rate_pct, rollout.crash_threshold_pct, with_errors, on_target, CRASH_RATE_WINDOW_HOURS,
        )));
    }
    Ok(None)
}

async fn set_phase(
    pool: &SqlitePool,
    rollout_id: i64,
    phase: &str,
    rollback_reason: Option<&str>,
) -> Result<(), sqlx::Error> {
    if phase == "rolled_back" {
        sqlx::query(
            "UPDATE application_rollouts SET phase = ?, updated_at = datetime('now'), \
                rolled_back_at = datetime('now'), rolled_back_reason = ? \
             WHERE id = ?",
        )
        .bind(phase)
        .bind(rollback_reason.unwrap_or("auto-rollback"))
        .bind(rollout_id)
        .execute(pool)
        .await?;
    } else {
        sqlx::query(
            "UPDATE application_rollouts SET phase = ?, updated_at = datetime('now') WHERE id = ?",
        )
        .bind(phase)
        .bind(rollout_id)
        .execute(pool)
        .await?;
    }
    Ok(())
}
