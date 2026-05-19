# Library archives — server-side mirror

**Цель документа:** server-side взгляд на [LIBRARY-ARCHIVES-CONTRACT](https://github.com/daphate/tactical-ar-hud/blob/master/tools/LIBRARY-ARCHIVES-CONTRACT.md) (cross-team coordination doc в AR Hud repo). Здесь зафиксировано **что делает Outpost MDM team** в этой цепочке.

## TL;DR

KB team builds ZIP-архивы по тематикам из `sources/{category}/`. Outpost MDM team mirror'ит каждый ZIP на R2 и Cloud.ru под key `library/archives/<name>.zip`. AR Hud client скачивает ZIP по URL из `bootstrap-manifest.json`, проверяет sha256, extract'ит в `/sdcard/Outpost/docs/<category>/`.

## Архитектура mirror'а

| Параметр | Значение |
|---|---|
| Bucket R2 | `pub-ef0219f0ecf84d0e8e44497adfe9ceb0` (public Cloudflare R2) |
| Bucket Cloud.ru | `outpost` |
| Key prefix | `library/archives/` |
| Сidecar | `<name>.sha256` рядом с `<name>.zip` (inline body, не файл) |
| Mirror parity | **обязательна** — same sha256 на обоих mirror'ах. Audit через `verify_head` после upload'а. |

## Upload workflow

Для одного архива:

```sh
python3 tools/upload_library_archives.py /path/to/medical-tactical-v1.zip
```

Для batch'а (целая директория):

```sh
python3 tools/upload_library_archives.py /path/to/build/library-archives/
```

Dry-run (compute sha256 без upload'а):

```sh
python3 tools/upload_library_archives.py --dry-run /path/to/archives/
```

Script автоматически:

1. Считает sha256 локально.
2. Загружает ZIP на **R2** под `library/archives/<basename>`.
3. Загружает ZIP на **Cloud.ru** под тем же key.
4. Загружает `.sha256` sidecar (inline `<hash>  <name>` body) на оба mirror'а.
5. `HEAD` запросы к обоим mirror'ам — verify size совпадает с локальным.
6. Print summary: `N/M uploaded successfully`.

## Credentials

Используются те же что и для APK/V25/models — из `F:\projects\tactical-ar-hud\.tmp\`:
- `r2-creds.env` — `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`, `R2_BUCKET`, `R2_ENDPOINT_URL`
- `cloudru-creds.env` — `CLOUDRU_TENANT_ID`, `CLOUDRU_KEY_ID`, `CLOUDRU_SECRET_ACCESS_KEY`, `CLOUDRU_BUCKET`, `CLOUDRU_ENDPOINT_URL`

Cloud.ru endpoint требует `NO_PROXY` bypass под Hide.My.Name VPN — script сам добавляет нужные suffix'ы (`<endpoint-host>`, `.cloud.ru`, `.s3.cloud.ru`).

## Версионирование

Имя файла включает версию: `<category>-v<N>.zip`. KB team bump'ает `N` при изменении содержимого. Manifest сторона:

- Старая версия (`medical-tactical-v1.zip`) **остаётся** на mirror'ах — устройства на старом manifest продолжают работать.
- Новая версия (`medical-tactical-v2.zip`) аплоадится рядом, manifest указывает на v2.
- Cleanup старых версий — отдельный maintenance task, не часть upload'а. После того как ВСЕ active devices обновились до v2 (visible через `device_logs.client.app_version` в admin UI), v1 можно удалить через `aws s3 rm`.

Идемпотентно: повторный upload того же ZIP перезатирает тот же key (R2 и Cloud.ru оба позволяют overwrite по PUT). Sha256 sidecar тоже перезаписывается.

## Manifest re-upload

После upload новых ZIP'ов KB team регенерит `bootstrap-manifest.json` (через свой generator скрипт), кладёт обновлённый файл и сообщает MDM team. MDM team загружает manifest на mirror'ы.

**TODO:** manifest upload — отдельный скрипт `tools/upload_manifest.py` (планируется когда KB team пришлёт первый обновлённый manifest для library.archives[]; до этого момента manifest продолжает живёт как bundled asset в APK).

## Что НЕ делает MDM team

Чтобы не было путаницы в three-team coordination:

- **Не строит ZIP** — это KB team, через `tools/build_library_archives.py` (TBD в AR Hud repo).
- **Не пишет manifest** — это KB team, через `tools/build_bootstrap_manifest.py`.
- **Не имплементит ArchiveDownloader, UI** — это AR Hud team, rc43-b21+.

MDM team — это только mirror upload + integrity verify. Cредние шаги pipeline.

## Граничные случаи

**ZIM (347 ГБ)** — НЕ заворачивается в архив. ZIM сами по себе sqlite-контейнеры с FT-index, повторная ZIP-обёртка бесполезна. Они продолжают живёт в `manifest.zim[]` как individual download'ы.

**maps_extra (mbtiles)** — то же. Они в `manifest.maps_extra[]` (юзер выбирает region на устройстве).

**Очень большие категории (>500 МБ docs)**: KB team может split на sub-archives (`weapons-small-arms-rifles-v1.zip` + `weapons-small-arms-pistols-v1.zip`). MDM script их обрабатывает идентично — каждый ZIP = отдельный key.

**Failed upload в середине batch'а** — script reports `N/M uploaded successfully`. Не удаляет уже загруженные другие архивы. Retry-driven recovery: re-run script на failed архиве вручную.

## Cross-references

- [`tools/upload_library_archives.py`](../tools/upload_library_archives.py) — upload script
- [`tools/LIBRARY-ARCHIVES-CONTRACT.md`](https://github.com/daphate/tactical-ar-hud/blob/master/tools/LIBRARY-ARCHIVES-CONTRACT.md) — full cross-team contract (в AR Hud repo)
- [`docs/V25-SOLDIER-ROLLOUT.md`](V25-SOLDIER-ROLLOUT.md) — similar pattern для model upload'а (handoff между ML team и MDM team)
- Existing upload patterns: `.tmp/upload_v25.py` (V25 model upload), `tools/upload_apk.py` (в AR Hud repo, APK upload)
