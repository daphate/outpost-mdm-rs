//! Prometheus exposition endpoint (`GET /metrics`).
//!
//! Emits text-format 0.0.4 — the format Prometheus's `scrape_configs`
//! ingests natively. Two families of metrics:
//!
//! 1. **Server self-metrics** — request counters, push queue depth, RSS.
//!    These are observability for the MDM process itself.
//! 2. **Fleet-aggregated device metrics** — counts of OTLP ingested rows
//!    over the last 24 h, broken down by metric name. Cheap rollup query
//!    on `device_metrics` / `device_logs`.
//!
//! Authentication: open to localhost only (Prometheus scrapes via
//! `127.0.0.1:8080/metrics`, never over the internet). The nginx site
//! deliberately does NOT proxy `/metrics` to the outside world.

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/metrics", get(scrape))
}

async fn scrape(State(state): State<AppState>) -> Response {
    let mut out = String::with_capacity(4096);

    // ----- server self-metrics ----------------------------------------------

    out.push_str("# HELP outpost_build_info Build identification\n");
    out.push_str("# TYPE outpost_build_info gauge\n");
    out.push_str(&format!(
        "outpost_build_info{{version=\"{}\"}} 1\n",
        env!("CARGO_PKG_VERSION")
    ));

    // Push queue depth — operational signal: large pending count means
    // devices haven't checked in.
    if let Ok(pending) =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM push_messages WHERE status = 'pending'")
            .fetch_one(&state.db)
            .await
    {
        out.push_str("# HELP outpost_push_pending_total Push messages in 'pending' state.\n");
        out.push_str("# TYPE outpost_push_pending_total gauge\n");
        out.push_str(&format!("outpost_push_pending_total {pending}\n"));
    }

    if let Ok(failed) =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM push_messages WHERE status = 'failed'")
            .fetch_one(&state.db)
            .await
    {
        out.push_str("# HELP outpost_push_failed_total Push messages in 'failed' state.\n");
        out.push_str("# TYPE outpost_push_failed_total gauge\n");
        out.push_str(&format!("outpost_push_failed_total {failed}\n"));
    }

    if let Ok(enrolled) =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM devices WHERE is_enrolled = 1")
            .fetch_one(&state.db)
            .await
    {
        out.push_str(
            "# HELP outpost_devices_enrolled_total Devices that have completed enrollment.\n",
        );
        out.push_str("# TYPE outpost_devices_enrolled_total gauge\n");
        out.push_str(&format!("outpost_devices_enrolled_total {enrolled}\n"));
    }

    if let Ok(online) =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM devices WHERE is_online = 1")
            .fetch_one(&state.db)
            .await
    {
        out.push_str("# HELP outpost_devices_online_total Devices with is_online=1.\n");
        out.push_str("# TYPE outpost_devices_online_total gauge\n");
        out.push_str(&format!("outpost_devices_online_total {online}\n"));
    }

    if let Ok(active_24h) = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(DISTINCT device_id) FROM device_logs WHERE ts >= datetime('now', '-1 day')",
    )
    .fetch_one(&state.db)
    .await
    {
        out.push_str(
            "# HELP outpost_devices_active_24h Distinct devices that sent any OTLP signal in the last 24 h.\n",
        );
        out.push_str("# TYPE outpost_devices_active_24h gauge\n");
        out.push_str(&format!("outpost_devices_active_24h {active_24h}\n"));
    }

    // ----- OTLP ingest counters (24 h windows) -----------------------------

    if let Ok(logs_24h) = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM device_logs WHERE received_at >= datetime('now', '-1 day')",
    )
    .fetch_one(&state.db)
    .await
    {
        out.push_str("# HELP outpost_otlp_logs_24h Log records ingested in the last 24 h.\n");
        out.push_str("# TYPE outpost_otlp_logs_24h gauge\n");
        out.push_str(&format!("outpost_otlp_logs_24h {logs_24h}\n"));
    }

    if let Ok(errors_24h) = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM device_logs WHERE received_at >= datetime('now', '-1 day') AND severity_number >= 17",
    )
    .fetch_one(&state.db)
    .await
    {
        out.push_str(
            "# HELP outpost_otlp_errors_24h Log records with severity_number >= ERROR ingested in the last 24 h.\n",
        );
        out.push_str("# TYPE outpost_otlp_errors_24h gauge\n");
        out.push_str(&format!("outpost_otlp_errors_24h {errors_24h}\n"));
    }

    if let Ok(metrics_24h) = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM device_metrics WHERE received_at >= datetime('now', '-1 day')",
    )
    .fetch_one(&state.db)
    .await
    {
        out.push_str(
            "# HELP outpost_otlp_metrics_24h Metric data-points ingested in the last 24 h.\n",
        );
        out.push_str("# TYPE outpost_otlp_metrics_24h gauge\n");
        out.push_str(&format!("outpost_otlp_metrics_24h {metrics_24h}\n"));
    }

    if let Ok(traces_24h) = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM device_traces WHERE received_at >= datetime('now', '-1 day')",
    )
    .fetch_one(&state.db)
    .await
    {
        out.push_str("# HELP outpost_otlp_traces_24h Spans ingested in the last 24 h.\n");
        out.push_str("# TYPE outpost_otlp_traces_24h gauge\n");
        out.push_str(&format!("outpost_otlp_traces_24h {traces_24h}\n"));
    }

    // ----- top metric names — useful as Prometheus labels for dashboards ----
    // We expose a fixed set of common Outpost metric names so Grafana queries
    // can plot them without high-cardinality label explosion. Each row is a
    // latest sample per metric, scoped to the whole fleet.
    let common_names = [
        "app.session_seconds",
        "app.foreground_ms",
        "app.crashes",
        "app.anr_count",
        "battery.pct",
        "battery.charging",
        "ram.available_mb",
        "storage.free_mb",
        "network.requests_total",
        "network.errors_total",
        "ml.inference_ms",
        "ml.queue_depth",
    ];
    out.push_str(
        "# HELP outpost_metric_latest Latest value per metric name across the fleet (max over devices).\n",
    );
    out.push_str("# TYPE outpost_metric_latest gauge\n");
    for name in common_names {
        if let Ok(Some(v)) = sqlx::query_scalar::<_, Option<f64>>(
            "SELECT MAX(value) FROM device_metrics WHERE name = ?",
        )
        .bind(name)
        .fetch_one(&state.db)
        .await
        {
            // metric name as Prom label rather than as metric name avoids the
            // unbounded cardinality of dynamic metric names.
            out.push_str(&format!(
                "outpost_metric_latest{{name=\"{}\"}} {v}\n",
                escape_prom_label(name)
            ));
        }
    }

    // ----- per-device snapshot — for drill-down dashboards + per-device --------
    // alert rules. Cardinality is bounded by fleet size × curated names (~30 ×
    // 12 = 360 series в текущем prod-сценарии).
    //
    // We expose two families:
    //   1. `outpost_device_metric_latest{device_id, serial, name}` — last
    //      reading per (device, metric name), within the last hour.
    //   2. `outpost_device_last_seen_seconds_ago{device_id, serial}` — derived
    //      from `devices.last_seen_at`. Used by offline alert rules.

    let per_device_rows: Result<Vec<(i64, String, String, f64)>, sqlx::Error> = sqlx::query_as(
        "SELECT m.device_id, d.serial, m.name, m.value \
         FROM device_metrics m \
         JOIN devices d ON d.id = m.device_id \
         WHERE m.ts > datetime('now', '-1 hour') \
           AND m.name IN \
             ('app.session_seconds','app.foreground_ms','app.crashes','app.anr_count',\
              'battery.pct','battery.charging','ram.available_mb','storage.free_mb',\
              'network.requests_total','network.errors_total','ml.inference_ms','ml.queue_depth') \
           AND (m.device_id, m.name, m.ts) IN ( \
             SELECT device_id, name, MAX(ts) FROM device_metrics \
             WHERE ts > datetime('now', '-1 hour') \
             GROUP BY device_id, name \
           )",
    )
    .fetch_all(&state.db)
    .await;
    if let Ok(rows) = per_device_rows {
        out.push_str(
            "# HELP outpost_device_metric_latest Per-device latest reading (1h window).\n",
        );
        out.push_str("# TYPE outpost_device_metric_latest gauge\n");
        for (device_id, serial, name, value) in rows {
            out.push_str(&format!(
                "outpost_device_metric_latest{{device_id=\"{}\",serial=\"{}\",name=\"{}\"}} {}\n",
                device_id,
                escape_prom_label(&serial),
                escape_prom_label(&name),
                value,
            ));
        }
    }

    let last_seen_rows: Result<Vec<(i64, String, Option<i64>)>, sqlx::Error> = sqlx::query_as(
        "SELECT id, serial, \
                CAST((strftime('%s','now') - strftime('%s', last_seen_at)) AS INTEGER) \
         FROM devices WHERE is_enrolled = 1",
    )
    .fetch_all(&state.db)
    .await;
    if let Ok(rows) = last_seen_rows {
        out.push_str(
            "# HELP outpost_device_last_seen_seconds_ago Seconds since last sync per device.\n",
        );
        out.push_str("# TYPE outpost_device_last_seen_seconds_ago gauge\n");
        for (device_id, serial, sec_ago) in rows {
            let v = sec_ago.unwrap_or(-1);
            out.push_str(&format!(
                "outpost_device_last_seen_seconds_ago{{device_id=\"{}\",serial=\"{}\"}} {}\n",
                device_id,
                escape_prom_label(&serial),
                v,
            ));
        }
    }

    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], out).into_response()
}

fn escape_prom_label(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[allow(dead_code)]
fn _kept() -> StatusCode {
    StatusCode::OK
}
