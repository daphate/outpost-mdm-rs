-- Виртуальные маяки: операторские гео-зоны угроз для класса «игроки STALKER».
-- Игрок в онлайне, входя по GPS в радиус маяка, получает угрозу так же, как от
-- физического Wi-Fi-модуля. `beacon_type` — буква угрозы (как первая буква SSID
-- у физических модулей), `coeff` — сила/радиус (как коэффициент в имени SSID,
-- напр. A100), `radius_m` — радиус срабатывания в метрах. Приложение забирает
-- активные маяки и гео-фенсит их локально.
CREATE TABLE virtual_beacons (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id  INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    unit_id      INTEGER REFERENCES units(id) ON DELETE SET NULL,
    -- Буква угрозы: R радиация, A аномалия, M ментал, C контролёр, B бюрер,
    -- Z зов Монолита, H лечение (Оазис).
    beacon_type  TEXT    NOT NULL CHECK (beacon_type IN ('R','A','M','C','B','Z','H')),
    coeff        INTEGER NOT NULL DEFAULT 100,
    radius_m     REAL    NOT NULL DEFAULT 30,
    lat          REAL    NOT NULL,
    lon          REAL    NOT NULL,
    label        TEXT,
    is_active    INTEGER NOT NULL DEFAULT 1,
    created_by   INTEGER REFERENCES users(id) ON DELETE SET NULL,
    created_at   TEXT    NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_virtual_beacons_customer ON virtual_beacons(customer_id, is_active);
