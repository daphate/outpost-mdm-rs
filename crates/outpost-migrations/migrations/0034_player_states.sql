-- Ф4 класс «игроки STALKER»: игровое состояние игрока (per-device, только
-- последнее значение — история перемещения покрывается device_positions).
-- Ключ = device_id (upsert на каждый отчёт). Радиация/здоровье/угроза —
-- игровые величины из приложения net.afterday.compas.
CREATE TABLE player_states (
    device_id    INTEGER PRIMARY KEY REFERENCES devices(id) ON DELETE CASCADE,
    customer_id  INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    radiation    REAL,       -- текущий уровень радиации (игровые единицы)
    health       REAL,       -- здоровье 0..100
    threat_level TEXT,       -- 'none' | 'low' | 'high' и т.п.
    artifacts    INTEGER,    -- число собранных артефактов
    detail_json  TEXT,       -- произвольное расширение состояния
    updated_at   TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_player_states_customer ON player_states(customer_id);
