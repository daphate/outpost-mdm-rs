# OTLP contract — Outpost-Android → Outpost MDM server (v2)

**Status:** v2.0, **supersedes** [`tactical-ar-hud/tools/OTEL-CONTRACT.md`](../../tactical-ar-hud/tools/OTEL-CONTRACT.md) v1.0 of 2026-05-17.

**Receiver:** [`crates/outpost-server/src/routes/otel.rs`](../crates/outpost-server/src/routes/otel.rs) — in-process Axum handler that persists OTLP/HTTP-JSON into SQLite. **OpenObserve / Grafana Loki / external collectors are not used** — the in-tree receiver gives us first-class integration with the MDM device table, the admin UI, and the multi-tenant `customer_id` scoping. The decision is conscious: lower operational complexity, one storage system to back up, and the receiver is already tested.

**Receiver host:** `mdm.secondf8n.tech` (production).
**Tests:** [`crates/outpost-server/tests/otel.rs`](../crates/outpost-server/tests/otel.rs) — 8 integration tests, including auth/parse/persistence/empty-batch cases. **149/149 passing** as of v0.6.0.

---

## What changed from v1.0

The v1.0 contract (written by the sender session) targeted "any standard OTEL collector". The receiver session (this side) chose to implement a first-party receiver in the MDM server. That decision drives three breaking changes against v1.0:

| Concern | v1.0 (Outpost-Android sender draft) | **v2 (this contract — authoritative)** |
|---|---|---|
| Authentication | "Phase 1: none. Phase 2: Bearer." | **Bearer required from day 1.** Phase 1 = use the `device_token` issued at `/api/v1/enroll`. |
| Signals | `/v1/logs` only | `/v1/logs` **+** `/v1/metrics` **+** `/v1/traces`. Logs alone is still a valid Phase-1 implementation; the other two are optional but recommended. |
| Device identity | `X-Outpost-Device-Id` UUID header + `device.id` resource attribute | **Identity comes from the Bearer token.** `device.id` resource attribute may be sent verbatim from the sender (e.g. for cross-correlation across pipelines), but the receiver does not consult it; it derives `customer_id` and `device_id` from the verified session. The `X-Outpost-Device-Id` header is ignored. |
| Receiver UI | "OpenObserve / Grafana / etc." (operator chooses) | **MDM admin UI** at `/telemetry`, `/devices/{id}/telemetry`, `/devices/{id}/logs`. Grafana ALSO available at `/grafana/` scraping `/metrics` for fleet-wide aggregates. |
| Storage | "operator's choice" | SQLite tables `device_logs`, `device_metrics`, `device_traces`. |

Everything else from v1.0 (OTLP/HTTP-JSON wire format, severity numbers, event-catalog naming, privacy contract) carries over unchanged — see §3 below for the full superseded text.

---

## 1. Endpoints

| Method | URL | Body | Notes |
|---|---|---|---|
| `POST` | `https://mdm.secondf8n.tech/v1/logs`    | OTLP/HTTP `ExportLogsServiceRequest` JSON    | Required signal. |
| `POST` | `https://mdm.secondf8n.tech/v1/metrics` | OTLP/HTTP `ExportMetricsServiceRequest` JSON | Optional. |
| `POST` | `https://mdm.secondf8n.tech/v1/traces`  | OTLP/HTTP `ExportTraceServiceRequest` JSON   | Optional. |

* **Content-Type:** `application/json` only. Protobuf is deferred.
* **Authentication:** `Authorization: Bearer <device_token>`. The token is the same long-lived JWT issued at `/api/v1/enroll`. It is **also** the token used for `/api/v1/sync`; do not invent a second credential.
* **Compression:** off in v2 (server body limit 200 MiB is generous). Don't enable gzip on the sender yet.
* **`X-Outpost-Device-Id` header:** ignored by the receiver. Senders may keep emitting it for forward-compatibility with external collectors but should not rely on it.

### 1.1. Response shape

```json
{ "inserted": <i64> }     // number of records actually persisted in this batch
```

