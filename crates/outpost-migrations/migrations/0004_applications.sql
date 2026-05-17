-- Outpost MDM schema: applications and versioned releases.
--
-- An `application` is identified by an Android package name. Multiple
-- `application_versions` rows track APK release history; each carries a
-- sha256 + on-disk path to the file (file content never stored in SQLite).

CREATE TABLE applications (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id  INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    package_name TEXT    NOT NULL,
    display_name TEXT,
    description  TEXT,
    -- Outpost-specific tag: 'apk', 'llm-model', 'mmproj', 'whisper', 'tts',
    -- 'knowledge-db', 'mbtiles', 'config'. Drives device-side handling.
    kind         TEXT    NOT NULL DEFAULT 'apk',
    icon_path    TEXT,
    created_at   TEXT    NOT NULL DEFAULT (datetime('now')),
    updated_at   TEXT    NOT NULL DEFAULT (datetime('now')),
    UNIQUE (customer_id, package_name)
);

CREATE INDEX idx_applications_customer ON applications(customer_id);
CREATE INDEX idx_applications_kind ON applications(kind);

CREATE TABLE application_versions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    application_id  INTEGER NOT NULL REFERENCES applications(id) ON DELETE CASCADE,
    -- Android versionCode (integer) — monotonic per application.
    version_code    INTEGER NOT NULL,
    -- Android versionName (semver-ish string).
    version_name    TEXT    NOT NULL,
    -- Path on disk relative to $APP_FILES_DIR.
    file_path       TEXT    NOT NULL,
    file_size_bytes INTEGER NOT NULL,
    -- Lowercase hex sha256 of the artifact.
    sha256          TEXT    NOT NULL,
    -- Android minSdk reported by aapt (NULL for non-APK artifacts).
    min_sdk         INTEGER,
    -- Set when the version is the official latest for the application.
    is_active       INTEGER NOT NULL DEFAULT 0,
    notes           TEXT,
    uploaded_by     INTEGER REFERENCES users(id) ON DELETE SET NULL,
    uploaded_at     TEXT    NOT NULL DEFAULT (datetime('now')),
    UNIQUE (application_id, version_code)
);

CREATE INDEX idx_application_versions_app ON application_versions(application_id);
CREATE INDEX idx_application_versions_active ON application_versions(application_id, is_active);
