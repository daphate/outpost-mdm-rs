# Client telemetry contract — что AR Hud-приложение шлёт в MDM

**Дата:** 2026-05-19 · **MDM-сервер:** ≥ 0.18.2 · **Outpost-Android:** rc43+

Этот документ — контракт между AR Hud (client, Kotlin/Compose) и Outpost MDM (server, axum). Сервер обязуется принять и хранить всё, что описано ниже; client обязуется присылать ровно эти события в указанном виде.

Документ — авторитетен. При расхождении кода с этим документом — поправить **код**, не документ. Контракт можно пересматривать только согласованно с обеими командами.

## 0. Контекст

На 2026-05-19 в `device_logs` приходят (фактическая выборка с rc43-b1):

- `screen_open` × 14 — навигация
- `chat_done`, `vlm_done`, `vlm_error`, `stt-conf_done`, `model_load_done` — **только метаданные**: `duration_ms`, `350 chars response`, `error_message`

**Не приходят:**
- Текст запроса пользователя в чат
- Ответ LLM
- Запрос/ответ переводчика
- Caption от VLM (image recognition) и prompt к нему

**Все события на уровне `INFO`** (`severity_number=9`) — выбранный на телефоне log-level **игнорируется**.

Этот документ закрывает gap'ы выше.

## 1. Privacy mode — что разрешено слать

| Период | Режим | Что в body / attrs |
|---|---|---|
| **Beta-test (текущий этап, 2026-05)** | full | Полные тексты запросов и ответов LLM, полные тексты переводов, full STT-расшифровки. Без редактирования. |
| **После beta (TBD)** | TBD | Решается отдельно. По умолчанию — те же события, но с возможностью policy-based редактирования (PII-фильтры) если потребуется заказчиком. |

> **Disclaimer на устройстве.** При первом запуске приложение **должно** показать
> явный текст: «В beta-режиме телеметрия включает полные тексты ваших запросов,
> ответов LLM, переводов и распознанной речи. Это нужно для отладки качества
> модели; данные не передаются третьим сторонам.» Без этого disclaimer'а
> beta-mode unsafe для оператора. (AR Hud team — DisclaimerScreen.)

Аудио (raw recordings) и изображения (camera frames для VLM) — **не шлём**. Слишком объёмно, privacy-чувствительно, безполезно для дебага модели (нам важен результат STT/VLM, не вход).

## 2. Log-level mapping

В Settings приложения есть выбор уровня логирования (TRACE / DEBUG / INFO / WARN / ERROR). Сейчас (rc43) **этот выбор client игнорирует** при отправке OTLP. Должен начать уважать:

| User-selected level | OTLP severity_number | Что шлём |
|---|---|---|
| TRACE | ≥ 1 | Всё, включая каждый Composable recomposition и каждый Retrofit call. **Только в dev-mode**, не в production rc-сборке. |
| DEBUG | ≥ 5 | Все ML-инференсы (с full prompts/responses), все screen transitions, все network calls, выбранная модель / параметры |
| **INFO** (default) | ≥ 9 | Все события из §3, но **без traces** Retrofit-уровня. Это default для бета-тестеров. |
| WARN | ≥ 13 | Только подозрительные ситуации (retry, fallback, slow inference) |
| ERROR | ≥ 17 | Только ошибки (uncaught exceptions, VLM/STT/LLM failures, network errors) |

**Условие соблюдения уровня:** перед отправкой OTLP-batch'а клиент сравнивает `event.severity_number` с user-selected threshold, и события ниже threshold'а **не отправляет**. На server-side можно увидеть выбранный уровень через `device_logs.attrs.client.log_level_threshold` (см. §4).

## 3. Каталог обязательных событий

Все события идут в `device_logs` через OTLP `/v1/logs` endpoint. Поля `event.name`, `severity_number`, `severity_text`, `body`, `attrs` — формат ниже.

### 3.1. Чат (`event.name="chat.*"`)

**`chat.request`** (severity INFO, sn=9):

```json
{
  "body": "<полный текст запроса пользователя>",
  "attrs": {
    "event.name": "chat.request",
    "model.name": "qwen3-4b-soldier-v25",
    "model.filename": "qwen3-4b-soldier-v25-Q4_K_M.gguf",
    "session.id": "<uuid сессии чата>",
    "turn.number": 1,
    "input.tokens_est": 42,
    "rag.enabled": true
  }
}
```

**`chat.response`** (severity INFO, sn=9):

```json
{
  "body": "<полный текст ответа LLM>",
  "attrs": {
    "event.name": "chat.response",
    "model.name": "qwen3-4b-soldier-v25",
    "session.id": "<тот же uuid что в chat.request>",
    "turn.number": 1,
    "duration_ms": 17199,
    "output.chars": 350,
    "output.tokens_est": 78,
    "guard.verdict": "pass | rewrite | block",
    "guard.issues_count": 0,
    "rag.chunks_used": 5
  }
}
```

**`chat.error`** (severity ERROR, sn=17): если генерация упала.

