//! Integration tests for `/api/v1/configurations`.

mod common;

use common::{TestApp, http_get, http_json, http_request};

async fn create_config(app: &TestApp, name: &str, settings: serde_json::Value) -> i64 {
    let body = http_json(
        "POST",
        &app.url("/api/v1/configurations"),
        &app.admin_token,
        &serde_json::json!({
            "name": name,
            "settings_json": settings.to_string(),
        })
        .to_string(),
        201,
    )
    .await;
    serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap()
}

#[tokio::test]
async fn configurations_crud_happy_path() {
    let app = TestApp::start().await;
    let id = create_config(
        &app,
        "default-config",
        serde_json::json!({"preferredLlm": "qwen3-vl-8b"}),
    )
    .await;

    // GET round-trip preserves settings_json
    let body = http_get(
        &app.url(&format!("/api/v1/configurations/{id}")),
        &app.admin_token,
        200,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let settings: serde_json::Value =
        serde_json::from_str(v["settings_json"].as_str().unwrap()).unwrap();
    assert_eq!(settings["preferredLlm"], "qwen3-vl-8b");

    // UPDATE settings_json
    let new_settings = serde_json::json!({"preferredLlm": "qwen3-vl-30b"}).to_string();
    let body = http_json(
        "PUT",
        &app.url(&format!("/api/v1/configurations/{id}")),
        &app.admin_token,
        &serde_json::json!({"settings_json": new_settings}).to_string(),
        200,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let settings: serde_json::Value =
        serde_json::from_str(v["settings_json"].as_str().unwrap()).unwrap();
    assert_eq!(settings["preferredLlm"], "qwen3-vl-30b");

    // DELETE
    let (status, _) = http_request(
        "DELETE",
        &app.url(&format!("/api/v1/configurations/{id}")),
        Some(&app.admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 204);
}

#[tokio::test]
async fn invalid_settings_json_rejected_on_create() {
    let app = TestApp::start().await;
    let body = http_json(
        "POST",
        &app.url("/api/v1/configurations"),
        &app.admin_token,
        &serde_json::json!({
            "name": "bad",
            "settings_json": "{ not valid",
        })
        .to_string(),
        400,
    )
    .await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], "bad_request");
}

#[tokio::test]
async fn invalid_settings_json_rejected_on_update() {
    let app = TestApp::start().await;
    let id = create_config(&app, "to-update", serde_json::json!({})).await;
    http_json(
        "PUT",
        &app.url(&format!("/api/v1/configurations/{id}")),
        &app.admin_token,
        &serde_json::json!({"settings_json": "{still bad"}).to_string(),
        400,
    )
    .await;
}

#[tokio::test]
async fn config_app_attachment_lifecycle() {
    let app = TestApp::start().await;
    let cfg_id = create_config(&app, "cfg", serde_json::json!({})).await;

    // Create an application to attach
    let body = http_json(
        "POST",
        &app.url("/api/v1/applications"),
        &app.admin_token,
        &serde_json::json!({"package_name": "com.example.attach"}).to_string(),
        201,
    )
    .await;
    let app_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();

    // Attach
    let (status, _) = http_request(
        "POST",
        &app.url(&format!("/api/v1/configurations/{cfg_id}/applications")),
        Some(&app.admin_token),
        None,
        Some(&serde_json::json!({"application_id": app_id}).to_string()),
    )
    .await;
    assert_eq!(status, 204);

    // List attachments
    let body = http_get(
        &app.url(&format!("/api/v1/configurations/{cfg_id}/applications")),
        &app.admin_token,
        200,
    )
    .await;
    let arr: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["application_id"], app_id);
    assert_eq!(arr[0]["mode"], "install");

    // Re-attach same app → 400 (unique constraint)
    let (status, _) = http_request(
        "POST",
        &app.url(&format!("/api/v1/configurations/{cfg_id}/applications")),
        Some(&app.admin_token),
        None,
        Some(&serde_json::json!({"application_id": app_id}).to_string()),
    )
    .await;
    assert_eq!(status, 400);

    // Detach
    let (status, _) = http_request(
        "DELETE",
        &app.url(&format!(
            "/api/v1/configurations/{cfg_id}/applications/{app_id}"
        )),
        Some(&app.admin_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 204);
}
