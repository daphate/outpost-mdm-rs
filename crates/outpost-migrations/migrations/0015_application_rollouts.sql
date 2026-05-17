-- v0.12.0 — Tier 2 APK update lifecycle.
--
-- Three additions:
--
--   1. `devices.app_version_code` — устройство теперь шлёт integer versionCode
--      в /api/v1/sync (rc42 b37+), а не только string version_name. Сравнения
--      "у устройства версия ниже target" работают по числам, не по парсингу.
--
--   2. `devices.pinned_version_id` — per-device pin. Set'нул → ровно эту
--      версию ставим, что бы ни говорили rollouts. NULL → следуем policy.
--
--   3. `application_rollouts` — staged rollout policy. Каждая строка — это
--      назначение "вот эта версия => вот этой группе устройств => в этой
--      фазе". При phase='canary' — только устройствам из group_id.
--      При phase='fleet' — всем (group_id IGNORE'ится, NULL по convention).
--      При phase='paused' — никому. При phase='rolled_back' — никому (но
--      audit-trail сохраняется).
--
--      Canary auto-promotion: если canary_until_at < datetime('now')
--      и phase = 'canary' — background task переключает на 'fleet'.
--      Если за canary период ml.inference_error rate (errors_24h /
--      total_24h * 100) превысил `crash_threshold_pct` — переключает
--      на 'rolled_back'.
--
-- Поведение /api/v1/sync (для каждого устройства):
--
--   target_version_id =
--     COALESCE(
--       devices.pinned_version_id,                 -- 1. per-device pin
--       (latest application_rollouts с phase='fleet' для application_id), -- 2. fleet
--       (application_rollouts с phase='canary' AND device_id IN group_id)) -- 3. canary
--
--   если target.version_code > device.app_version_code → update_available

ALTER TABLE devices
    ADD COLUMN app_version_code INTEGER;

ALTER TABLE devices
    ADD COLUMN pinned_version_id INTEGER
    REFERENCES application_versions(id) ON DELETE SET NULL;

CREATE INDEX idx_devices_pinned_version ON devices(pinned_version_id);

CREATE TABLE application_rollouts (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    application_id    INTEGER NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
    -- Какую версию катим.
    target_version_id INTEGER NOT NULL REFERENCES application_versions(id) ON DELETE CASCADE,
    -- Группа устройств в фазе canary. NULL → fleet-wide (значит phase=='fleet'
    -- с самого начала, без canary).
    group_id          INTEGER REFERENCES groups(id) ON DELETE SET NULL,
    -- Жизненный цикл:
    --   'canary'      — катим только в group_id, ждём canary_until_at.
    --   'fleet'       — катим всем (auto-promoted из canary или admin
    --                    создал сразу fleet-rollout без group_id).
    --   'paused'      — admin поставил на паузу, никому не отдаём.
    --   'rolled_back' — auto-rollback по crash-threshold (или admin вручную).
    phase             TEXT    NOT NULL DEFAULT 'canary'
                                CHECK (phase IN ('canary', 'fleet', 'paused', 'rolled_back')),
    -- Когда canary должна auto-промоутиться в fleet. NULL для fleet-rollouts.
    canary_until_at   TEXT,
    -- Crash-rate gate в процентах [0, 100]. По дефолту 5%: если в canary-фазе
    -- error_rate превышает это значение — auto-rollback.
    crash_threshold_pct REAL  NOT NULL DEFAULT 5.0,
    -- Audit trail
    created_at        TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at        TEXT    NOT NULL DEFAULT (datetime('now')),
    created_by        INTEGER REFERENCES users(id) ON DELETE SET NULL,
    notes             TEXT,
    -- При manual/auto rollback — кто и почему.
    rolled_back_at    TEXT,
    rolled_back_reason TEXT
);

CREATE INDEX idx_application_rollouts_app   ON application_rollouts(application_id);
CREATE INDEX idx_application_rollouts_phase ON application_rollouts(application_id, phase);
CREATE INDEX idx_application_rollouts_group ON application_rollouts(group_id);

-- В системных настройках — частота auto-promote / auto-rollback ticker'а.
-- Default 60 секунд; clamp в коде [10, 600].
INSERT INTO settings (key, value_json, description)
VALUES ('rollout.monitor_tick_secs', '"60"', 'Rollout monitor tick (canary promote + crash auto-rollback), seconds')
ON CONFLICT(key) DO NOTHING;
