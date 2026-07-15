//! `/api/v1/player/state` — игровой канал класса «игроки STALKER» (Ф4).
//!
//! Приложение net.afterday.compas шлёт сюда позицию вместе с игровым
//! состоянием (радиация, здоровье, угроза, артефакты). Позиция проходит через
//! общий приём [`crate::routes::position::apply_position`] (история + живая
//! шина), игровое состояние — upsert в `player_states` и публикация события
//! `player` в шину для вида `/map/players`.

use crate::auth_extract::{AuthDevice, AuthUser};
use crate::error::ApiError;
use crate::permission::require_permission;
use crate::routes::position::apply_position;
use crate::state::{AppState, LiveEvent};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::post,
};
use serde::Deserialize;
use std::sync::Arc;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/player/state", post(ingest))
        // Гейм-мастерский канал (операторская сторона «Ноосферы»).
        .route("/api/v1/player/{id}/command", post(command_one))
        .route("/api/v1/players/command", post(command_broadcast))
        // DOS-1: все хендлеры разбирают JSON-тело (в т.ч. произвольный payload),
        // а `Json` буферизует и парсит его до проверки права внутри хендлера.
        // Без per-route лимита наследовался бы глобальный 200 MiB → auth-gated
        // OOM (systemd MemoryMax=256M). 256 KiB — с запасом для игровой команды.
        // Совпадает с push.rs/enrollment.rs; покрывает и приём телеметрии игрока.
        .layer(axum::extract::DefaultBodyLimit::max(256 * 1024))
}

#[derive(Debug, Deserialize)]
pub struct PlayerReport {
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub alt: Option<f64>,
    #[serde(default)]
    pub bearing: Option<f64>,
    #[serde(default)]
    pub speed: Option<f64>,
    #[serde(default)]
    pub accuracy: Option<f64>,
    #[serde(default)]
    pub battery_pct: Option<i64>,
    // Игровое состояние.
    #[serde(default)]
    pub radiation: Option<f64>,
    #[serde(default)]
    pub health: Option<f64>,
    #[serde(default)]
    pub threat_level: Option<String>,
    #[serde(default)]
    pub artifacts: Option<i64>,
    /// Произвольное расширение (сериализуется как строка JSON).
    #[serde(default)]
    pub detail: Option<serde_json::Value>,
}

async fn ingest(
    device: AuthDevice,
    State(state): State<AppState>,
    Json(req): Json<PlayerReport>,
) -> Result<StatusCode, ApiError> {
    // Позиция + история + событие "position" — общий код с быстрым каналом.
    apply_position(
        &state,
        device.id,
        device.customer_id,
        req.lat,
        req.lon,
        req.alt,
        req.bearing,
        req.speed,
        req.accuracy,
        req.battery_pct,
    )
    .await?;

    let detail_str = req.detail.as_ref().map(|v| v.to_string());
    sqlx::query(
        "INSERT INTO player_states \
           (device_id, customer_id, radiation, health, threat_level, artifacts, detail_json, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, datetime('now')) \
         ON CONFLICT(device_id) DO UPDATE SET \
           customer_id = excluded.customer_id, \
           radiation = excluded.radiation, \
           health = excluded.health, \
           threat_level = excluded.threat_level, \
           artifacts = excluded.artifacts, \
           detail_json = excluded.detail_json, \
           updated_at = datetime('now')",
    )
    .bind(device.id)
    .bind(device.customer_id)
    .bind(req.radiation)
    .bind(req.health)
    .bind(&req.threat_level)
    .bind(req.artifacts)
    .bind(&detail_str)
    .execute(&state.db)
    .await?;

    let payload = serde_json::json!({
        "device_id": device.id,
        "radiation": req.radiation,
        "health": req.health,
        "threat_level": req.threat_level,
        "artifacts": req.artifacts,
    })
    .to_string();
    state.live.publish(LiveEvent {
        customer_id: device.customer_id,
        name: "player",
        data: Arc::from(payload.as_str()),
    });

    Ok(StatusCode::NO_CONTENT)
}

