# OTLP contract — Outpost-Android → Outpost MDM server

**Status:** Stable for v0.6.x. Server-side receiver code:
[`crates/outpost-server/src/routes/otel.rs`](../crates/outpost-server/src/routes/otel.rs).
Tests in [`crates/outpost-server/tests/otel.rs`](../crates/outpost-server/tests/otel.rs).

This document is the canonical reference for the **sender** (Outpost-Android
app, written in a parallel session). The server-side endpoints described
below are already implemented and integration-tested. Anything not in this
document is **not** in the receiver — please coordinate before assuming.

---

## Why telemetry matters

Demo units go to clients. We need to see — without intruding — whether the
client opens the app, what they do in it, what errors they hit. The
telemetry stream is the **only** loopback that tells us if a demo is alive
or sitting in a drawer.

Goals (in order of priority):

1. **Was the app ever launched?** App-start timestamp, install timestamp.
2. **How often is it used?** Foreground time per day, session count.
3. **What features do they touch?** Screen-opened events, model-load
   events, model-inference events (without payload content, only counts +
   timings).
4. **What errors do they hit?** Uncaught exceptions, ANRs, network
   failures, model-load failures.
5. **What hardware do we have visibility into?** Battery, network type,
   storage free, RAM available.

---

## Transport

* **Protocol:** OTLP/HTTP-JSON (NOT protobuf, NOT gRPC). Spec:
  <https://opentelemetry.io/docs/specs/otlp/#otlphttp>
* **Encoding:** `Content-Type: application/json`
* **Authentication:** `Authorization: Bearer <device_token>` on every
  request. The token is the long-lived JWT issued at `/api/v1/enroll`. It
  is the **same** token the device uses for `/api/v1/sync` — do not invent
  a second credential.
* **Compression:** **not used in v0.6** — body limit is 200 MiB, so even
  uncompressed batches fit. Gzip support can land later if traffic
  warrants; do not enable it yet on the sender side.

### Endpoints

| Endpoint | Body | Empty-batch response |
|---|---|---|
| `POST https://mdm.secondf8n.tech/v1/logs`    | `ExportLogsServiceRequest` JSON     | `{"inserted":0}` |
| `POST https://mdm.secondf8n.tech/v1/metrics` | `ExportMetricsServiceRequest` JSON  | `{"inserted":0}` |
| `POST https://mdm.secondf8n.tech/v1/traces`  | `ExportTraceServiceRequest` JSON    | `{"inserted":0}` |

### Response shape

```json
{ "inserted": <i64> }     // number of records actually persisted
```

* `200 OK` — batch accepted. `inserted` is the row count.
* `400 Bad Request` — malformed JSON. Body says what's wrong.
* `401 Unauthorized` — missing, expired, revoked, or wrong-kind token.
  **Do not retry with the same token** — re-enroll.
* `413 Payload Too Large` — single batch > 200 MiB. Chunk it.
* `5xx` — server bug; retry with exponential backoff.

---

## Batching & retry policy (recommended)

* **Batch size:** up to **500 records per batch** OR **1 MiB on the wire**,
  whichever is smaller. The server has no hard cap below 200 MiB but
  smaller batches make ingest latency observable.
* **Flush triggers:** every **30 s**, OR when batch hits 500 records, OR
  when severity ≥ ERROR (flush immediately).
* **Network gating:** **send only on Wi-Fi by default**, fall back to
  cellular only if explicitly opted in via app setting. Telemetry must
  not eat data plans.
* **Persistence:** queue locally to a SQLite buffer; drop oldest when the
  buffer exceeds 20 MiB or 50 000 records.
* **Retry:** exponential backoff 5 s → 30 s → 5 min, max 5 min. Drop the
  batch only after 24 h of continuous failure.

---

## Resource attributes (every batch)

Set on the OTLP `Resource` object **once per batch**, NOT per record:

```json
{
  "attributes": [
    {"key":"service.name",         "value":{"stringValue":"outpost-android"}},
    {"key":"service.version",      "value":{"stringValue":"<your app version>"}},
    {"key":"device.id",            "value":{"stringValue":"<HW serial / IMEI>"}},
    {"key":"device.model",         "value":{"stringValue":"Ulefone Armor 28 Ultra"}},
    {"key":"os.type",              "value":{"stringValue":"android"}},
    {"key":"os.version",           "value":{"stringValue":"14"}}
  ]
}
```