| Status | Meaning | Retry? |
|---|---|---|
| `200 OK`              | Batch accepted; `inserted` is the row count. | Done. |
| `400 Bad Request`     | Malformed JSON or invalid OTLP envelope.     | **No.** Fix the payload. |
| `401 Unauthorized`    | Missing / expired / revoked / wrong-kind token. | **No.** Re-enroll, then retry. |
| `413 Payload Too Large` | Single batch > 200 MiB.                    | Chunk the batch. |
| `5xx`                 | Server bug or temporary failure.             | Yes, exponential backoff. |

---

## 2. Batching & retry policy (recommended sender behaviour)

Compatible with v1.0; tighter defaults.

* **Batch size:** up to **500 records per batch** OR **1 MiB on the wire**, whichever is smaller.
* **Flush triggers:**
  * every **30 s** in foreground / **5 min** in background
  * batch hits 500 records OR 1 MiB
  * any record with `severityNumber >= 17` (ERROR/FATAL) — flush immediately
* **Network gating:** Wi-Fi by default; cellular only if the user opts in via app setting.
* **Persistence:** queue locally to a JSONL or SQLite buffer; drop oldest when the buffer exceeds **20 MiB** or **50 000 records**.
* **Retry:** exponential backoff 5 s → 30 s → 5 min, max 5 min. Drop a batch after 24 h of continuous failure.
* **Empty batch handling:** sending `{"resourceLogs":[]}` (or equivalent for metrics/traces) is a valid heartbeat — the server returns `200 OK` with `inserted:0`. Use this once at app start to confirm the contract is honoured.

---

## 3. Payload (carries v1.0 schema verbatim — receiver matches it)

### 3.1. Resource attributes — set once per batch

The receiver persists the full Resource block as JSON on every record, so cross-record queries on resource attributes are cheap.

```json
{
  "attributes": [
    { "key": "service.name",              "value": { "stringValue": "outpost-android" } },
    { "key": "service.version",           "value": { "stringValue": "<app version>" } },
    { "key": "device.id",                 "value": { "stringValue": "<random per-install UUID>" } },
    { "key": "device.model",              "value": { "stringValue": "Armor 28 Ultra" } },
    { "key": "device.manufacturer",       "value": { "stringValue": "Ulefone" } },
    { "key": "os.name",                   "value": { "stringValue": "android" } },
    { "key": "os.version",                "value": { "intValue":    "34" } },
    { "key": "telemetry.sdk.name",        "value": { "stringValue": "outpost-usage-telemetry" } },
    { "key": "telemetry.sdk.language",    "value": { "stringValue": "kotlin" } },
    { "key": "telemetry.sdk.version",     "value": { "stringValue": "<sender lib version>" } }
  ]
}
```

The receiver does **not** authenticate by `device.id` — see §1 — but the value is preserved for cross-reference with on-device logs.

### 3.2. Severity mapping — unchanged from v1.0

| Outpost event suffix | severityNumber | severityText | Use case |
|---|---|---|---|
| `_done` (success)      |  9 | INFO  | Pipeline completed successfully |
| `_warn`                | 13 | WARN  | Recoverable issue |
| `_error`               | 17 | ERROR | Pipeline failed, user-visible degradation |
| `error.uncaught_exception`, `error.anr` | 21 | FATAL | Crash / ANR |
| (other lifecycle / nav) |  9 | INFO  | Generic informational event |

Standard OTEL SeverityNumber values (RFC-5424-aligned). The `/telemetry` overview treats `severityNumber >= 17` as the "error" bucket in all KPI cards.

### 3.3. Logs (`/v1/logs`) — required catalog

| `event.name` (also `body.stringValue`) | Triggered when | Attributes |
|---|---|---|
| `app_open`               | `App.onCreate` in main process       | `cold_start: bool` |
| `app_close`              | `onTerminate` (best-effort)          | — |
| `<pipeline>_done`        | `PipelineLog.done(op, detail)`       | `duration_ms: int64`, `detail: string` (≤200 chars) |
| `<pipeline>_error`       | `PipelineLog.error(op, msg, ex?)`    | `error_message: string` (≤200), `exception: string?`, `duration_ms: int64?` |
| `model_load_done`        | LlamaBridge / WhisperBridge init     | `model: string`, `role: "LLM"\|"STT"\|"VLM"\|"TTS"`, `duration_ms: int64` |
| `model_load_error`       | init failure                         | `model, role, error_message` |
| `model_swap`             | runtime model swap                   | `from, to, role` |
| `screen_open`            | NavGraph onComposable (Phase 2)      | `screen: string` |
| `feature_use`            | user action in screen (Phase 2)      | `screen, action` |
| `network.unavailable`    | no usable network                    | `kind: "wifi"\|"cell"\|"none"` |
| `error.uncaught_exception` | crash handler caught a throwable   | `type: string`, `message: string` (≤200) |
| `error.anr`              | ANR detected                         | `thread: string` |