// ---- Гейм-мастерский канал: реализация игровых механик («Ноосфера») --------
//
// Операторская сторона «Ноосферы» из руководства игротехника ПДА «Компас»:
// центральная система на ходу меняет игровые параметры на полигоне (Выброс,
// угрозы, лечение, воскрешение, «прокол нарушителя» и т.п.). Команда ложится в
// общую очередь `push_messages` (command = "game.*") и доставляется приложению
// net.afterday.compas тем же каналом `POST /api/v1/sync`, что и прочие
// push-команды MDM; приложение подтверждает исполнение через `applied_commands`.
// Исполнение внутри игрового движка приложения — отдельная работа (app-side);
// здесь реализована постановка команд и разграничение доступа.

/// Разрешённый набор игровых команд и нормализация полезной нагрузки. Значения
/// и диапазоны взяты из руководства игротехника (v178) и разбора механик
/// (`pda-compass/out/BUILD-COMPARISON-AND-MECHANICS.md`): унифицированная угроза
/// с коэффициентом силы/радиуса 1..999 (A1 — красной зоны нет, A999 — вся зона
/// смертельна), Выброс (предупреждение + длительность + признак ложного), Оазис
/// (лечение), KILL/REVIVE, смена фракции, опыт (50 = уровень), выдача предмета,
/// «прокол нарушителя», сообщение на ПДА. Неизвестная команда отвергается.
fn validate_game_command(
    command: &str,
    payload: &serde_json::Value,
) -> Result<serde_json::Value, ApiError> {
    use serde_json::json;
    let bad = |m: &str| ApiError::BadRequest(m.to_string());
    let obj = payload.as_object().cloned().unwrap_or_default();
    let int_or = |k: &str, d: i64| obj.get(k).and_then(|v| v.as_i64()).unwrap_or(d);
    let str_of = |k: &str| {
        obj.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    };
    let out = match command {
        // Его Величество Выброс: синхронно у всех; fake — тревога без урона.
        "game.emission" => json!({
            "warn_sec": int_or("warn_sec", 0).clamp(0, 3600),
            "duration_sec": int_or("duration_sec", 0).clamp(0, 3600),
            "fake": obj.get("fake").and_then(|v| v.as_bool()).unwrap_or(false),
        }),
        // Унифицированная угроза: тип по букве SSID, коэффициент — сила/радиус.
        // Исключение: Зов Монолита (Z) — по руководству его сила от коэффициента
        // НЕ зависит (правило коэффициента действует только для R,A,M,B,C),
        // поэтому для Z коэффициент не принимаем и не передаём.
        "game.threat" => {
            let t = str_of("type");
            if !["R", "A", "M", "C", "B", "Z"].contains(&t.as_str()) {
                return Err(bad("type должен быть один из R,A,M,C,B,Z"));
            }
            if t == "Z" {
                json!({ "type": t, "duration_sec": int_or("duration_sec", 0).clamp(0, 3600) })
            } else {
                json!({
                    "type": t,
                    "coeff": int_or("coeff", 100).clamp(1, 999),
                    "duration_sec": int_or("duration_sec", 0).clamp(0, 3600),
                })
            }
        }
        // Оазис/лечение (H): бинарно в радиусе, коэффициент из имени модуля.
        "game.heal" => json!({
            "coeff": int_or("coeff", 100).clamp(1, 999),
            "duration_sec": int_or("duration_sec", 0).clamp(0, 3600),
        }),
        "game.kill" => json!({}),
        "game.revive" => json!({ "health_pct": int_or("health_pct", 100).clamp(1, 100) }),
        "game.faction" => {
            let f = str_of("faction");
            if !["STALKER", "MONOLITH", "GAMEMASTER", "DARKEN"].contains(&f.as_str()) {
                return Err(bad("неизвестная фракция"));
            }
            json!({ "faction": f })
        }
        "game.grant_xp" => json!({ "xp": int_or("xp", 50).clamp(1, 100_000) }),
        "game.item" => {
            let code = str_of("code");
            if code.is_empty() || code.chars().count() > 128 {
                return Err(bad("code предмета обязателен (до 128 символов)"));
            }
            json!({ "code": code })
        }
        "game.violator_mark" => {
            let reason = str_of("reason");
            if reason.chars().count() > 200 {
                return Err(bad("reason слишком длинный (до 200 символов)"));
            }
            json!({ "reason": reason })
        }
        "game.message" => {
            let text = str_of("text");
            if text.is_empty() || text.chars().count() > 500 {
                return Err(bad("text обязателен (до 500 символов)"));
            }
            json!({ "text": text })
        }
        _ => return Err(bad("неизвестная игровая команда")),
    };
    Ok(out)
}

