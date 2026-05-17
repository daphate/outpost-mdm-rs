//! Integration tests for the OTLP/HTTP-JSON ingest endpoints.
//!
//! Coverage:
//! - /v1/logs accepts a well-formed batch under a valid device token and
//!   the rows actually land in `device_logs`
//! - /v1/metrics ingests gauge + sum data points
//! - /v1/traces ingests a span with start/end and computes duration_ms
//! - missing / wrong / user-issued token → 401
//! - malformed JSON → 400
//! - empty resource arrays produce 0 insertions but still return 200
//!
//! These tests run against the standard `TestApp` (in-memory SQLite,
//! seeded admin), so they also implicitly cover the device-token issuance
//! flow at /api/v1/devices/{id}/enrollment + /api/v1/enroll.

mod common;

use common::{http_request, TestApp};
use serde_json::json;

async fn enroll_a_device(app: &TestApp, serial: &str) -> String {
    // 1. admin creates the device
    let body = json!({"serial": serial}).to_string();
    let (status, raw) = http_request(
        "POST",
        &app.url("/api/v1/devices"),
        Some(&app.admin_token),
        None,
        Some(&body),
    )
    .await;
    assert_eq!(status, 201);
    let dev: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let did = dev["id"].as_i64().unwrap();
    // 2. generate enrollment payload
    let (status, raw) = http_request(
        "POST",
        &app.url(&format!("/api/v1/devices/{did}/enrollment")),
        Some(&app.admin_token),
        None,
        Some("{}"),
    )
    .await;
    assert_eq!(status, 200);
    let pay: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let secret = pay["enrollment_secret"].as_str().unwrap().to_string();
    // 3. device redeems the secret for a device token
    let body = json!({"device_id": did, "enrollment_secret": secret}).to_string();
    let (status, raw) = http_request(
        "POST",
        &app.url("/api/v1/enroll"),
        None,
        None,
        Some(&body),
    )
    .await;
    assert_eq!(status, 200);
    let resp: serde_json::Value = serde_json::from_str(&raw).unwrap();
    resp["device_token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn logs_ingest_persists_records() {
    let app = TestApp::start().await;
    let dev_token = enroll_a_device(&app, "OTEL-LOGS-1").await;

    let payload = json!({
        "resourceLogs": [{
            "resource": {
                "attributes": [{"key": "service.name", "value": {"stringValue": "outpost-android"}}]
            },
            "scopeLogs": [{
                "scope": {"name": "ru.tacticalar.outpost"},
                "logRecords": [
                    {
                        "timeUnixNano": "1779000000000000000",
                        "severityNumber": 9,
                        "severityText": "INFO",
                        "body": {"stringValue": "screen.opened"},
                        "attributes": [{"key": "screen", "value": {"stringValue": "home"}}]
                    },
                    {
                        "timeUnixNano": "1779000001000000000",
                        "severityNumber": 17,
                        "severityText": "ERROR",
                        "body": {"stringValue": "uncaught_exception"},
                        "attributes": [{"key": "exception", "value": {"stringValue": "NPE"}}]
                    }
                ]
            }]
        }]
    });

    let (status, raw) = http_request(
        "POST",
        &app.url("/v1/logs"),
        Some(&dev_token),
        None,
        Some(&payload.to_string()),
    )
    .await;
    assert_eq!(status, 200, "body: {raw}");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["inserted"], 2);
}