Pipeline names that send `_done`/`_error`: `chat`, `vlm`, `stt`, `stt-conf`, `translate`, `bench`. New pipelines should follow the same convention.

The receiver column mapping is:

| OTLP field | DB column | Notes |
|---|---|---|
| `timeUnixNano` (or `observedTimeUnixNano`) | `ts` | If absent → `datetime('now')` |
| `severityNumber` | `severity_number` | |
| `severityText`   | `severity_text`   | Falls back to mapping from `severityNumber` |
| `body.stringValue` | `body` | Other AnyValue variants are JSON-stringified |
| `attributes` | `attrs_json` | Flattened to a JSON object |
| (resource) | `resource_json` | Stored per record |
| `traceId` | `trace_id` | Optional |
| `spanId`  | `span_id`  | Optional |

### 3.4. Metrics (`/v1/metrics`) — optional but recommended

Send these names so the Prometheus `/metrics` endpoint can publish them as `outpost_metric_latest{name="…"}` (curated label set, fixed cardinality):

| `name` | Kind | Unit | Notes |
|---|---|---|---|
| `app.session_seconds`   | sum   | `s`     | Cumulative foreground time since install |
| `app.foreground_ms`     | gauge | `ms`    | Last session duration |
| `app.crashes`           | sum   | `1`     | Lifetime crash count |
| `app.anr_count`         | sum   | `1`     | Lifetime ANR count |
| `battery.pct`           | gauge | `1`     | 0..100 |
| `battery.charging`      | gauge | `1`     | 0 or 1 |
| `network.requests_total`| sum   | `1`     | All outbound HTTPS, including this telemetry POST |
| `network.errors_total`  | sum   | `1`     | Attributes: `kind: "timeout"\|"5xx"\|"dns"\|"tls"\|"reset"` |
| `ml.inference_ms`       | gauge | `ms`    | Last finished inference; attrs `{model, role}` |
| `ml.queue_depth`        | gauge | `1`     | Pending inference requests |
| `storage.free_mb`       | gauge | `MiBy`  | Internal storage free |
| `ram.available_mb`      | gauge | `MiBy`  | App's perception of free RAM |

Other metric names are accepted and stored, but only the names above appear on the Prometheus `/metrics` curated exposition. Add new names to `routes/prom.rs` `common_names` if you want them graphable in Grafana.

Receiver handles: `gauge.dataPoints`, `sum.dataPoints`, `histogram.dataPoints` (only `count` is persisted; bucket layout is dropped), `summary.dataPoints` (only `count`). `exponentialHistogram` is not yet supported — do not send.

Value parsing tolerates `asDouble`, `asInt` (integer or numeric string), and `count` as fallbacks.

### 3.5. Traces (`/v1/traces`) — optional

