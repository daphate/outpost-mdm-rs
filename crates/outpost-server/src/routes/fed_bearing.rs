//! Федерация с ЦУП «Пеленг» (Ф3, антидрон).
//!
//! MDM не переписывает фьюжн «Пеленга», а отражает его оперативную картинку
//! для отдельного вида карты `/map/antidrone`. Обработчик ходит на сервер
//! «Пеленга» (`{BEARING_BASE_URL}/v1/*`) с токеном федерации
//! (`BEARING_FED_TOKEN`, роль viewer), собирает узлы, пеленги, фиксы, треки и
//! тревоги за один заход и отдаёт их клиенту одним ответом.
//!
//! Преобразования на стороне адаптера:
//!  - углы «Пеленга» математические (против часовой от востока) → компасные
//!    (по часовой от севера): `compass = (90 − math) mod 360`;
//!  - онлайн узла считается здесь по порогу 120 с от `last_seen`
//!    (то же правило, что в UI «Пеленга»).
//!
//! Федерация включается только если заданы обе переменные окружения; иначе
//! обработчик отвечает 503 «не настроено».

use crate::auth_extract::AuthUser;
use crate::error::ApiError;
use crate::permission::require_permission;
use crate::state::AppState;
use axum::{Json, Router, extract::State, routing::get};
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::time::Duration;

pub fn router() -> Router<AppState> {
    Router::new().route("/api/v1/antidrone/scene", get(scene))
}

const CLASS_NAMES: [&str; 4] = ["background", "piston", "electric", "jet"];

struct Fed {
    base: String,
    token: String,
}

/// Конфигурация федерации из окружения, читается один раз. `None` — выключено.
fn fed_config() -> Option<&'static Fed> {
    static CFG: OnceLock<Option<Fed>> = OnceLock::new();
    CFG.get_or_init(|| {
        let base = std::env::var("BEARING_BASE_URL")
            .ok()
            .filter(|s| !s.is_empty())?;
        let token = std::env::var("BEARING_FED_TOKEN")
            .ok()
            .filter(|s| !s.is_empty())?;
        Some(Fed {
            base: base.trim_end_matches('/').to_string(),
            token,
        })
    })
    .as_ref()
}

/// Настроена ли федерация с ЦУП «Пеленг» на этом инстансе (без похода в сеть).
/// Используется сводкой антидронного тенанта.
pub fn is_configured() -> bool {
    fed_config().is_some()
}

fn http_client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(Duration::from_secs(8))
                .build()
                .ok()
        })
        .as_ref()
}

async fn fetch_json(fed: &Fed, path: &str) -> Result<Value, ApiError> {
    let client = http_client()
        .ok_or_else(|| ApiError::ServiceUnavailable("http client init failed".into()))?;
    let resp = client
        .get(format!("{}{}", fed.base, path))
        .header("Authorization", format!("Bearer {}", fed.token))
        .send()
        .await
        .map_err(|e| ApiError::ServiceUnavailable(format!("«Пеленг» недоступен: {e}")))?;
    if !resp.status().is_success() {
        return Err(ApiError::ServiceUnavailable(format!(
            "«Пеленг» {path} → HTTP {}",
            resp.status().as_u16()
        )));
    }
    // reqwest в этой сборке без feature `json` — читаем текст и парсим сами.
    let body = resp
        .text()
        .await
        .map_err(|e| ApiError::ServiceUnavailable(format!("«Пеленг» чтение тела: {e}")))?;
    serde_json::from_str::<Value>(&body)
        .map_err(|e| ApiError::ServiceUnavailable(format!("«Пеленг» вернул не-JSON: {e}")))
}

/// Математический угол (против часовой от востока) → компасный (по часовой от севера).
fn compass(math_deg: f64) -> f64 {
    (90.0 - math_deg).rem_euclid(360.0)
}

fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Онлайн, если последнее событие моложе 120 с (правило UI «Пеленга»).
fn is_online(last_seen: Option<f64>, now: f64) -> bool {
    matches!(last_seen, Some(ls) if now - ls < 120.0)
}

fn f(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(|x| x.as_f64())
}
fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(str::to_string)
}