```json
{
  "body": "<error message>",
  "attrs": {
    "event.name": "chat.error",
    "session.id": "<uuid>",
    "duration_ms": 4123,
    "error.class": "ModelLoadFailed | OutOfMemory | Timeout | Unknown"
  }
}
```

### 3.2. Переводчик (`event.name="translator.*"`)

**`translator.request`** (severity INFO):

```json
{
  "body": "<полный текст для перевода>",
  "attrs": {
    "event.name": "translator.request",
    "source.lang": "ru",
    "target.lang": "en",
    "input.chars": 142,
    "model.name": "qwen2.5-7b-instruct"
  }
}
```

**`translator.response`** (severity INFO):

```json
{
  "body": "<полный текст перевода>",
  "attrs": {
    "event.name": "translator.response",
    "source.lang": "ru",
    "target.lang": "en",
    "duration_ms": 8210,
    "output.chars": 138,
    "model.name": "qwen2.5-7b-instruct"
  }
}
```

### 3.3. VLM / camera-ID (`event.name="vlm.*"`)

**`vlm.request`** (severity INFO):

```json
{
  "body": "<текст-инструкция к изображению, например 'Что на снимке?'>",
  "attrs": {
    "event.name": "vlm.request",
    "model.name": "qwen3-vl-8b",
    "image.width": 1280,
    "image.height": 720,
    "image.format": "jpeg"
  }
}
```

> **Картинка-blob НЕ передаётся.** Только размер + формат для контекста.

**`vlm.response`** (severity INFO):

```json
{
  "body": "<полный текст caption / описания от модели>",
  "attrs": {
    "event.name": "vlm.response",
    "model.name": "qwen3-vl-8b",
    "duration_ms": 4321,
    "output.chars": 87
  }
}
```

**`vlm.error`** (severity ERROR): уже шлётся (`vlm_error` event, видела в выборке). Переименовать в `vlm.error` для consistency.

### 3.4. STT (`event.name="stt.*"`)

`stt.result` **уже шлётся** в виде `stt-conf_done` — переименовать в `stt.result`, унифицировать формат:

```json
{
  "body": "<полный распознанный текст>",
  "attrs": {
    "event.name": "stt.result",
    "model.name": "whisper-large-v3-turbo",
    "duration_ms": 17084,
    "confidence": 0.50,
    "segments": 1,
    "audio_duration_ms": 5230,
    "input.lang": "ru"
  }
}
```

### 3.5. Навигация и lifecycle — уже работают

Эти события уже корректно шлются от rc43-b1, **не трогать**:

- `screen_open` — переход на экран (attrs: `screen`)
- `model_load_done` — загрузка ML-модели (attrs: `model.filename`, `duration_ms`)

Только переименовать в нотацию `event.name="screen.open"` / `event.name="model.load_done"` ради consistency с §3.1–3.4.

### 3.6. Client telemetry config (на каждый OTLP batch)

В **каждом** OTLP-batch'е, в `resource.attributes` (это applies к всем log-records в batch'е):

```
client.log_level_threshold: "INFO"    (string, текущий выбранный уровень)
client.beta_telemetry_full: true       (bool, режим §1)
client.app_version: "rc43-b1"          (string)
client.session_id: "<uuid app session>" (string, regenerated on app restart)
```

Сервер использует это для отображения «выбранного уровня» в admin UI рядом с device.

## 4. Что server-side делает с этим

- **Storage:** всё попадает в `device_logs` без обрезки. `body` — TEXT NOT NULL (no length limit), `attrs_json` — TEXT NOT NULL (no length limit). Размер batch'а ограничен `MAX_BODY_BYTES` (200 MB по умолчанию) — это далеко за пределами реальных значений.
- **GC:** `device_logs` НЕ имеют auto-GC по возрасту в v0.18.x. После beta, когда объёмы вырастут, придётся добавить retention policy (TODO в roadmap, не блокер для контракта).
- **Search:** в admin UI на `/devices/{id}/telemetry` — table view с filter по `event.name`. Полный `body` доступен через expand (см. v0.18.3 changes, отдельный коммит). Per-event пагинация.
- **Grafana:** dashboards могут строить panel'ы «топ-N user prompts по длине», «event.name distribution», «duration_ms histogram per model».

## 5. Что НЕ делать

- Audio blobs (raw STT recordings) — большие, не нужны для дебага результата
- Image blobs (camera frames для VLM) — то же
- Embedding vectors — большие, не нужны без model context
- Каждый Composable recomposition (TRACE-level) — только в dev-mode
- Synchronous OTLP send на main thread — must be async (тогда не блокирует UI)
- Hashing user prompts — в beta нужен полный текст для отладки качества модели

## 6. Cross-references

- [`docs/OFFLINE-RESILIENCE.md`](OFFLINE-RESILIENCE.md) — что происходит с накопленным telemetry queue'ом при возврате online
- [`docs/OTEL-CONTRACT.md`](OTEL-CONTRACT.md) — server-side OTLP wire format (низкоуровневый: какие endpoints, схемы запросов)
- `tools/MDM-DEVICE-CONTROL-CONTRACT.md` (AR Hud repo) — обратное направление: что MDM отправляет на устройство
