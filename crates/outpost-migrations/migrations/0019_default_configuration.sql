-- Per-customer default configuration. New devices that don't have a
-- configuration assigned at enrollment time inherit this one. The field
-- is nullable so it can be cleared via admin UI without orphaning
-- existing device configurations.
--
-- ON DELETE SET NULL — if the admin deletes the configuration that is
-- currently the default, we drop the pointer rather than deny the
-- delete; existing devices keep their resolved configuration_id (the
-- direct FK on `devices.configuration_id` has the same ON DELETE SET
-- NULL behaviour from migration 0011).

ALTER TABLE customers
    ADD COLUMN default_configuration_id INTEGER
        REFERENCES configurations(id) ON DELETE SET NULL;

-- Seed a default configuration for every existing customer that doesn't
-- already have one named "По умолчанию". Idempotent — re-running this
-- migration after creating a customer manually is safe.
INSERT INTO configurations (customer_id, name, description, settings_json, is_active)
SELECT
    c.id,
    'По умолчанию',
    'Конфигурация по умолчанию. Назначается новым устройствам автоматически при создании. Меняй её содержимое чтобы влиять на всех новых бойцов.',
    '{}',
    1
FROM customers c
WHERE NOT EXISTS (
    SELECT 1 FROM configurations
    WHERE customer_id = c.id AND name = 'По умолчанию'
);

-- Link each customer's `default_configuration_id` to the freshly-seeded
-- (or pre-existing) "По умолчанию" config. Doesn't overwrite a default
-- that's already set to something else — operator's choice wins.
UPDATE customers
SET default_configuration_id = (
    SELECT id FROM configurations
    WHERE configurations.customer_id = customers.id AND name = 'По умолчанию'
    LIMIT 1
)
WHERE default_configuration_id IS NULL;

-- Back-fill existing devices that have no configuration assigned —
-- they inherit the customer default. Devices that ALREADY have an
-- explicit `configuration_id` are untouched.
UPDATE devices
SET configuration_id = (
    SELECT default_configuration_id FROM customers WHERE customers.id = devices.customer_id
)
WHERE configuration_id IS NULL;
