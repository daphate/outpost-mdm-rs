//! OTLP/HTTP-JSON receiver — ingests OpenTelemetry spans, metrics, and logs
//! sent by Outpost-Android devices.
//!
//! Encoding: `application/json` only. The protobuf wire format is deferred
//! — JSON is enough on the 512 MB box (devices throttle batches anyway,
//! and the protobuf code bloat / `prost` build cost is not worth it for
//! demo-fleet volumes). OTLP/HTTP-JSON is part of the spec and used by
//! the same standard collector.
//!
//! Authentication: each ingest call must carry the device's `device_token`
//! as `Authorization: Bearer <token>` — the same token issued at
//! `/api/v1/enroll`. No anonymous ingest, ever. `customer_id` + `device_id`
//! are pulled from the verified session.
//!
//! Endpoints:
//!   POST /v1/traces   — ExportTraceServiceRequest
//!   POST /v1/metrics  — ExportMetricsServiceRequest
//!   POST /v1/logs     — ExportLogsServiceRequest
//!
//! Response: `application/json` with `{}` (no partial_success indication
//! yet — every well-formed batch is persisted in full or rejected with
//! 400; transport errors are 5xx).

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::post;
use axum::Router;
use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use crate::auth_extract::extract_token;
use crate::session::{self, KIND_DEVICE};
use crate::state::AppState;

/// v0.18.20 (security review DOS-1 follow-up, missed-instance otel): per-batch
/// тело OTLP-ingest. Каждый handler берёт СЫРОЙ `Request<Body>` и сам зовёт
/// `to_bytes(body, …)` — поэтому `DefaultBodyLimit`-слой их НЕ ограничивает
/// (raw Body минует limited-body wrapper), лимит обязан передаваться прямо в
/// `to_bytes`. Раньше передавался `state.max_body_bytes` (глобальный 200 MiB,
/// нужный для APK-аплоадов), и один enrolled device мог прислать ~200MB → JSON
/// Value (inflation ×N) + INSERT-per-record loop → OOM-kill (MemoryMax=256M).
/// 4 MiB — щедро для легитимного device-batch'а телеметрии, на порядок ниже
/// cgroup-потолка. (Hard per-batch record-cap — follow-up.)
const OTEL_MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/traces", post(ingest_traces))
        .route("/v1/metrics", post(ingest_metrics))
        .route("/v1/logs", post(ingest_logs))
}

/// Extract device session from `Authorization: Bearer ...`. Returns
/// `(customer_id, device_id)` on success, or a 401 response.
async fn authenticate_device(
    parts: &mut axum::http::request::Parts,
    state: &AppState,
) -> Result<(i64, i64), Response> {
    let token = extract_token(parts).ok_or_else(|| {
        (StatusCode::UNAUTHORIZED, "missing bearer token").into_response()
    })?;
    let sess = session::verify(&token, &state.db).await.map_err(|_| {
        (StatusCode::UNAUTHORIZED, "invalid or expired token").into_response()
    })?;
    if sess.kind != KIND_DEVICE {
        return Err((StatusCode::UNAUTHORIZED, "device token required").into_response());
    }
    Ok((sess.customer_id, sess.subject_id))
}

// ---------------------------------------------------------------------------
// /v1/logs
// ---------------------------------------------------------------------------