fn transform_nodes(raw: &Value, now: f64) -> Value {
    let features: Vec<Value> = raw
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|n| {
                    let lat = f(n, "lat")?;
                    let lon = f(n, "lon")?;
                    Some(json!({
                        "type": "Feature",
                        "geometry": {"type": "Point", "coordinates": [lon, lat]},
                        "properties": {
                            "node_id": s(n, "node_id"),
                            "name": s(n, "name"),
                            "status": s(n, "status"),
                            "online": is_online(f(n, "last_seen"), now),
                            "last_seen": f(n, "last_seen"),
                            "yaw_compass": f(n, "yaw_deg").map(compass),
                            "enabled": n.get("enabled").and_then(|x| x.as_i64()),
                        }
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    json!({"type": "FeatureCollection", "features": features})
}

fn transform_bearings(raw: &Value) -> Value {
    let items: Vec<Value> = raw
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|b| {
                    let lat = f(b, "lat")?;
                    let lon = f(b, "lon")?;
                    let az = f(b, "az")?;
                    Some(json!({
                        "node_id": s(b, "node_id"),
                        "lat": lat, "lon": lon,
                        "az_compass": compass(az),
                        "q": f(b, "q"),
                        "cls_name": s(b, "cls_name"),
                        "verdict": s(b, "verdict"),
                        "confirmed": b.get("confirmed").and_then(|x| x.as_i64()),
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    Value::Array(items)
}

fn cls_name(v: &Value) -> Option<String> {
    v.get("cls")
        .and_then(|x| x.as_i64())
        .and_then(|i| CLASS_NAMES.get(i as usize))
        .map(|s| s.to_string())
}

fn transform_fixes(raw: &Value) -> Value {
    let features: Vec<Value> = raw
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|x| {
                    let lat = f(x, "lat")?;
                    let lon = f(x, "lon")?;
                    Some(json!({
                        "type": "Feature",
                        "geometry": {"type": "Point", "coordinates": [lon, lat]},
                        "properties": {
                            "cls_name": cls_name(x),
                            "sigma_m": f(x, "sigma_m"),
                            "semi_major_m": f(x, "semi_major_m"),
                            "semi_minor_m": f(x, "semi_minor_m"),
                            "method": s(x, "method"),
                            "n_nodes": x.get("n_nodes").and_then(|v| v.as_i64()),
                            "track_id": x.get("track_id").and_then(|v| v.as_i64()),
                        }
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    json!({"type": "FeatureCollection", "features": features})
}

fn transform_tracks(raw: &Value) -> Value {
    let items: Vec<Value> = raw
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let lat = f(t, "lat")?;
                    let lon = f(t, "lon")?;
                    Some(json!({
                        "lat": lat, "lon": lon,
                        "cls_name": s(t, "cls_name"),
                        "heading_compass": f(t, "heading_deg").map(compass),
                        "speed_ms": f(t, "speed_ms"),
                        "n_fixes": t.get("n_fixes").and_then(|x| x.as_i64()),
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    Value::Array(items)
}

fn transform_alerts(raw: &Value) -> Value {
    let items: Vec<Value> = raw
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|a| {
                    json!({
                        "node": s(a, "node"),
                        "cls_name": s(a, "cls_name"),
                        "kind": s(a, "kind"),
                        "verdict": s(a, "verdict"),
                        "lat": f(a, "lat"),
                        "lon": f(a, "lon"),
                        "az_compass": f(a, "az").map(compass),
                        "heading_compass": f(a, "heading_deg").map(compass),
                        "speed_ms": f(a, "speed_ms"),
                        "confirmed": a.get("confirmed").and_then(|x| x.as_i64()),
                        "ts": f(a, "ts"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Value::Array(items)
}

/// Единый снимок картины «Пеленга» для вида `/map/antidrone`. Узлы обязательны;
/// остальные слои деградируют до пустых, если их запрос не удался, — карта
/// остаётся полезной, если жив хотя бы реестр узлов.
async fn scene(user: AuthUser, State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    require_permission(&state.db, user.role_id, "devices.read").await?;
    let fed = fed_config().ok_or_else(|| {
        ApiError::ServiceUnavailable(
            "федерация с «Пеленгом» не настроена (BEARING_BASE_URL / BEARING_FED_TOKEN)".into(),
        )
    })?;

    let (nodes, bearings, fixes, tracks, alerts) = tokio::join!(
        fetch_json(fed, "/v1/nodes"),
        fetch_json(fed, "/v1/bearings?window_s=30"),
        fetch_json(fed, "/v1/fixes?limit=200"),
        fetch_json(fed, "/v1/tracks"),
        fetch_json(fed, "/v1/alerts?limit=100"),
    );

    let now = now_unix();
    let empty = json!([]);
    Ok(Json(json!({
        "server_now": now,
        "nodes": transform_nodes(&nodes?, now),
        "bearings": transform_bearings(bearings.as_ref().unwrap_or(&empty)),
        "fixes": transform_fixes(fixes.as_ref().unwrap_or(&empty)),
        "tracks": transform_tracks(tracks.as_ref().unwrap_or(&empty)),
        "alerts": transform_alerts(alerts.as_ref().unwrap_or(&empty)),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compass_conversion() {
        assert_eq!(compass(0.0), 90.0); // восток → 90° по компасу
        assert_eq!(compass(90.0), 0.0); // север → 0°
        assert_eq!(compass(180.0), 270.0); // запад → 270°
        assert_eq!(compass(-90.0), 180.0); // юг → 180°
    }

    #[test]
    fn online_threshold() {
        assert!(is_online(Some(1000.0), 1100.0)); // 100 с назад — онлайн
        assert!(!is_online(Some(1000.0), 1200.0)); // 200 с назад — оффлайн
        assert!(!is_online(None, 1000.0)); // не было событий
    }

    #[test]
    fn nodes_skip_missing_coords() {
        let raw = json!([
            {"node_id": "a", "lat": 55.0, "lon": 37.0, "yaw_deg": 0.0, "last_seen": 1000.0, "status": "installed"},
            {"node_id": "b", "lat": null, "lon": null}
        ]);
        let out = transform_nodes(&raw, 1050.0);
        let feats = out["features"].as_array().unwrap();
        assert_eq!(feats.len(), 1);
        assert_eq!(feats[0]["properties"]["node_id"], "a");
        assert_eq!(feats[0]["properties"]["online"], true);
        assert_eq!(feats[0]["properties"]["yaw_compass"], 90.0);
    }
}
