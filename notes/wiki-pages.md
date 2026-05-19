# Yandex Wiki pages (НИИ ИИ org)

Маппинг ID страниц на Yandex Wiki для проекта outpost-mdm-rs. При синхронизации `docs/*.md` / `README.md` через `api.wiki.yandex.net` используется ID из этой таблицы; slug — для human-readable URL.

| Wiki ID | Slug | Source | Назначение |
|---|---|---|---|
| 49790823 | `homepage/nii-ii/outpost-mdm-rs` | `.tmp/wiki-outpost-mdm.md` (содержимое заходит при синхронизации) | Главная страница проекта — overview, фичи, ссылки на docs, scope разделение |

## Обновление

Workflow для синхронизации согласно [`sophia-soul/INFRASTRUCTURE.md`](https://github.com/daphate/sophia-soul/blob/main/INFRASTRUCTURE.md):

1. `scp <local>.md lokitheone@mac-mini-loki:/tmp/wiki-page.md`
2. SSH на mac-mini-loki + `security unlock-keychain -p ""`
3. `POST /v1/pages/{id}` с body `{"content": "..."}` — НЕ PATCH, не PUT
4. Headers: `Authorization: OAuth <token>`, `X-Collab-Org-Id: 924f54db-9bf1-40f0-8ab2-95e9bb1d10ab`

Прямой URL для просмотра: https://wiki.yandex.ru/homepage/nii-ii/outpost-mdm-rs

## Заметки

- Page создана 2026-05-19 (v0.18.13). Содержит обзор проекта на момент v0.18.13.
- При значительных изменениях фич — обновить `.tmp/wiki-outpost-mdm.md` и пере-POST'ить через workflow выше.
- Дочерние страницы (если появятся — например `homepage/nii-ii/outpost-mdm-rs/architecture`) добавлять в эту же таблицу.