async fn ingest_logs(
    State(state): State<AppState>,
    req: axum::http::Request<axum::body::Body>,
) -> Response {
    let (mut parts, body) = req.into_parts();
    let (customer_id, device_id) = match authenticate_device(&mut parts, &state).await {
        Ok(ids) => ids,
        Err(r) => return r,
    };
    let bytes = match axum::body::to_bytes(body, OTEL_MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "otlp logs body read failed");
            return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response();
        }
    };
    let v: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("malformed OTLP/JSON: {e}"),
            )
                .into_response();
        }
    };
    let mut inserted: i64 = 0;
    let resource_batches = v
        .get("resourceLogs")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    for rb in resource_batches {
        let resource_attrs = attrs_to_json(rb.get("resource").and_then(|r| r.get("attributes")));
        let resource_json = resource_attrs.to_string();
        let scope_batches = rb
            .get("scopeLogs")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        for sb in scope_batches {
            let records = sb
                .get("logRecords")
                .and_then(|x| x.as_array())
                .cloned()
                .unwrap_or_default();
            for rec in records {
                let ts = parse_ts(
                    rec.get("timeUnixNano")
                        .or_else(|| rec.get("observedTimeUnixNano")),
                );
                let severity_number = rec
                    .get("severityNumber")
                    .and_then(|x| x.as_i64())
                    .unwrap_or(9);
                let severity_text = rec
                    .get("severityText")
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| severity_text_from_number(severity_number).to_string());
                let body = rec
                    .get("body")
                    .and_then(extract_value_string)
                    .unwrap_or_default();
                let attrs = attrs_to_json(rec.get("attributes"));
                let trace_id = rec.get("traceId").and_then(|x| x.as_str()).map(String::from);
                let span_id = rec.get("spanId").and_then(|x| x.as_str()).map(String::from);

                let res = sqlx::query(
                    "INSERT INTO device_logs \
                        (customer_id, device_id, ts, severity_number, severity_text, body, \
                         attrs_json, resource_json, trace_id, span_id) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(customer_id)
                .bind(device_id)
                .bind(&ts)
                .bind(severity_number)
                .bind(&severity_text)
                .bind(&body)
                .bind(attrs.to_string())
                .bind(&resource_json)
                .bind(&trace_id)
                .bind(&span_id)
                .execute(&state.db)
                .await;
                match res {
                    Ok(_) => inserted += 1,
                    Err(e) => {
                        tracing::error!(error = %e, "device_logs insert failed");
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "ingest failed",
                        )
                            .into_response();
                    }
                }
            }
        }
    }
    tracing::info!(
        device_id,
        customer_id,
        inserted,
        "otlp logs ingested"
    );
    Json(serde_json::json!({"inserted": inserted})).into_response()
}

// ---------------------------------------------------------------------------
// /v1/metrics
// ---------------------------------------------------------------------------

async fn ingest_metrics(
    State(state): State<AppState>,
    req: axum::http::Request<axum::body::Body>,
) -> Response {
    let (mut parts, body) = req.into_parts();
    let (customer_id, device_id) = match authenticate_device(&mut parts, &state).await {
        Ok(ids) => ids,
        Err(r) => return r,
    };
    let bytes = match axum::body::to_bytes(body, OTEL_MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response(),
    };
    let v: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("malformed OTLP/JSON: {e}"),
            )
                .into_response();
        }
    };
    let mut inserted: i64 = 0;
    let resource_batches = v
        .get("resourceMetrics")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    for rb in resource_batches {
        let resource_attrs = attrs_to_json(rb.get("resource").and_then(|r| r.get("attributes")));
        let resource_json = resource_attrs.to_string();
        let scopes = rb
            .get("scopeMetrics")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        for sb in scopes {
            let metrics = sb
                .get("metrics")
                .and_then(|x| x.as_array())
                .cloned()
                .unwrap_or_default();
            for m in metrics {
                let name = m
                    .get("name")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let unit = m
                    .get("unit")
                    .and_then(|x| x.as_str())
                    .map(String::from);
                // OTLP metric data shape: one of `gauge`, `sum`, `histogram`,
                // `summary`, `exponentialHistogram`. Each carries dataPoints[].
                for (kind_name, key) in [
                    ("gauge", "gauge"),
                    ("sum", "sum"),
                    ("histogram", "histogram"),
                    ("summary", "summary"),
                ] {
                    let Some(node) = m.get(key) else { continue };
                    let Some(points) = node.get("dataPoints").and_then(|x| x.as_array()) else {
                        continue;
                    };
                    for p in points {
                        let ts = parse_ts(p.get("timeUnixNano"));
                        let attrs = attrs_to_json(p.get("attributes"));
                        // Try numeric value names that OTLP uses.
                        let value: f64 = p
                            .get("asDouble")
                            .and_then(|x| x.as_f64())
                            .or_else(|| p.get("asInt").and_then(|x| x.as_f64()))
                            .or_else(|| {
                                p.get("asInt")
                                    .and_then(|x| x.as_str())
                                    .and_then(|s| s.parse::<f64>().ok())
                            })
                            .or_else(|| p.get("count").and_then(|x| x.as_f64()))
                            .or_else(|| {
                                p.get("count")
                                    .and_then(|x| x.as_str())
                                    .and_then(|s| s.parse::<f64>().ok())
                            })
                            .unwrap_or(0.0);
                        let res = sqlx::query(
                            "INSERT INTO device_metrics \
                                (customer_id, device_id, ts, name, kind, value, attrs_json, resource_json, unit) \
                             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        )
                        .bind(customer_id)
                        .bind(device_id)
                        .bind(&ts)
                        .bind(&name)
                        .bind(kind_name)
                        .bind(value)
                        .bind(attrs.to_string())
                        .bind(&resource_json)
                        .bind(&unit)
                        .execute(&state.db)
                        .await;
                        match res {
                            Ok(_) => inserted += 1,
                            Err(e) => {
                                tracing::error!(error = %e, "device_metrics insert failed");
                                return (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    "ingest failed",
                                )
                                    .into_response();
                            }
                        }
                    }
                }
            }
        }
    }
    tracing::info!(
        device_id,
        customer_id,
        inserted,
        "otlp metrics ingested"
    );
    Json(serde_json::json!({"inserted": inserted})).into_response()
}

