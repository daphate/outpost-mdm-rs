//! Push scheduler — fan out `push_schedule` rows into per-device `push_messages`.
//!
//! Runs as a tokio task spawned from `main.rs`. The loop drains every
//! `tick_secs` (read once at start; live tuning via `settings` table is
//! out of scope for v1). For one-shot scheduled tasks (`due_at IS NOT NULL`),
//! the row transitions `pending → done`. Cron expressions are not yet
//! supported — the column is reserved for future iteration.

use sqlx::SqlitePool;
use std::time::Duration;
use tokio::time;

/// Default tick interval if the `settings` lookup misses or is malformed.
pub const DEFAULT_TICK_SECS: u64 = 60;

/// Spawn the scheduler. Returns the join handle so `main.rs` can await it
/// on shutdown (best-effort drain).
pub fn spawn(pool: SqlitePool) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Read the cadence from settings on boot.
        let tick = read_tick_secs(&pool).await;
        tracing::info!(tick_secs = tick, "push scheduler started");
        let mut interval = time::interval(Duration::from_secs(tick));
        // Skip the first immediate tick — give the rest of the app a moment.
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(e) = tick_once(&pool).await {
                tracing::error!(error = ?e, "scheduler tick failed");
            }
        }
    })
}

async fn read_tick_secs(pool: &SqlitePool) -> u64 {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT json_extract(value_json, '$') FROM settings WHERE key = 'push.scheduler_tick_secs'",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    raw.and_then(|s| s.parse::<u64>().ok())
        .filter(|n| (5..=3600).contains(n))
        .unwrap_or(DEFAULT_TICK_SECS)
}

/// Row read by the tick — kept as a named struct to avoid clippy's
/// `type_complexity` complaint and to make the SQL column order explicit.
#[derive(sqlx::FromRow)]
struct ScheduleRow {
    id: i64,
    customer_id: i64,
    device_id: Option<i64>,
    group_id: Option<i64>,
    configuration_id: Option<i64>,
    command: String,
    payload_json: String,
}

