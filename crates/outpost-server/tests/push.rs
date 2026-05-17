//! Integration tests for `/api/v1/push/{messages,schedule}`.

mod common;

use common::{TestApp, http_get, http_json, http_request};

#[tokio::test]
async fn create_schedule_with_due_at_succeeds() {
    let app = TestApp::start().await;
    let body = http_json(
        "POST",
        &app.url("/api/v1/push/schedule"),
        &app.admin_token,
        &serde_json::json!({
            "command": "reboot",
            "due_at": "2099-01-01 00:00:00",
        })
        .to_string(),
        201,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["command"], "reboot");
    assert_eq!(v["status"], "pending");
    assert!(v["id"].as_i64().is_some());
}

#[tokio::test]
async fn missing_due_at_and_cron_rejected() {
    let app = TestApp::start().await;
    let body = http_json(
        "POST",
        &app.url("/api/v1/push/schedule"),
        &app.admin_token,
        &serde_json::json!({"command": "sync"}).to_string(),
        400,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], "bad_request");
}

#[tokio::test]
async fn multiple_targets_rejected() {
    let app = TestApp::start().await;
    let body = http_json(
        "POST",
        &app.url("/api/v1/push/schedule"),
        &app.admin_token,
        &serde_json::json!({
            "command": "x",
            "device_id": 1,
            "group_id": 2,
            "due_at": "2099-01-01 00:00:00",
        })
        .to_string(),
        400,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], "bad_request");
}

#[tokio::test]
async fn invalid_payload_json_rejected() {
    let app = TestApp::start().await;
    http_json(
        "POST",
        &app.url("/api/v1/push/schedule"),
        &app.admin_token,
        &serde_json::json!({
            "command": "x",
            "payload_json": "{ not valid",
            "due_at": "2099-01-01 00:00:00",
        })
        .to_string(),
        400,
    )
    .await;
}

#[tokio::test]
async fn empty_command_rejected() {
    let app = TestApp::start().await;
    http_json(
        "POST",
        &app.url("/api/v1/push/schedule"),
        &app.admin_token,
        &serde_json::json!({
            "command": "   ",
            "due_at": "2099-01-01 00:00:00",
        })
        .to_string(),
        400,
    )
    .await;
}

#[tokio::test]
async fn cancel_schedule_transitions_status() {
    let app = TestApp::start().await;
    let body = http_json(
        "POST",
        &app.url("/api/v1/push/schedule"),
        &app.admin_token,
        &serde_json::json!({"command": "x", "due_at": "2099-01-01 00:00:00"}).to_string(),
        201,
    )
    .await;
    let id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();

    let (status, _) = http_request(
        "DELETE",
        &app.url(&format!("/api/v1/push/schedule/{id}")),
        Some(&app.admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 204);

    let body = http_get(&app.url("/api/v1/push/schedule"), &app.admin_token, 200).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let item = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id)
        .unwrap();
    assert_eq!(item["status"], "cancelled");
}

#[tokio::test]
async fn list_messages_empty_initially() {
    let app = TestApp::start().await;
    let body = http_get(&app.url("/api/v1/push/messages"), &app.admin_token, 200).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["total"], 0);
    assert!(v["items"].as_array().unwrap().is_empty());
}
