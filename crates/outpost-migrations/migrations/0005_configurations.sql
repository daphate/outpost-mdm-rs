-- Outpost MDM schema: configurations and configuration↔application assignments.
--
-- A `configuration` is a named bundle of policy + apps assigned to one or
-- more devices. Devices reference a single active configuration via their
-- `metadata_json.active_configuration_id` field (no FK column for now —
-- keeps the device row lean; resolution is application-side).

CREATE TABLE configurations (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id     INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    name            TEXT    NOT NULL,
    description     TEXT,
    -- Versioned bundle of arbitrary settings (`preferredLlm`,
    -- `preferredTranslatorLlm`, RAG endpoint overrides, etc.) — JSON.
    settings_json   TEXT    NOT NULL DEFAULT '{}',
    -- When non-NULL, devices on this config receive the kiosk policy.
    kiosk_package   TEXT,
    is_active       INTEGER NOT NULL DEFAULT 1,
    created_at      TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at      TEXT    NOT NULL DEFAULT (datetime('now')),
    UNIQUE (customer_id, name)
);

CREATE INDEX idx_configurations_customer ON configurations(customer_id);

CREATE TABLE configuration_applications (
    id                      INTEGER PRIMARY KEY AUTOINCREMENT,
    configuration_id        INTEGER NOT NULL REFERENCES configurations(id) ON DELETE CASCADE,
    application_id          INTEGER NOT NULL REFERENCES applications(id)   ON DELETE CASCADE,
    -- Pinned version; NULL = always latest.
    application_version_id  INTEGER          REFERENCES application_versions(id) ON DELETE SET NULL,
    -- Mode: 'install' (silent install), 'show' (icon only), 'remove'
    mode                    TEXT    NOT NULL DEFAULT 'install',
    -- Display ordering within the device launcher (0 = top).
    sort_order              INTEGER NOT NULL DEFAULT 0,
    created_at              TEXT    NOT NULL DEFAULT (datetime('now')),
    UNIQUE (configuration_id, application_id)
);

CREATE INDEX idx_config_apps_config ON configuration_applications(configuration_id);
CREATE INDEX idx_config_apps_app    ON configuration_applications(application_id);