/// Execute one scheduler pass. Public so tests can drive it deterministically.
pub async fn tick_once(pool: &SqlitePool) -> sqlx::Result<usize> {
    let rows: Vec<ScheduleRow> = sqlx::query_as::<_, ScheduleRow>(
        "SELECT id, customer_id, device_id, group_id, configuration_id, command, payload_json \
         FROM push_schedule \
         WHERE status = 'pending' \
         AND (due_at IS NULL OR due_at <= datetime('now')) \
         ORDER BY id ASC LIMIT 50",
    )
    .fetch_all(pool)
    .await?;

    let mut emitted = 0_usize;
    for ScheduleRow {
        id: sched_id,
        customer_id,
        device_id,
        group_id,
        configuration_id: config_id,
        command,
        payload_json: payload,
    } in rows
    {
        let device_ids = resolve_targets(pool, customer_id, device_id, group_id, config_id).await?;
        // Атомарно: все push_messages этого расписания + перевод в 'done' — в
        // одной транзакции. Иначе крах (panic="abort") между вставками и
        // UPDATE'ом оставил бы расписание 'pending' → повторная полная рассылка
        // всех команд при следующем тике.
        let mut tx = pool.begin().await?;
        let mut row_emitted = 0_usize;
        for did in device_ids {
            sqlx::query(
                "INSERT INTO push_messages \
                    (customer_id, device_id, command, payload_json, status, schedule_id) \
                 VALUES (?, ?, ?, ?, 'pending', ?)",
            )
            .bind(customer_id)
            .bind(did)
            .bind(&command)
            .bind(&payload)
            .bind(sched_id)
            .execute(&mut *tx)
            .await?;
            row_emitted += 1;
        }
        sqlx::query(
            "UPDATE push_schedule SET status = 'done', last_run_at = datetime('now') WHERE id = ?",
        )
        .bind(sched_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        emitted += row_emitted;
    }

    if emitted > 0 {
        tracing::info!(emitted, "scheduler tick emitted push messages");
    }
    // Opportunistic session GC: drop rows expired/revoked > 30 days ago.
    if let Ok(n) = crate::session::cleanup(pool, 30).await
        && n > 0
    {
        tracing::info!(cleaned = n, "scheduler tick pruned old sessions");
    }
    // Retention: телеметрия (device_logs / device_metrics / device_traces) —
    // append-only и растёт неограниченно на 2 GB VM. Чистим строки старше
    // настраиваемого окна (settings `telemetry.retention_days`, default 30).
    let retention_days = read_telemetry_retention_days(pool).await;
    match prune_telemetry(pool, retention_days).await {
        Ok(n) if n > 0 => {
            tracing::info!(pruned = n, days = retention_days, "scheduler pruned old telemetry")
        }
        Err(e) => tracing::warn!(error = ?e, "telemetry retention prune failed"),
        _ => {}
    }
    Ok(emitted)
}

/// Retention window for telemetry, from settings (`telemetry.retention_days`,
/// clamped 1..=365), defaulting to 30 days.
async fn read_telemetry_retention_days(pool: &SqlitePool) -> i64 {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT json_extract(value_json, '$') FROM settings WHERE key = 'telemetry.retention_days'",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    raw.and_then(|s| s.parse::<i64>().ok())
        .filter(|n| (1..=365).contains(n))
        .unwrap_or(30)
}

/// Delete telemetry rows whose `received_at` is older than `days` days.
/// Table names are hardcoded literals (no injection surface); the cutoff is
/// bound as a parameter. Returns the total number of pruned rows.
async fn prune_telemetry(pool: &SqlitePool, days: i64) -> sqlx::Result<u64> {
    let modifier = format!("-{days} days");
    let mut total = 0u64;
    for table in ["device_logs", "device_metrics", "device_traces"] {
        let sql = format!("DELETE FROM {table} WHERE received_at < datetime('now', ?)");
        let res = sqlx::query(&sql).bind(&modifier).execute(pool).await?;
        total += res.rows_affected();
    }
    Ok(total)
}

async fn resolve_targets(
    pool: &SqlitePool,
    customer_id: i64,
    device_id: Option<i64>,
    group_id: Option<i64>,
    config_id: Option<i64>,
) -> sqlx::Result<Vec<i64>> {
    if let Some(did) = device_id {
        return Ok(vec![did]);
    }
    if let Some(gid) = group_id {
        let ids: Vec<(i64,)> = sqlx::query_as(
            "SELECT d.id FROM devices d \
             JOIN device_groups dg ON dg.device_id = d.id \
             WHERE dg.group_id = ? AND d.customer_id = ? AND d.is_enrolled = 1",
        )
        .bind(gid)
        .bind(customer_id)
        .fetch_all(pool)
        .await?;
        return Ok(ids.into_iter().map(|(i,)| i).collect());
    }
    if let Some(_cid) = config_id {
        // Configuration → all devices that "use" that config. For v1 there
        // is no explicit device_configurations table; treat as customer-wide.
        let ids: Vec<(i64,)> =
            sqlx::query_as("SELECT id FROM devices WHERE customer_id = ? AND is_enrolled = 1")
                .bind(customer_id)
                .fetch_all(pool)
                .await?;
        return Ok(ids.into_iter().map(|(i,)| i).collect());
    }
    // Fall-through: tenant broadcast.
    let ids: Vec<(i64,)> =
        sqlx::query_as("SELECT id FROM devices WHERE customer_id = ? AND is_enrolled = 1")
            .bind(customer_id)
            .fetch_all(pool)
            .await?;
    Ok(ids.into_iter().map(|(i,)| i).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[tokio::test]
    async fn tick_emits_messages_for_direct_device_target() {
        let pool = db::open_pool(":memory:").await.unwrap();

        // Seed an enrolled device.
        sqlx::query(
            "INSERT INTO devices (customer_id, serial, is_enrolled, is_active) \
             VALUES (1, 'T-1', 1, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let device_id: i64 = sqlx::query_scalar("SELECT id FROM devices WHERE serial = 'T-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO push_schedule (customer_id, device_id, command, payload_json, status) \
             VALUES (1, ?, 'reboot', '{}', 'pending')",
        )
        .bind(device_id)
        .execute(&pool)
        .await
        .unwrap();

        let n = tick_once(&pool).await.unwrap();
        assert_eq!(n, 1, "expected one emitted message");

        let cmd_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM push_messages WHERE device_id = ? AND status = 'pending'",
        )
        .bind(device_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(cmd_count, 1);

        let sched_status: String =
            sqlx::query_scalar("SELECT status FROM push_schedule WHERE device_id = ?")
                .bind(device_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(sched_status, "done");
    }

    #[tokio::test]
    async fn tick_skips_future_due_at() {
        let pool = db::open_pool(":memory:").await.unwrap();
        sqlx::query("INSERT INTO devices (customer_id, serial, is_enrolled) VALUES (1, 'F-1', 1)")
            .execute(&pool)
            .await
            .unwrap();
        let device_id: i64 = sqlx::query_scalar("SELECT id FROM devices WHERE serial = 'F-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO push_schedule (customer_id, device_id, command, payload_json, due_at, status) \
             VALUES (1, ?, 'reboot', '{}', datetime('now', '+1 day'), 'pending')",
        )
        .bind(device_id)
        .execute(&pool)
        .await
        .unwrap();
        let n = tick_once(&pool).await.unwrap();
        assert_eq!(n, 0, "future schedules should not be drained");
    }

    #[tokio::test]
    async fn tick_fans_out_to_group_members() {
        let pool = db::open_pool(":memory:").await.unwrap();
        // Two enrolled devices in one group.
        sqlx::query(
            "INSERT INTO devices (customer_id, serial, is_enrolled) VALUES \
                (1, 'G-1', 1), (1, 'G-2', 1), (1, 'G-3', 0)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO groups (customer_id, name) VALUES (1, 'platoon-1')")
            .execute(&pool)
            .await
            .unwrap();
        let group_id: i64 = sqlx::query_scalar("SELECT id FROM groups WHERE name = 'platoon-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        let dev_rows: Vec<(i64,)> =
            sqlx::query_as("SELECT id FROM devices WHERE is_enrolled = 1 ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        for (did,) in &dev_rows {
            sqlx::query("INSERT INTO device_groups (device_id, group_id) VALUES (?, ?)")
                .bind(did)
                .bind(group_id)
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query(
            "INSERT INTO push_schedule (customer_id, group_id, command, payload_json, status) \
             VALUES (1, ?, 'sync-config', '{}', 'pending')",
        )
        .bind(group_id)
        .execute(&pool)
        .await
        .unwrap();

        let n = tick_once(&pool).await.unwrap();
        // Only the 2 enrolled devices receive the message; the un-enrolled one is skipped.
        assert_eq!(n, 2);
    }
}