The receiver does **not** need `device.id` for auth (the bearer token
identifies the device), but the value is preserved for debugging.

---

## Logs (`/v1/logs`)

### Schema

The receiver persists each `LogRecord` to `device_logs` with the columns:

| OTLP field | DB column | Notes |
|---|---|---|
| `timeUnixNano` (preferred) or `observedTimeUnixNano` | `ts` | If missing, server-side `datetime('now')`. |
| `severityNumber` | `severity_number` | RFC 5424–style mapping (TRACE=1..4, INFO=9..12, ERROR=17..20, FATAL=21..24). |
| `severityText`    | `severity_text` | Falls back to mapping from `severityNumber` if absent. |
| `body.stringValue` | `body` | Other AnyValue variants are JSON-stringified. |
| `attributes`     | `attrs_json` (JSON object) | Flattened from KeyValue[]. |
| (resource)       | `resource_json`    | Stored once per record so cross-record queries are cheap. |
| `traceId`        | `trace_id`         | Optional. |
| `spanId`         | `span_id`          | Optional. |

### Required log signals — minimum set

The dashboard at `/telemetry` keys off these names. Emit them all:

| `body` | When | `attributes` |
|---|---|---|
| `app.launched`           | App's first foreground after install or reboot | `{install_seq:<n>, since_install_seconds:<n>}` |
| `app.foregrounded`       | Each time app comes to foreground | `{prev_state:"background"\|"new"}` |
| `app.backgrounded`       | Each time app goes to background | `{session_seconds:<n>}` |
| `screen.opened`          | A top-level screen mounts | `{screen:"home"\|"library"\|"chat"\|...}` |
| `model.load.started`     | LLM/Whisper/TTS load begins | `{model:"<id>", kind:"llm"\|"whisper"\|"tts"\|"mmproj"}` |
| `model.load.finished`    | Load succeeds | `{model:"<id>", duration_ms:<n>}` |
| `model.load.failed`      | Load errors | `{model:"<id>", error:"<short message>"}` |
| `inference.started`      | An LLM/translator inference call begins | `{model:"<id>", role:"llm"\|"translator"}` |
| `inference.finished`     | Same call ends OK | `{model:"<id>", tokens_in:<n>, tokens_out:<n>, duration_ms:<n>}` |
| `inference.failed`       | Same call errors | `{model:"<id>", error:"<short>"}` |
| `network.unavailable`    | App detected no usable network | `{kind:"wifi"\|"cell"\|"none"}` |
| `error.uncaught_exception` | Crash handler caught a throwable | `{type:"<exception class>", message:"<short>"}` |
| `error.anr`              | ANR detected | `{thread:"<name>"}` |

**severityNumber convention:**
* `info` → 9 (TRACE/DEBUG/INFO/WARN/ERROR/FATAL = 1/5/9/13/17/21).
* App-launched, screen-opened, lifecycle → severityNumber `9`.
* Warn-class (slow network, retry succeeded) → `13`.
* `model.load.failed`, `inference.failed`, `network.unavailable` → `17`
  (ERROR).
* `error.uncaught_exception`, `error.anr` → `21` (FATAL).

The `/telemetry` overview counts `severityNumber >= 17` as "errors" in
all KPI cards.

---

## Metrics (`/v1/metrics`)

### Supported instrument kinds

The receiver flattens each data point in:
* `gauge.dataPoints[]`
* `sum.dataPoints[]`
* `histogram.dataPoints[]` (the histogram payload is reduced to `count` —
  full bucket layout is dropped; if histograms matter, prefer many
  per-bucket gauges)
* `summary.dataPoints[]` (same — only `count` is stored)
* `exponentialHistogram` — **not yet** stored; do not send

`asDouble`, `asInt` (integer or string), and `count` are all accepted for
the value.

### Required metric set — minimum

