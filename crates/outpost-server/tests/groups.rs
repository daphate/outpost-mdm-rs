//! Integration tests for `/api/v1/groups` + device membership.

mod common;

use common::{TestApp, http_get, http_json, http_request};

async fn create_device(app: &TestApp, serial: &str) -> i64 {
    let body = http_json(
        "POST",
        &app.url("/api/v1/devices"),
        &app.admin_token,
        &serde_json::json!({"serial": serial}).to_string(),
        201,
    )
    .await;
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn create_group(app: &TestApp, name: &str) -> i64 {
    let body = http_json(
        "POST",
        &app.url("/api/v1/groups"),
        &app.admin_token,
        &serde_json::json!({"name": name}).to_string(),
        201,
    )
    .await;
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap()
}

#[tokio::test]
async fn groups_crud_happy_path() {
    let app = TestApp::start().await;
    let id = create_group(&app, "platoon-1").await;

    // LIST
    let body = http_get(&app.url("/api/v1/groups"), &app.admin_token, 200).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["total"], 1);

    // UPDATE
    let body = http_json(
        "PUT",
        &app.url(&format!("/api/v1/groups/{id}")),
        &app.admin_token,
        &serde_json::json!({"description": "First platoon"}).to_string(),
        200,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["description"], "First platoon");

    // DELETE
    let (status, _) = http_request(
        "DELETE",
        &app.url(&format!("/api/v1/groups/{id}")),
        Some(&app.admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 204);
}

#[tokio::test]
async fn duplicate_group_name_rejected() {
    let app = TestApp::start().await;
    create_group(&app, "alpha").await;
    let body = http_json(
        "POST",
        &app.url("/api/v1/groups"),
        &app.admin_token,
        &serde_json::json!({"name": "alpha"}).to_string(),
        400,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], "bad_request");
}

#[tokio::test]
async fn group_device_membership_lifecycle() {
    let app = TestApp::start().await;
    let group_id = create_group(&app, "bravo").await;
    let d1 = create_device(&app, "B-001").await;
    let d2 = create_device(&app, "B-002").await;

    // Add both devices
    for d in [d1, d2] {
        let (status, _) = http_request(
            "POST",
            &app.url(&format!("/api/v1/groups/{group_id}/devices")),
            Some(&app.admin_token),
            None,
            Some(&serde_json::json!({"device_id": d}).to_string()),
        )
        .await;
        assert_eq!(status, 204);
    }

    // List members
    let body = http_get(
        &app.url(&format!("/api/v1/groups/{group_id}/devices")),
        &app.admin_token,
        200,
    )
    .await;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(arr.len(), 2);

    // Remove one
    let (status, _) = http_request(
        "DELETE",
        &app.url(&format!("/api/v1/groups/{group_id}/devices/{d1}")),
        Some(&app.admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 204);

    let body = http_get(
        &app.url(&format!("/api/v1/groups/{group_id}/devices")),
        &app.admin_token,
        200,
    )
    .await;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], d2);
}

#[tokio::test]
async fn adding_unknown_device_to_group_400() {
    let app = TestApp::start().await;
    let group_id = create_group(&app, "charlie").await;
    let (status, body) = http_request(
        "POST",
        &app.url(&format!("/api/v1/groups/{group_id}/devices")),
        Some(&app.admin_token),
        None,
        Some(&serde_json::json!({"device_id": 99999}).to_string()),
    )
    .await;
    assert_eq!(status, 400);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], "bad_request");
}

#[tokio::test]
async fn missing_group_returns_404() {
    let app = TestApp::start().await;
    let (status, _) = http_request(
        "GET",
        &app.url("/api/v1/groups/99999"),
        Some(&app.admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 404);
}