// ---------------------------------------------------------------------------
// /v1/traces
// ---------------------------------------------------------------------------

async fn ingest_traces(
    State(state): State<AppState>,
    req: axum::http::Request<axum::body::Body>,
) -> Response {
    let (mut parts, body) = req.into_parts();
    let (customer_id, device_id) = match authenticate_device(&mut parts, &state).await {
        Ok(ids) => ids,
        Err(r) => return r,
    };
    let bytes = match axum::body::to_bytes(body, OTEL_MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "body too large").into_response(),
    };
    let v: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("malformed OTLP/JSON: {e}"),
            )
                .into_response();
        }
    };
    let mut inserted: i64 = 0;
    let resource_batches = v
        .get("resourceSpans")
        .and_then(|x| x.as_array())
        .cloned()
        .unwrap_or_default();
    for rb in resource_batches {
        let resource_attrs = attrs_to_json(rb.get("resource").and_then(|r| r.get("attributes")));
        let resource_json = resource_attrs.to_string();
        let scopes = rb
            .get("scopeSpans")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default();
        for sb in scopes {
            let spans = sb
                .get("spans")
                .and_then(|x| x.as_array())
                .cloned()
                .unwrap_or_default();
            for sp in spans {
                let trace_id = sp.get("traceId").and_then(|x| x.as_str()).unwrap_or("");
                let span_id = sp.get("spanId").and_then(|x| x.as_str()).unwrap_or("");
                let parent_span_id = sp
                    .get("parentSpanId")
                    .and_then(|x| x.as_str())
                    .filter(|s| !s.is_empty())
                    .map(String::from);
                let name = sp.get("name").and_then(|x| x.as_str()).unwrap_or("");
                let kind = sp.get("kind").and_then(|x| x.as_i64()).unwrap_or(0);
                let start_ts = parse_ts(sp.get("startTimeUnixNano"));
                let end_ts = parse_ts(sp.get("endTimeUnixNano"));
                let duration_ms = compute_duration_ms(&start_ts, &end_ts);
                let (status_code, status_message) = match sp.get("status") {
                    Some(s) => (
                        s.get("code").and_then(|x| x.as_i64()).unwrap_or(0),
                        s.get("message")
                            .and_then(|x| x.as_str())
                            .map(String::from),
                    ),
                    None => (0, None),
                };
                let attrs = attrs_to_json(sp.get("attributes"));
                let res = sqlx::query(
                    "INSERT INTO device_traces \
                        (customer_id, device_id, trace_id, span_id, parent_span_id, name, kind, \
                         start_ts, end_ts, duration_ms, status_code, status_message, \
                         attrs_json, resource_json) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(customer_id)
                .bind(device_id)
                .bind(trace_id)
                .bind(span_id)
                .bind(&parent_span_id)
                .bind(name)
                .bind(kind)
                .bind(&start_ts)
                .bind(&end_ts)
                .bind(duration_ms)
                .bind(status_code)
                .bind(&status_message)
                .bind(attrs.to_string())
                .bind(&resource_json)
                .execute(&state.db)
                .await;
                match res {
                    Ok(_) => inserted += 1,
                    Err(e) => {
                        tracing::error!(error = %e, "device_traces insert failed");
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "ingest failed",
                        )
                            .into_response();
                    }
                }
            }
        }
    }
    tracing::info!(
        device_id,
        customer_id,
        inserted,
        "otlp traces ingested"
    );
    Json(serde_json::json!({"inserted": inserted})).into_response()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// OTLP timestamps are nanoseconds since the Unix epoch, encoded as either