When to emit spans (in priority order, send what's cheap):

| span `name` | kind | useful attributes |
|---|---|---|
| `boot`                   | INTERNAL | App cold-start → first foreground frame |
| `screen.render.<name>`   | INTERNAL | Mount → first interactive |
| `mdm.sync`               | CLIENT   | `/api/v1/sync` request |
| `ml.inference`           | INTERNAL | `{model, tokens_in, tokens_out}` |
| `network.request`        | CLIENT   | Outbound HTTPS |

Required fields: `traceId` (32-hex), `spanId` (16-hex), `name`, `startTimeUnixNano`, `endTimeUnixNano`. Open spans (no `endTimeUnixNano`) are **rejected**. Span duration is computed server-side; clamped to ≥ 0.

---

## 4. Privacy — unchanged from v1.0

**Sender MUST filter out:**
- ❌ Chat content (LLM prompts / responses)
- ❌ Photo content
- ❌ GPS coordinates
- ❌ Audio recordings
- ❌ User-identifiable info (contacts, files, accounts)
- ❌ Sensitive device IDs (IMEI / MAC / Serial / Android-ID)

**Sender MAY collect:**
- ✅ Random per-install UUID (`device.id`) — not PII
- ✅ Device fingerprint: model, manufacturer, Android SDK
- ✅ App version
- ✅ Counters / durations / outcomes of LLM operations
- ✅ Error messages truncated to 200 chars (must not contain user input)

**Receiver-side:**
- 90-day retention for raw events (rollup tables retain forever).
- GDPR / 152-ФЗ erasure: identify by `device_id` (the MDM numeric id derived from the Bearer token) and batch-delete that device's rows from `device_logs`, `device_metrics`, `device_traces`.

---

## 5. Migration guide for the v1.0 sender

If you already implemented the v1.0 contract, three changes:

1. **Add the device token.** Right after `/api/v1/enroll` succeeds, store `device_token` in your secure storage. On every OTLP POST, set:
   ```http
   Authorization: Bearer <device_token>
   ```
   Without it the receiver returns 401. The token rotates only on re-enrollment.

2. **Drop the `X-Outpost-Device-Id` reliance.** The header may continue to be sent; the receiver ignores it. Use the `device.id` resource attribute for app-side correlation, not for receiver identification.

3. **(Optional) Wire metrics + traces.** Sender is welcome to keep emitting only logs in Phase 1 — the receiver accepts that. To enable richer dashboards, plug an OpenTelemetry SDK (Kotlin) and emit the metrics listed in §3.4 plus the spans in §3.5.

That's it. No JSON schema changes within the OTLP envelope.

---

## 6. Smoke test (for the sender)

After plugging in the token:

```bash
TOKEN="<device_token>"
curl -X POST https://mdm.secondf8n.tech/v1/logs \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"resourceLogs":[]}'
# expect: HTTP 200, body {"inserted":0}
```

Send a single record:

```bash
curl -X POST https://mdm.secondf8n.tech/v1/logs \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"outpost-android"}}]},"scopeLogs":[{"logRecords":[{"timeUnixNano":"1779000000000000000","severityNumber":9,"severityText":"INFO","body":{"stringValue":"smoke_test"}}]}]}]}'
# expect: HTTP 200, body {"inserted":1}
```

Verify it landed in the UI:

* `https://mdm.secondf8n.tech/telemetry` — overview KPIs increment
* `https://mdm.secondf8n.tech/devices/{your-device-id}/telemetry` — your device appears
* `https://mdm.secondf8n.tech/devices/{your-device-id}/logs` — the `smoke_test` event is in the log stream

Grafana fleet dashboard updates within one Prometheus scrape (≤30 s):
`https://mdm.secondf8n.tech/grafana/` → Outpost folder → "Fleet overview".

---

## 7. Why not OpenObserve / Loki / Honeycomb (briefly)

The v1.0 contract recommended OpenObserve as a single-binary minimal receiver. We tried it on 2026-05-17 evening:

* It ran (256 MB cap, ~200 MB RSS), but **requires Basic Auth on ingest by default** — there is no clean "anonymous Phase 1" mode without disabling auth globally. That conflicts with the v1.0 "no auth Phase 1" assumption.
* It does **not** know about MDM `customer_id` / `device_id`, so the UI cannot join telemetry to the device records page-by-page.
* Adding it on top of Prometheus + Grafana + outpost-server + nginx pushes the 1 GB box to ~80 % RAM.

The in-process Axum receiver wins on integration, costs ~1 MB RSS over baseline, and keeps the operational surface flat (one binary, one DB). External collectors remain a Phase-3 option if the device fleet outgrows SQLite — at which point we'd put `routes/otel.rs` in front of a real time-series store rather than rip it out.

---

## 8. Versioning

This contract is `OTEL-CONTRACT v2.0`. Breaking changes (rename of an endpoint, removal of an instrument kind, change to severity mapping) will land here and tag the server release as **v0.7+**. Non-breaking additions (new optional metric name, new attribute on an existing event) do not bump the contract.

To check compatibility from the device at app start, send the empty-batch smoke (§6). If you receive `200 OK` with `inserted:0`, you are on a compatible receiver.
