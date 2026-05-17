-- v0.14 — Per-device encrypted distribution: §2.6 encrypted_distributions.
--
-- Single ciphertext blob (uploaded once в Cloud.ru/R2/MDM-local), но per-recipient
-- wrap эфемерным ECDH+HKDF→AES-GCM(DEK). N rows для группы из N устройств.
--
-- Lifecycle:
--   created_at → push_message с command='fetch-encrypted-file' INSERTится
--   ↓
--   client скачивает blob, decrypt'ит, install'ит (PdfStorage / ZimReader / …)
--   ↓ /sync applied_commands[id=cmd_id, status=ok|error]
--   delivered_at заполняется (либо last_error если status=error)
--   ↓ (если expires_at прошёл + grace 7 дней)
--   GC task удаляет blob с Cloud.ru/локально, выставляет purged_at

CREATE TABLE encrypted_distributions (
    id                    INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id           INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    -- Источник распространения — обычно один из uploaded_files; nullable
    -- чтобы можно было распространять ad-hoc blob'ы без перманентной uploaded_files row.
    file_id               INTEGER REFERENCES uploaded_files(id) ON DELETE SET NULL,
    -- Имя файла на устройстве после расшифровки. Не путать с file_path
    -- в uploaded_files.
    filename              TEXT    NOT NULL,
    -- Куда устройство положит расшифрованный plaintext.
    -- См. MDM-DEVICE-CONTROL-CONTRACT.md §2.3 table.
    kind                  TEXT    NOT NULL,  -- 'pdf' | 'zim' | 'knowledge_db_chunk' | 'model_gguf' | 'arbitrary_blob'

    recipient_device_id   INTEGER NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    -- Денормализованный key_id из device_keys; позволяет verify on receive
    -- что устройство ключ не сменило между распространением и доставкой.
    recipient_key_id      TEXT    NOT NULL,

    -- Ciphertext (один на всю группу) — N rows этой таблицы могут ссылаться
    -- на один blob_url, или на разные если admin перезагружал.
    ciphertext_url        TEXT    NOT NULL,
    ciphertext_size       INTEGER NOT NULL,
    ciphertext_sha256     TEXT    NOT NULL,
    ciphertext_iv         BLOB    NOT NULL,   -- 12 bytes
    ciphertext_tag        BLOB    NOT NULL,   -- 16 bytes (AES-GCM tag separated)
    plaintext_sha256      TEXT    NOT NULL,
    plaintext_size        INTEGER NOT NULL,

    -- Per-recipient ECDH wrap данные:
    eph_pubkey_der        BLOB    NOT NULL,    -- 91 bytes SPKI P-256
    wrapped_dek           BLOB    NOT NULL,    -- 48 bytes = 32 ciphertext + 16 tag
    wrapped_dek_iv        BLOB    NOT NULL,    -- 12 bytes

    -- Связь с push_messages: при создании distribution мы создаём
    -- push_message и сохраняем сюда его id для cross-reference / cleanup.
    push_message_id       INTEGER REFERENCES push_messages(id) ON DELETE SET NULL,
    expires_at            TEXT,    -- после grace 7 дней — eligible for GC
    delivered_at          TEXT,    -- заполняется когда /sync applied_commands ok
    last_error            TEXT,    -- если client отчитался status=error
    created_at            TEXT    NOT NULL DEFAULT (datetime('now')),
    -- Когда blob был удалён с Cloud.ru/локально GC-task'ом (для audit).
    purged_at             TEXT
);

CREATE INDEX idx_encrypted_distributions_recipient
    ON encrypted_distributions(recipient_device_id);
CREATE INDEX idx_encrypted_distributions_file
    ON encrypted_distributions(file_id);
CREATE INDEX idx_encrypted_distributions_gc
    ON encrypted_distributions(expires_at) WHERE purged_at IS NULL;