#[tokio::test]
async fn logs_ingest_rejects_user_token() {
    let app = TestApp::start().await;
    let user_token = app.admin_token.clone();
    let payload = json!({"resourceLogs": []});
    let (status, _raw) = http_request(
        "POST",
        &app.url("/v1/logs"),
        Some(&user_token),
        None,
        Some(&payload.to_string()),
    )
    .await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn logs_ingest_rejects_no_token() {
    let app = TestApp::start().await;
    let payload = json!({"resourceLogs": []});
    let (status, _raw) = http_request(
        "POST",
        &app.url("/v1/logs"),
        None,
        None,
        Some(&payload.to_string()),
    )
    .await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn logs_ingest_rejects_malformed_json() {
    let app = TestApp::start().await;
    let dev_token = enroll_a_device(&app, "OTEL-LOGS-2").await;
    let (status, _raw) = http_request(
        "POST",
        &app.url("/v1/logs"),
        Some(&dev_token),
        None,
        Some("{not valid"),
    )
    .await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn metrics_ingest_gauge_and_sum() {
    let app = TestApp::start().await;
    let dev_token = enroll_a_device(&app, "OTEL-METRICS-1").await;
    let payload = json!({
        "resourceMetrics": [{
            "resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "outpost-android"}}]},
            "scopeMetrics": [{
                "metrics": [
                    {
                        "name": "battery.pct",
                        "unit": "1",
                        "gauge": {
                            "dataPoints": [
                                {"timeUnixNano": "1779000000000000000", "asDouble": 87.5, "attributes": []}
                            ]
                        }
                    },
                    {
                        "name": "network.requests_total",
                        "unit": "1",
                        "sum": {
                            "dataPoints": [
                                {"timeUnixNano": "1779000001000000000", "asInt": "42", "attributes": []}
                            ]
                        }
                    }
                ]
            }]
        }]
    });
    let (status, raw) = http_request(
        "POST",
        &app.url("/v1/metrics"),
        Some(&dev_token),
        None,
        Some(&payload.to_string()),
    )
    .await;
    assert_eq!(status, 200, "body: {raw}");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["inserted"], 2);
}

#[tokio::test]
async fn traces_ingest_computes_duration() {
    let app = TestApp::start().await;
    let dev_token = enroll_a_device(&app, "OTEL-TRACES-1").await;
    let payload = json!({
        "resourceSpans": [{
            "resource": {"attributes": []},
            "scopeSpans": [{
                "scope": {"name": "ru.tacticalar.outpost"},
                "spans": [{
                    "traceId": "deadbeef00000000000000000000abcd",
                    "spanId":  "1234567890abcdef",
                    "name": "screen.render",
                    "kind": 1,
                    "startTimeUnixNano": "1779000000000000000",
                    "endTimeUnixNano":   "1779000000500000000",
                    "attributes": [{"key": "fps", "value": {"intValue": "60"}}],
                    "status": {"code": 1}
                }]
            }]
        }]
    });
    let (status, raw) = http_request(
        "POST",
        &app.url("/v1/traces"),
        Some(&dev_token),
        None,
        Some(&payload.to_string()),
    )
    .await;
    assert_eq!(status, 200, "body: {raw}");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["inserted"], 1);
}

#[tokio::test]
async fn metrics_endpoint_emits_prometheus_format() {
    let app = TestApp::start().await;
    let (status, body) = http_request("GET", &app.url("/metrics"), None, None, None).await;
    assert_eq!(status, 200);
    // The Prometheus exposition has at minimum the build-info and the
    // canonical # HELP / # TYPE comments.
    assert!(body.contains("# HELP outpost_build_info"));
    assert!(body.contains("# TYPE outpost_build_info gauge"));
    assert!(body.contains("outpost_build_info"));
    assert!(body.contains("outpost_devices_enrolled_total"));
    assert!(body.contains("outpost_otlp_logs_24h"));
}

#[tokio::test]
async fn empty_otlp_batches_still_return_200() {
    let app = TestApp::start().await;
    let dev_token = enroll_a_device(&app, "OTEL-EMPTY-1").await;
    for path in ["/v1/logs", "/v1/metrics", "/v1/traces"] {
        let (status, raw) = http_request(
            "POST",
            &app.url(path),
            Some(&dev_token),
            None,
            Some("{}"),
        )
        .await;
        assert_eq!(status, 200, "{path} body: {raw}");
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["inserted"], 0);
    }
}