/// an integer (small workloads) or a string (large nanosecond values that
/// overflow JS i64 — OTLP-JSON spec says use string then). We accept both.
fn parse_ts(node: Option<&Value>) -> String {
    let Some(node) = node else {
        return Utc::now().to_rfc3339();
    };
    let nanos: Option<i128> = match node {
        Value::String(s) => s.parse::<i128>().ok(),
        Value::Number(n) => n.as_i64().map(i128::from).or_else(|| n.as_f64().map(|f| f as i128)),
        _ => None,
    };
    let Some(nanos) = nanos else {
        return Utc::now().to_rfc3339();
    };
    let secs = (nanos / 1_000_000_000) as i64;
    let nanos_part = (nanos % 1_000_000_000) as u32;
    match Utc.timestamp_opt(secs, nanos_part).single() {
        Some(dt) => dt.to_rfc3339(),
        None => Utc::now().to_rfc3339(),
    }
}

fn compute_duration_ms(start: &str, end: &str) -> i64 {
    let Ok(s) = DateTime::parse_from_rfc3339(start) else {
        return 0;
    };
    let Ok(e) = DateTime::parse_from_rfc3339(end) else {
        return 0;
    };
    let delta = e.signed_duration_since(s);
    delta.num_milliseconds().max(0)
}

/// Flatten OTLP `KeyValue[]` into a JSON object. AnyValue variants:
///   stringValue | boolValue | intValue | doubleValue | arrayValue | kvlistValue | bytesValue
fn attrs_to_json(node: Option<&Value>) -> Value {
    let Some(arr) = node.and_then(|x| x.as_array()) else {
        return serde_json::json!({});
    };
    let mut map = serde_json::Map::new();
    for kv in arr {
        let key = match kv.get("key").and_then(|x| x.as_str()) {
            Some(k) => k.to_string(),
            None => continue,
        };
        let val = kv.get("value").map(extract_value).unwrap_or(Value::Null);
        map.insert(key, val);
    }
    Value::Object(map)
}

/// OTLP AnyValue → serde_json::Value. Loses `bytesValue` (rare; emitted as
/// a base64 string per spec).
fn extract_value(node: &Value) -> Value {
    if let Some(s) = node.get("stringValue").and_then(|x| x.as_str()) {
        return Value::String(s.to_string());
    }
    if let Some(b) = node.get("boolValue").and_then(|x| x.as_bool()) {
        return Value::Bool(b);
    }
    if let Some(n) = node.get("intValue") {
        if let Some(i) = n.as_i64() {
            return Value::from(i);
        }
        if let Some(s) = n.as_str() {
            if let Ok(i) = s.parse::<i64>() {
                return Value::from(i);
            }
        }
    }
    if let Some(n) = node.get("doubleValue").and_then(|x| x.as_f64()) {
        return Value::from(n);
    }
    if let Some(arr) = node
        .get("arrayValue")
        .and_then(|x| x.get("values"))
        .and_then(|x| x.as_array())
    {
        return Value::Array(arr.iter().map(extract_value).collect());
    }
    if let Some(kv) = node
        .get("kvlistValue")
        .and_then(|x| x.get("values"))
    {
        return attrs_to_json(Some(kv));
    }
    if let Some(bytes) = node.get("bytesValue").and_then(|x| x.as_str()) {
        return Value::String(bytes.to_string());
    }
    Value::Null
}

fn extract_value_string(node: &Value) -> Option<String> {
    match extract_value(node) {
        Value::String(s) => Some(s),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn severity_text_from_number(n: i64) -> &'static str {
    // OTLP SeverityNumber → SeverityText canonical mapping.
    match n {
        1..=4 => "TRACE",
        5..=8 => "DEBUG",
        9..=12 => "INFO",
        13..=16 => "WARN",
        17..=20 => "ERROR",
        21..=24 => "FATAL",
        _ => "UNSPECIFIED",
    }
}

// `Bytes` import is kept for future raw-body handling (e.g. protobuf).
#[allow(dead_code)]
fn _unused(_b: Bytes) {}
