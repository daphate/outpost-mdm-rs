# Yandex Wiki pages (НИИ ИИ org)

Маппинг ID страниц на Yandex Wiki для проекта outpost-mdm-rs. При синхронизации `docs/*.md` / `README.md` через `api.wiki.yandex.net` используется ID из этой таблицы; slug — для human-readable URL.

## Parent page

| Wiki ID | Slug | Source | Назначение |
|---|---|---|---|
| 49790823 | `homepage/nii-ii/outpost-mdm-rs` | `.tmp/wiki-outpost-mdm.md` | Главная страница проекта — overview, фичи, ссылки на docs, scope разделение |

## Child pages — документация по продукту

Синхронизируются скриптом [`.tmp/sync_wiki_docs.py`](../.tmp/sync_wiki_docs.py) — на mac-mini-loki, идемпотентно (re-run обновит content существующих страниц).

| Wiki ID | Slug | Source file | Title |
|---|---|---|---|
| 49792569 | `.../architecture` | [`docs/ARCHITECTURE.md`](../docs/ARCHITECTURE.md) | Архитектура |
| 49792570 | `.../deploy` | [`docs/DEPLOY.md`](../docs/DEPLOY.md) | Развёртывание |
| 49792571 | `.../provision-new-device` | [`docs/PROVISION-NEW-DEVICE.md`](../docs/PROVISION-NEW-DEVICE.md) | Провижининг нового устройства |
| 49792572 | `.../offline-resilience` | [`docs/OFFLINE-RESILIENCE.md`](../docs/OFFLINE-RESILIENCE.md) | Устойчивость к offline |
| 49792573 | `.../otel-contract` | [`docs/OTEL-CONTRACT.md`](../docs/OTEL-CONTRACT.md) | Контракт OTLP телеметрии |
| 49792576 | `.../client-telemetry-contract` | [`docs/CLIENT-TELEMETRY-CONTRACT.md`](../docs/CLIENT-TELEMETRY-CONTRACT.md) | Контракт client telemetry |
| 49792577 | `.../v25-soldier-rollout` | [`docs/V25-SOLDIER-ROLLOUT.md`](../docs/V25-SOLDIER-ROLLOUT.md) | V25 Soldier rollout |
| 49792580 | `.../changelog` | [`CHANGELOG.md`](../CHANGELOG.md) | Журнал изменений |
| 49792840 | `.../library-archives` | [`docs/LIBRARY-ARCHIVES.md`](../docs/LIBRARY-ARCHIVES.md) | Library archives — server-side mirror |

## URLs для просмотра

- Главная: https://wiki.yandex.ru/homepage/nii-ii/outpost-mdm-rs
- Архитектура: https://wiki.yandex.ru/homepage/nii-ii/outpost-mdm-rs/architecture
- Развёртывание: https://wiki.yandex.ru/homepage/nii-ii/outpost-mdm-rs/deploy
- Провижининг: https://wiki.yandex.ru/homepage/nii-ii/outpost-mdm-rs/provision-new-device
- Offline-resilience: https://wiki.yandex.ru/homepage/nii-ii/outpost-mdm-rs/offline-resilience
- OTEL contract: https://wiki.yandex.ru/homepage/nii-ii/outpost-mdm-rs/otel-contract
- Client telemetry: https://wiki.yandex.ru/homepage/nii-ii/outpost-mdm-rs/client-telemetry-contract
- V25 rollout: https://wiki.yandex.ru/homepage/nii-ii/outpost-mdm-rs/v25-soldier-rollout
- Changelog: https://wiki.yandex.ru/homepage/nii-ii/outpost-mdm-rs/changelog
- Library archives: https://wiki.yandex.ru/homepage/nii-ii/outpost-mdm-rs/library-archives

## Workflow обновления

Полный re-sync (например после изменений в docs/*.md):

```bash
# 1. scp всех docs на loki
scp -i ~/.ssh/awscalifornia docs/*.md CHANGELOG.md lokitheone@mac-mini-loki:/tmp/
scp -i ~/.ssh/awscalifornia .tmp/sync_wiki_docs.py lokitheone@mac-mini-loki:/tmp/

# 2. run sync
ssh -i ~/.ssh/awscalifornia lokitheone@mac-mini-loki '
  security unlock-keychain -p "" login.keychain
  python3 /tmp/sync_wiki_docs.py
'
```

Скрипт идемпотентен: `find_existing(slug)` проверяет существование, переиспользует ID; иначе создаёт новую страницу.

Workflow на per-page уровне ([`sophia-soul/INFRASTRUCTURE.md`](https://github.com/daphate/sophia-soul/blob/main/INFRASTRUCTURE.md)):
- **Create**: `POST /v1/pages` с body `{slug, title}` (опционально content)
- **Update content**: `POST /v1/pages/{id}` с body `{content: "..."}` — **не PATCH**
- **Headers**: `Authorization: OAuth <token>`, `X-Collab-Org-Id: 924f54db-9bf1-40f0-8ab2-95e9bb1d10ab`

## Заметки

- При значительных изменениях фич — обновить файлы в `docs/`, потом запустить sync скрипт.
- API ignore`s `page_type: markdown` — все страницы создаются как `wysiwyg`, но Markdown содержимое отображается корректно в UI.
- Дочерние страницы автоматически связаны через slug-hierarchy (`/parent/child` URL pattern).