| `name` | Kind | Unit | Attributes | Notes |
|---|---|---|---|---|
| `app.session_seconds`       | `sum`   | `s`  | (none) | Cumulative foreground time since install. |
| `app.foreground_ms`         | `gauge` | `ms` | (none) | Last session duration. |
| `app.crashes`               | `sum`   | `1`  | (none) | Lifetime crash count. |
| `app.anr_count`             | `sum`   | `1`  | (none) | Lifetime ANR count. |
| `battery.pct`               | `gauge` | `1`  | `{state:"charging"\|"discharging"}` | 0..100. |
| `battery.charging`          | `gauge` | `1`  | (none) | 0 or 1. |
| `network.requests_total`    | `sum`   | `1`  | `{result:"ok"\|"error"}` | All outbound HTTPS (excluding telemetry POSTs themselves — don't recurse). |
| `network.errors_total`      | `sum`   | `1`  | `{kind:"timeout"\|"5xx"\|"dns"\|"tls"\|"reset"}` | |
| `ml.inference_ms`           | `gauge` | `ms` | `{model, role}` | Last finished inference. |
| `ml.queue_depth`            | `gauge` | `1`  | (none) | Pending inference requests. |
| `storage.free_mb`           | `gauge` | `MiBy` | (none) | Internal storage. |
| `ram.available_mb`          | `gauge` | `MiBy` | (none) | App's perception of free RAM. |

These names align with the hard-coded list in the receiver's
`/metrics` endpoint — Prometheus picks up `outpost_metric_latest{name=…}`
exactly for these.

### Don't send

* Per-event-id metrics. Use logs for events; metrics for numbers that move
  continuously.
* High-cardinality labels (UUIDs, user-supplied strings). The server has
  no enforced cap but operators will hate you.
* PII. Hash device serials if anything else needs to reference them.

---

## Traces (`/v1/traces`)

### When to use

Spans are the right tool for **measuring multi-step operations** where you
want to see which step took how long. Examples we want:

| Span `name` | Kind | What goes inside |
|---|---|---|
| `boot`                 | INTERNAL | App cold-start, end at first foreground frame. |
| `screen.render.<name>` | INTERNAL | Per-screen mount → first interactive. |
| `mdm.sync`             | CLIENT   | `/api/v1/sync` request lifecycle. |
| `ml.inference`         | INTERNAL | Inference request, with `{model, tokens_in, tokens_out}` on attributes. |
| `network.request`      | CLIENT   | Any outbound HTTPS; child spans for DNS/TLS/HTTP. |

### Required fields

The receiver maps OTLP `Span` → `device_traces`:

| OTLP | DB column | Notes |
|---|---|---|
| `traceId` (32-hex) | `trace_id` | |
| `spanId`  (16-hex) | `span_id`  | |
| `parentSpanId`     | `parent_span_id` | Empty string → NULL. |
| `name`             | `name` | |
| `kind`             | `kind` | 0 INTERNAL / 1 SERVER / 2 CLIENT / 3 PRODUCER / 4 CONSUMER. |
| `startTimeUnixNano`| `start_ts` | Falls back to `datetime('now')` if missing. |
| `endTimeUnixNano`  | `end_ts`   | |
| (computed)         | `duration_ms` | Server computes `end - start`; clamped to ≥0. |
| `status.code`      | `status_code` | 0 UNSET / 1 OK / 2 ERROR. |
| `status.message`   | `status_message` | |
| `attributes`       | `attrs_json` | JSON object. |
| (resource)         | `resource_json` | |

### Don't send

* Spans without `endTimeUnixNano` — open spans are not supported in v0.6.
* Spans wider than 10 s for app-lifecycle, or 5 min for any single span.
  Long spans wreck dashboard time-bucketing.

---

## What the server does with the data

* **Logs:** persisted to `device_logs`. The `/telemetry` overview and
  `/devices/{id}/logs` HTMX pages query this table directly.
* **Metrics:** persisted to `device_metrics`. The `/metrics` Prometheus
  endpoint exposes a curated subset (the names listed above) as
  `outpost_metric_latest{name="…"}` gauges so Prometheus and Grafana can
  graph them. Histogram/exponential payloads are reduced to a scalar
  `count`.
* **Traces:** persisted to `device_traces`. The `/devices/{id}/telemetry`
  HTMX page lists the latest 20 spans per device with name, duration,
  status. Full waterfall view is on the roadmap (likely Tempo
  integration once we have the budget).

There is **no exemplar-trace correlation, no log-trace correlation, and
no metric-exemplar correlation** in v0.6. Send `trace_id`/`span_id` on
log records freely — they are stored, but not yet rendered as links.

---

## Versioning

This contract is `OTEL-CONTRACT v0.6`. A breaking change to any of the
above (rename of an endpoint, change of an enum, removal of a metric from
the curated Prometheus set) will land in this doc and tag the server
release as `v0.7`.

To check compatibility from the device, send a single empty batch:

```bash
curl -X POST https://mdm.secondf8n.tech/v1/logs \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"resourceLogs":[]}'
```

If you get `{"inserted":0}` with `200 OK`, the contract is honoured.
