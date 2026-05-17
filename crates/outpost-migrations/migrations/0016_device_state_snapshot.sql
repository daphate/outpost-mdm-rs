-- v0.13 — Tier-2.5 Settings Sync.
--
-- См. tools/MDM-DEVICE-CONTROL-CONTRACT.md §1 в tactical-ar-hud для full
-- описания. Resumé:
--
-- Устройство шлёт в /api/v1/sync request body:
--   * `state_version` — monotonic integer, увеличивается при каждом set*()
--     в ModelPreferences.
--   * `current_state` — JSON snapshot всех видимых admin'у настроек
--     (preferredLlm/preferredVlm/ttsMode/wakeWordEnabled/answerMode/
--      translatorMode/translatorCloudEnabled/translatorAudioMode/
--      showBuildBadge/cpuThreadCount/logLevel/telemetryEnabled/
--      telemetryEndpoint + has-flags для секретов которые НЕ передаются:
--      telemetry_has_token, cloudru_has_override).
--   * `applied_commands` — outcomes исполнения push-команд из prior sync'ов
--     (status=ok|error, message). Также `acks` — list of UUID command ids
--     которые device применил и просит сервер пометить как delivered.
--
-- Server stores latest snapshot inline в devices table — admin может видеть
-- «вот так сейчас устройство сконфигурировано» без необходимости держать
-- history (это не нужно для core MDM workflow).

ALTER TABLE devices ADD COLUMN current_state_json    TEXT    NOT NULL DEFAULT '{}';
ALTER TABLE devices ADD COLUMN current_state_version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE devices ADD COLUMN current_state_seen_at TEXT;
