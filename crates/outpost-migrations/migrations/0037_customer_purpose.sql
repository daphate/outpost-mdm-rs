-- Назначение тенанта (ось, отдельная от kind = production/demo/test):
-- universal | tactical | antidrone | game | wearables.
--
-- Whitelist живёт в коде (outpost-server, nav::TenantPurpose) — как и у
-- kind, чтобы добавление нового назначения не требовало миграции. Верхнее
-- меню Web UI строится по этому полю (nav::NavCtx): игровой тенант видит
-- игровые разделы, антидронный — антидронные и т. д. Существующие тенанты
-- получают 'universal' — меню для них не меняется.
ALTER TABLE customers ADD COLUMN purpose TEXT NOT NULL DEFAULT 'universal';
