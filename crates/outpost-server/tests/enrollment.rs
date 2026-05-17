//! End-to-end enrollment + sync + scheduler flow.
//!
//! Simulates the full device lifecycle:
//! 1. Admin creates a device row
//! 2. Admin generates an enrollment payload (rotated `enrollment_secret`)
//! 3. Device calls `POST /api/v1/enroll` with the payload → receives a device JWT
//! 4. Admin schedules a `reboot` command targeted at the device
//! 5. Scheduler tick fans the schedule out into a `push_messages` row
//! 6. Device calls `POST /api/v1/sync` with its JWT → receives the command
//! 7. Device acks the command on its next sync → `push_messages.status = 'delivered'`

mod common;

use common::{TestApp, http_json, http_request};

#[tokio::test]
async fn enrollment_then_sync_round_trip() {
    let app = TestApp::start().await;

    // 1. Admin creates a device
    let body = http_json(
        "POST",
        &app.url("/api/v1/devices"),
        &app.admin_token,
        &serde_json::json!({"serial": "ENR-001", "display_name": "Field Unit Alpha"}).to_string(),
        201,
    )
    .await;
    let device_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();

    // 2. Admin generates enrollment payload
    let body = http_json(
        "POST",
        &app.url(&format!("/api/v1/devices/{device_id}/enrollment")),
        &app.admin_token,
        "{}", // body is unused
        200,
    )
    .await;
    let enrollment: serde_json::Value = serde_json::from_str(&body).unwrap();
    let enrollment_secret = enrollment["enrollment_secret"].as_str().unwrap();
    assert_eq!(enrollment["device_id"], device_id);

    // 3. Device enrolls (no auth header)
    let body = http_request(
        "POST",
        &app.url("/api/v1/enroll"),
        None,
        None,
        Some(
            &serde_json::json!({
                "device_id": device_id,
                "enrollment_secret": enrollment_secret,
                "os_version": "Android 14",
                "app_version": "0.9.0-rc38",
            })
            .to_string(),
        ),
    )
    .await
    .1;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let device_token = v["device_token"].as_str().unwrap().to_string();
    assert_eq!(v["device_id"], device_id);
    assert!(v["expires_in"].as_i64().unwrap() > 0);

    // Re-enrollment with the same (now-cleared) secret must fail
    let (status, _) = http_request(
        "POST",
        &app.url("/api/v1/enroll"),
        None,
        None,
        Some(
            &serde_json::json!({
                "device_id": device_id,
                "enrollment_secret": enrollment_secret,
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, 401);

    // 4. Admin schedules a reboot for this device
    http_json(
        "POST",
        &app.url("/api/v1/push/schedule"),
        &app.admin_token,
        &serde_json::json!({
            "device_id": device_id,
            "command": "reboot",
            "due_at": "2000-01-01 00:00:00", // past → eligible immediately
        })
        .to_string(),
        201,
    )
    .await;

    // 5. Drive the scheduler tick once (in-process, no waiting on the
    //    real tokio interval).
    let emitted = outpost_server::scheduler::tick_once(&app.pool)
        .await
        .unwrap();
    assert_eq!(emitted, 1);

    // 6. Device syncs and should see the reboot command.
    let body = http_request(
        "POST",
        &app.url("/api/v1/sync"),
        Some(&device_token),
        None,
        Some(
            &serde_json::json!({
                "battery_pct": 87,
                "last_lat": 55.7558,
                "last_lon": 37.6173,
                "acks": [],
            })
            .to_string(),
        ),
    )
    .await
    .1;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let commands = v["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["command"], "reboot");
    let push_id = commands[0]["id"].as_i64().unwrap();

    // 7. Device acks on a later sync; the push transitions to 'delivered'.
    let body = http_request(
        "POST",
        &app.url("/api/v1/sync"),
        Some(&device_token),
        None,
        Some(&serde_json::json!({"acks": [push_id]}).to_string()),
    )
    .await
    .1;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v["commands"].as_array().unwrap().is_empty());

    // Confirm via the admin API
    let body = http_request(
        "GET",
        &app.url(&format!("/api/v1/push/messages/{push_id}")),
        Some(&app.admin_token),
        None,
        None,
    )
    .await
    .1;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["status"], "delivered");
    assert!(v["delivered_at"].as_str().is_some());
}

#[tokio::test]
async fn enroll_with_wrong_secret_returns_401() {
    let app = TestApp::start().await;
    let body = http_json(
        "POST",
        &app.url("/api/v1/devices"),
        &app.admin_token,
        &serde_json::json!({"serial": "ENR-002"}).to_string(),
        201,
    )
    .await;
    let device_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();
    http_json(
        "POST",
        &app.url(&format!("/api/v1/devices/{device_id}/enrollment")),
        &app.admin_token,
        "{}",
        200,
    )
    .await;

    let (status, _) = http_request(
        "POST",
        &app.url("/api/v1/enroll"),
        None,
        None,
        Some(
            &serde_json::json!({
                "device_id": device_id,
                "enrollment_secret": "wrong-secret-value",
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn sync_requires_device_token_not_user_token() {
    let app = TestApp::start().await;
    // Admin user token must NOT work for /api/v1/sync — wrong kind.
    let (status, _) = http_request(
        "POST",
        &app.url("/api/v1/sync"),
        Some(&app.admin_token),
        None,
        Some(&serde_json::json!({"acks": []}).to_string()),
    )
    .await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn me_endpoint_rejects_device_token() {
    let app = TestApp::start().await;
    // Create + enroll a device, get a device token.
    let body = http_json(
        "POST",
        &app.url("/api/v1/devices"),
        &app.admin_token,
        &serde_json::json!({"serial": "ENR-003"}).to_string(),
        201,
    )
    .await;
    let device_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();
    let enrol = http_json(
        "POST",
        &app.url(&format!("/api/v1/devices/{device_id}/enrollment")),
        &app.admin_token,
        "{}",
        200,
    )
    .await;
    let enrolment_secret =
        serde_json::from_str::<serde_json::Value>(&enrol).unwrap()["enrollment_secret"]
            .as_str()
            .unwrap()
            .to_string();
    let body = http_request(
        "POST",
        &app.url("/api/v1/enroll"),
        None,
        None,
        Some(
            &serde_json::json!({
                "device_id": device_id,
                "enrollment_secret": enrolment_secret,
            })
            .to_string(),
        ),
    )
    .await
    .1;
    let device_token = serde_json::from_str::<serde_json::Value>(&body).unwrap()["device_token"]
        .as_str()
        .unwrap()
        .to_string();

    // /me is for users; expect 401 with kind mismatch.
    let (status, _) = http_request(
        "GET",
        &app.url("/api/v1/auth/me"),
        Some(&device_token),
        None,
        None,
    )
    .await;
    assert_eq!(status, 401);
}
