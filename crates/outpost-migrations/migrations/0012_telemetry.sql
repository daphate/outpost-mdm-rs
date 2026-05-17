-- Device telemetry — OTLP ingest sink.
--
-- Why: phones are going to clients as demo units; we have to see whether they
-- launched the app, what they tapped, what crashed. Headwind's `devicelog`
-- plugin (dropped in v0.1 plan) is replaced here by full OTLP-compatible
-- ingest: spans, metrics, log records.
--
-- The schema is permissive — every OTLP-shaped record lands as a row, with
-- the high-cardinality bits in JSON columns (attributes, resource). Indices
-- target the common dashboard query patterns: per-device timeline, per-name
-- aggregation, recent-N pagination.

-- ---------------------------------------------------------------------------
-- Log records (severity-tagged events: clicks, errors, lifecycle, ANR/crash)
-- ---------------------------------------------------------------------------
CREATE TABLE device_logs (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id     INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    device_id       INTEGER NOT NULL REFERENCES devices(id)   ON DELETE CASCADE,
    -- ISO-8601 UTC; falls back to server-side datetime('now') if device omits.
    ts              TEXT    NOT NULL,
    -- OTLP SeverityNumber 1..24 (TRACE/DEBUG/INFO/WARN/ERROR/FATAL etc.)
    severity_number INTEGER NOT NULL DEFAULT 9,  -- 9 = INFO
    -- Human-readable text from OTLP severityText, or derived if missing.
    severity_text   TEXT    NOT NULL DEFAULT 'INFO',
    -- Free-form payload — usually a short string ("screen.opened",
    -- "click", "error.uncaught_exception").
    body            TEXT    NOT NULL,
    -- OTLP attributes flattened to JSON: {"k":"v",...}
    attrs_json      TEXT    NOT NULL DEFAULT '{}',
    -- OTLP Resource attributes (service.name, host.id, etc.) at ingest time.
    resource_json   TEXT    NOT NULL DEFAULT '{}',
    -- Trace context (optional)
    trace_id        TEXT,
    span_id         TEXT,
    received_at     TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_device_logs_device_ts    ON device_logs(device_id, ts);
CREATE INDEX idx_device_logs_customer_ts  ON device_logs(customer_id, ts);
CREATE INDEX idx_device_logs_severity     ON device_logs(severity_number, ts);
CREATE INDEX idx_device_logs_received     ON device_logs(received_at);

-- ---------------------------------------------------------------------------
-- Metric data points (counters, gauges; histograms reduced to bucket sums)
-- ---------------------------------------------------------------------------
CREATE TABLE device_metrics (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id     INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    device_id       INTEGER NOT NULL REFERENCES devices(id)   ON DELETE CASCADE,
    ts              TEXT    NOT NULL,
    -- OTLP metric name, e.g. "app.session_seconds", "battery.pct"
    name            TEXT    NOT NULL,
    -- OTLP Instrument kind: "gauge" | "sum" | "histogram" | "summary"
    kind            TEXT    NOT NULL DEFAULT 'gauge',
    -- Value as REAL for everything; histograms record `count` here and the
    -- raw histogram payload in attrs_json under "_hist".
    value           REAL    NOT NULL DEFAULT 0,
    -- Per-point attributes (label set).
    attrs_json      TEXT    NOT NULL DEFAULT '{}',
    resource_json   TEXT    NOT NULL DEFAULT '{}',
    -- OTLP unit (e.g. "s", "By", "1")
    unit            TEXT,
    received_at     TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_device_metrics_device_ts     ON device_metrics(device_id, ts);
CREATE INDEX idx_device_metrics_customer_ts   ON device_metrics(customer_id, ts);
CREATE INDEX idx_device_metrics_name_ts       ON device_metrics(name, ts);
CREATE INDEX idx_device_metrics_received      ON device_metrics(received_at);

-- ---------------------------------------------------------------------------
-- Spans / traces
-- ---------------------------------------------------------------------------
CREATE TABLE device_traces (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    customer_id       INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    device_id         INTEGER NOT NULL REFERENCES devices(id)   ON DELETE CASCADE,
    -- 16-byte hex (OTLP TraceId)
    trace_id          TEXT    NOT NULL,
    -- 8-byte hex (OTLP SpanId)
    span_id           TEXT    NOT NULL,
    parent_span_id    TEXT,
    name              TEXT    NOT NULL,
    -- OTLP SpanKind: 0..5 (INTERNAL/SERVER/CLIENT/PRODUCER/CONSUMER)
    kind              INTEGER NOT NULL DEFAULT 0,
    -- Span boundaries as ISO-8601 (or nanos converted server-side).
    start_ts          TEXT    NOT NULL,
    end_ts            TEXT    NOT NULL,
    duration_ms       INTEGER NOT NULL DEFAULT 0,
    -- OTLP StatusCode: 0=UNSET, 1=OK, 2=ERROR
    status_code       INTEGER NOT NULL DEFAULT 0,
    status_message    TEXT,
    attrs_json        TEXT    NOT NULL DEFAULT '{}',
    resource_json     TEXT    NOT NULL DEFAULT '{}',
    received_at       TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_device_traces_trace_id      ON device_traces(trace_id);
CREATE INDEX idx_device_traces_device_start  ON device_traces(device_id, start_ts);
CREATE INDEX idx_device_traces_customer_st   ON device_traces(customer_id, start_ts);
CREATE INDEX idx_device_traces_received      ON device_traces(received_at);

-- ---------------------------------------------------------------------------
-- Aggregated daily activity (rolled up nightly by the scheduler — not part
-- of OTLP, but cheap precomputed roll-up for the /telemetry dashboard).
-- One row per (device_id, day, metric).
-- ---------------------------------------------------------------------------
CREATE TABLE device_activity_daily (
    customer_id     INTEGER NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    device_id       INTEGER NOT NULL REFERENCES devices(id)   ON DELETE CASCADE,
    day             TEXT    NOT NULL,  -- YYYY-MM-DD UTC
    log_count       INTEGER NOT NULL DEFAULT 0,
    error_count     INTEGER NOT NULL DEFAULT 0,
    metric_count    INTEGER NOT NULL DEFAULT 0,
    trace_count     INTEGER NOT NULL DEFAULT 0,
    last_seen       TEXT,
    PRIMARY KEY (device_id, day)
);

CREATE INDEX idx_activity_customer_day ON device_activity_daily(customer_id, day);
