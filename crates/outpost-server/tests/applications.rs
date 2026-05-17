//! Integration tests for `/api/v1/applications`.

mod common;

use common::{TestApp, http_get, http_json, http_request};

#[tokio::test]
async fn applications_crud_happy_path() {
    let app = TestApp::start().await;

    // CREATE
    let body = http_json(
        "POST",
        &app.url("/api/v1/applications"),
        &app.admin_token,
        &serde_json::json!({
            "package_name": "ru.tacticalar.outpost",
            "display_name": "Outpost",
            "kind": "apk",
        })
        .to_string(),
        201,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let id = v["id"].as_i64().unwrap();
    assert_eq!(v["package_name"], "ru.tacticalar.outpost");
    assert_eq!(v["kind"], "apk");

    // LIST
    let list = http_get(&app.url("/api/v1/applications"), &app.admin_token, 200).await;
    let v: serde_json::Value = serde_json::from_str(&list).unwrap();
    assert_eq!(v["total"], 1);

    // UPDATE
    let body = http_json(
        "PUT",
        &app.url(&format!("/api/v1/applications/{id}")),
        &app.admin_token,
        &serde_json::json!({ "display_name": "Outpost Renamed" }).to_string(),
        200,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["display_name"], "Outpost Renamed");

    // GET
    let body = http_get(
        &app.url(&format!("/api/v1/applications/{id}")),
        &app.admin_token,
        200,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["display_name"], "Outpost Renamed");

    // DELETE
    let (status, _) = http_request(
        "DELETE",
        &app.url(&format!("/api/v1/applications/{id}")),
        Some(&app.admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 204);

    // GET → 404
    let (status, _) = http_request(
        "GET",
        &app.url(&format!("/api/v1/applications/{id}")),
        Some(&app.admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn application_versions_lifecycle() {
    let app = TestApp::start().await;
    let body = http_json(
        "POST",
        &app.url("/api/v1/applications"),
        &app.admin_token,
        &serde_json::json!({"package_name": "com.example.app"}).to_string(),
        201,
    )
    .await;
    let app_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();

    // Create version
    let body = http_json(
        "POST",
        &app.url(&format!("/api/v1/applications/{app_id}/versions")),
        &app.admin_token,
        &serde_json::json!({
            "version_code": 100,
            "version_name": "1.0.0",
            "file_path": "ab/cd/abcd1234.apk",
            "file_size_bytes": 1024,
            "sha256": "abcd1234".repeat(8),
            "is_active": true,
        })
        .to_string(),
        201,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let version_id = v["id"].as_i64().unwrap();
    assert_eq!(v["version_code"], 100);
    assert_eq!(v["is_active"], true);

    // List versions
    let body = http_get(
        &app.url(&format!("/api/v1/applications/{app_id}/versions")),
        &app.admin_token,
        200,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);

    // Duplicate version_code → 400
    http_json(
        "POST",
        &app.url(&format!("/api/v1/applications/{app_id}/versions")),
        &app.admin_token,
        &serde_json::json!({
            "version_code": 100,
            "version_name": "1.0.0-dup",
            "file_path": "ef/gh/efgh.apk",
            "file_size_bytes": 1024,
            "sha256": "efgh".repeat(16),
        })
        .to_string(),
        400,
    )
    .await;

    // Delete version
    let (status, _) = http_request(
        "DELETE",
        &app.url(&format!(
            "/api/v1/applications/{app_id}/versions/{version_id}"
        )),
        Some(&app.admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 204);
}

#[tokio::test]
async fn duplicate_package_name_rejected() {
    let app = TestApp::start().await;
    http_json(
        "POST",
        &app.url("/api/v1/applications"),
        &app.admin_token,
        &serde_json::json!({"package_name": "com.example.dup"}).to_string(),
        201,
    )
    .await;
    let body = http_json(
        "POST",
        &app.url("/api/v1/applications"),
        &app.admin_token,
        &serde_json::json!({"package_name": "com.example.dup"}).to_string(),
        400,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], "bad_request");
}

#[tokio::test]
async fn viewer_cannot_create_application() {
    let app = TestApp::start().await;
    let viewer = app.token_for_role("v1", "ViewerPass123", 4).await;
    let (status, _) = http_request(
        "POST",
        &app.url("/api/v1/applications"),
        Some(&viewer),
        None,
        Some(&serde_json::json!({"package_name": "com.x"}).to_string()),
    )
    .await;
    assert_eq!(status, 403);
}