#[derive(Debug, Deserialize)]
pub struct GameCommandRequest {
    pub command: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// Поставить игровую команду в очередь доставки (`push_messages`, статус
/// `pending`) для одного устройства. Возвращает id команды.
async fn enqueue_game(
    state: &AppState,
    customer_id: i64,
    device_id: i64,
    command: &str,
    payload: &serde_json::Value,
) -> Result<i64, ApiError> {
    // Метка времени постановки (UTC): срочные события (Выброс, угроза) — по
    // расписанию «синхронно у всех», поэтому приложение должно уметь отбраковать
    // устаревшую команду, доставленную после переподключения. Серверная защита
    // от заведомо протухших — фильтр в дренаже /api/v1/sync (см. enrollment.rs).
    let mut obj = payload.as_object().cloned().unwrap_or_default();
    obj.insert(
        "issued_at".to_string(),
        serde_json::json!(chrono::Utc::now().to_rfc3339()),
    );
    let payload_json = serde_json::Value::Object(obj).to_string();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO push_messages (customer_id, device_id, command, payload_json, status) \
         VALUES (?, ?, ?, ?, 'pending') RETURNING id",
    )
    .bind(customer_id)
    .bind(device_id)
    .bind(command)
    .bind(&payload_json)
    .fetch_one(&state.db)
    .await?;
    Ok(id)
}

/// Убедиться, что устройство — игрок STALKER этого арендатора и попадает в
/// область видимости оператора (customer + unit-scope). Иначе — NotFound
/// (не раскрываем существование чужих устройств).
async fn ensure_player_in_scope(
    state: &AppState,
    user: &AuthUser,
    device_id: i64,
) -> Result<(), ApiError> {
    let scope = crate::unit_scope::resolve(state, user).await?;
    let owned: Option<i64> = sqlx::query_scalar(&format!(
        "SELECT 1 FROM devices WHERE id = ? AND customer_id = ? \
           AND device_class = 'stalker_player' AND {}",
        crate::unit_scope::UnitScope::CLAUSE
    ))
    .bind(device_id)
    .bind(user.customer_id)
    .bind(scope.scoped_flag())
    .bind(&scope.ids_json)
    .fetch_optional(&state.db)
    .await?;
    if owned.is_none() {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

/// `POST /api/v1/player/{id}/command` — игровая команда одному игроку.
async fn command_one(
    user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(req): Json<GameCommandRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&state.db, user.role_id, "push.send").await?;
    let payload = validate_game_command(&req.command, &req.payload)?;
    ensure_player_in_scope(&state, &user, id).await?;
    let cmd_id = enqueue_game(&state, user.customer_id, id, &req.command, &payload).await?;
    Ok(Json(
        serde_json::json!({ "command_id": cmd_id, "device_id": id }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct BroadcastRequest {
    pub command: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Слать только устройствам «в сети» (по умолчанию — всем в области).
    #[serde(default)]
    pub only_online: bool,
}

/// `POST /api/v1/players/command` — «Ноосфера»: одна игровая команда всем
/// игрокам в области видимости оператора (customer + unit-scope). Возвращает
/// число поставленных в очередь команд.
async fn command_broadcast(
    user: AuthUser,
    State(state): State<AppState>,
    Json(req): Json<BroadcastRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_permission(&state.db, user.role_id, "push.send").await?;
    let payload = validate_game_command(&req.command, &req.payload)?;
    let scope = crate::unit_scope::resolve(&state, &user).await?;
    let ids: Vec<i64> = sqlx::query_scalar(&format!(
        "SELECT id FROM devices \
         WHERE customer_id = ? AND device_class = 'stalker_player' \
           AND (? = 0 OR is_online = 1) AND {}",
        crate::unit_scope::UnitScope::CLAUSE
    ))
    .bind(user.customer_id)
    .bind(req.only_online as i64)
    .bind(scope.scoped_flag())
    .bind(&scope.ids_json)
    .fetch_all(&state.db)
    .await?;
    let mut count = 0i64;
    for did in &ids {
        enqueue_game(&state, user.customer_id, *did, &req.command, &payload).await?;
        count += 1;
    }
    Ok(Json(
        serde_json::json!({ "count": count, "command": req.command }),
    ))
}
