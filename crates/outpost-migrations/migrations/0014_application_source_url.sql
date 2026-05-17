-- v0.11.0 — APK watcher support.
--
-- Two changes to `application_versions`:
--   1. New `source_url` column — for releases discovered by the APK watcher
--      from an upstream mirror (R2 / Cloud.ru / GH Releases). Stores the
--      canonical download URL so admin UI can link out to it directly
--      without an MDM-local copy.
--   2. `file_path` and `file_size_bytes` are still NOT NULL, but a watcher-
--      tracked row may use file_path='' (empty string) + file_size_bytes=0
--      to signal "metadata-only, binary not pulled locally yet". The admin
--      UI distinguishes these from uploaded rows by checking `source_url IS
--      NOT NULL`. Once the watcher downloads the APK (Tier-2, not in this
--      migration), it will fill in file_path + file_size_bytes from the
--      on-disk artifact.
--
-- We also relax UNIQUE (application_id, version_code) is fine — но добавляем
-- explicit constraint that sha256 inside one application is unique, чтобы
-- предотвратить дубликаты при идемпотентных watcher pulls.

ALTER TABLE application_versions
    ADD COLUMN source_url TEXT;

CREATE UNIQUE INDEX idx_application_versions_app_sha256
    ON application_versions(application_id, sha256);
