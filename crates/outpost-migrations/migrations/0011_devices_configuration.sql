-- Add a per-device configuration pointer + custom property fields.
--
-- Headwind models each device as having ONE active configuration (which in
-- turn enumerates the apps/files/kiosk policy). We dropped that off the
-- initial schema because v0.1 didn't render or persist it; bringing it back
-- now that the UI can edit it.
--
-- custom1 / custom2 are free-form per-device strings — Headwind uses them
-- for operator name, room number, call-sign, anything the deployment wants.

ALTER TABLE devices
    ADD COLUMN configuration_id INTEGER REFERENCES configurations(id) ON DELETE SET NULL;

ALTER TABLE devices ADD COLUMN description TEXT;
ALTER TABLE devices ADD COLUMN custom1     TEXT;
ALTER TABLE devices ADD COLUMN custom2     TEXT;
ALTER TABLE devices ADD COLUMN phone       TEXT;

CREATE INDEX idx_devices_configuration ON devices(configuration_id);
