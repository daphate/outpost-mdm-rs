-- Outpost MDM schema: generic uploaded files (icons, attachments, model blobs).
--
-- `applications` and `application_versions` reference paths directly when
-- they are the owner; this table is a catalog of standalone uploads
-- (configuration_files, icon images, ML model side artifacts).

CREATE TABLE uploaded_files (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id     INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    -- Path relative to $APP_FILES_DIR.
    file_path       TEXT    NOT NULL,
    -- Original filename as uploaded (for download presentation).
    original_name   TEXT    NOT NULL,
    content_type    TEXT,
    file_size_bytes INTEGER NOT NULL,
    sha256          TEXT    NOT NULL,
    -- Kind tag for filtering ('icon', 'model', 'mbtiles', 'kb-snapshot', …).
    kind            TEXT    NOT NULL DEFAULT 'generic',
    uploaded_by     INTEGER REFERENCES users(id) ON DELETE SET NULL,
    uploaded_at     TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_uploaded_files_customer ON uploaded_files(customer_id);
CREATE INDEX idx_uploaded_files_sha256   ON uploaded_files(sha256);
CREATE INDEX idx_uploaded_files_kind     ON uploaded_files(kind);
