# Logs & observability

Every event ZeroClaw emits flows through one crate: `zeroclaw-log`. The crate
owns the on-disk JSONL schema, the in-process broadcast stream the dashboard
reads, the bridge to the typed `Observer` (Prometheus / OTel), and the
macros (`record!`, `scope!`, `spawn!`) that subsystems call.

This page covers what an operator needs: configuration, where the log lives,
the shape of the events, and how to query them.

## Config (`[observability]`)

Defaults: `log_persistence = "rolling"`, `log_persistence_max_entries = 200`,
`log_tool_io = "redacted"`, `log_tool_io_truncate_bytes = 40960`,
`log_llm_request_payload = "off"`. A fresh
install produces a 200-event rolling JSONL at
`~/.zeroclaw/data/state/runtime-trace.jsonl`, and the dashboard's Logs page
works without further configuration.

`log_persistence = "none"` disables persistence entirely. The broadcast
stream (dashboard SSE) and the typed `Observer` bridge still receive
events; only the JSONL writer is gated.

### Archive rotation (`log_persistence = "rotating"`)

`rotating` persists every event like `full`, but ZeroClaw manages the active
file: it is rotated to a timestamped archive on a size and/or daily boundary,
and old archives are pruned by count and age. This differs from `rolling`,
which trims old entries out of the active file; rotated events are preserved in
archive files for later diagnostics.

| Key | Default | Effect |
| --- | --- | --- |
| `log_persistence_max_bytes` | `0` | Rotate once an append leaves the active file at or above this many bytes. `0` disables size rotation. |
| `log_persistence_rotate_daily` | `true` | Before the first event of a new UTC day, archive a file whose last write fell on an earlier day. |
| `log_persistence_retention_max_files` | `7` | Keep at most this many archives; after a rotation the oldest beyond the cap are deleted. `0` keeps all. |
| `log_persistence_retention_max_age_days` | `0` | Delete archives older than this many days after a rotation. `0` disables age-based cleanup. |

Archives sit next to the active file and keep its extension, with a sortable
UTC stamp inserted before that extension. For example, `runtime-trace.jsonl`
rotates to `runtime-trace.20260624-031500.jsonl`. The dashboard and the
`/api/logs` endpoint read the active file only, so archives are an on-disk
record for offline inspection rather than a live query surface.

Daily rotation keys off the UTC calendar, so its boundary may not line up with
local midnight in other time zones. These keys are ignored unless
`log_persistence = "rotating"`, and the `none`, `rolling`, and `full` modes are
unchanged.

### GenAI span attributes (`observability-otel`)

`llm.response` spans carry the OTel GenAI message-content attributes
`gen_ai.input.messages`, `gen_ai.output.messages`, and `gen_ai.system_instructions`
(JSON-string encoded), which populate the Input/Output/System panes in Langfuse/Tempo.

> **Privacy & cost.** Captured content is sanitized best-effort: inline image data is
> elided and known credential shapes (key=value, bearer, and `sk-`/`ghp_`/`xoxb-`-style
> prefixes) are redacted. This does NOT guarantee removal of all secrets or PII. Prefer
> an access-controlled trace backend if conversations may be sensitive. Capture cost is
> O(prompt size) **per agent-loop iteration** (the growing history is re-scanned each
> round), and full text grows per-span payload proportionally. On per-byte backends,
> apply exporter-side truncation rather than dropping the attributes.

### OTel Content Capture

OTel content capture is independent of log-based capture (`log_tool_io`, `log_llm_request_payload`). It controls what content is emitted as OpenTelemetry span attributes.

#### GenAI Content

Controls `gen_ai.system_instructions`, `gen_ai.input.messages`, and `gen_ai.output.messages` on OTel spans.

```toml
[observability]
otel_genai_content = "off"            # off | redacted | full
otel_genai_content_max_chars = 1000  # per-field truncation limit
```

- `off` (default): No content attributes, only metadata.
- `redacted`: Content is leak-scanned and truncated at `max_chars` per field.
- `full`: Content is leak-scanned but not truncated.

#### Tool I/O

Controls `gen_ai.tool.arguments`, `input.value`, `gen_ai.tool.result`, and `output.value` on OTel spans.

```toml
[observability]
otel_tool_io = "off"                  # off | redacted | full
otel_tool_io_max_chars = 1000        # per-field truncation limit
```

- `off` (default): No content attributes, only tool name + outcome.
- `redacted`: Content is leak-scanned and truncated at `max_chars` per field.
- `full`: Content is leak-scanned but not truncated.

#### Behavior Notes

- Setting `*_max_chars = 0` is equivalent to `off` for that policy.
- Content is always scrubbed (credential patterns + secret patterns) before truncation.
- Truncation preserves JSON structure for tool arguments (leaf strings truncated).
- Truncated fields get a `…[truncated {n} of {total} chars]` marker. The marker is metadata and does not count against `max_chars`: the kept content is exactly `max_chars` characters, with the marker appended on top.
- Default `off` is a privacy-first change from previous behavior (feature-gated but always-on when enabled).

### LLM request payload capture (`log_llm_request_payload`)

`log_llm_request_payload` controls whether the `llm_request` event records the
outbound prompt and conversation in addition to its `messages_count`. It is
**off by default** and is a privacy-sensitive surface: when enabled, ZeroClaw
persists the full system prompt plus the entire conversation history on every
turn.

| Value | What is captured |
| --- | --- |
| `off` (default) | Only `messages_count`. No message content is recorded; existing behavior. |
| `redacted` | Full message history (role + content), credential-scanned with the same `scrub_credentials` pass used for `raw_response` and tool I/O, then truncated at `log_tool_io_truncate_bytes`. Truncation is flagged with `request_messages_truncated` and `request_messages_original_bytes`. |
| `full` | Same credential scrubbing as `redacted`, but untruncated (replay fidelity, mirroring `raw_response`). |

Both `redacted` and `full` always run credential scrubbing; the only difference
between them is truncation. The capture reuses the existing
`log_tool_io_truncate_bytes` cap rather than introducing a second one. Set or
leave `log_llm_request_payload = "off"` to disable capture instantly, with no
redeploy.

## On-disk format

JSONL: one event per line, UTF-8, `0o600` permissions on Unix. Every
line is `sync_data`'d after write, the line is durable before the
emitting code returns.

Line shape mirrors `zeroclaw_log::event::LogEvent`. Top-level keys:

| Key | Type | Notes |
| --- | --- | --- |
| `id` | UUID v4 string | Persistent event id. |
| `@timestamp` | RFC 3339 + ms, UTC | Lexicographic-sortable; the reader sorts on this. |
| `severity_number` | u8 | OTel: 1 TRACE, 5 DEBUG, 9 INFO, 13 WARN, 17 ERROR. |
| `severity_text` | string | Bucket label for `severity_number`. |
| `event.category` | string | `agent`, `channel`, `cron`, `memory`, `tool`, `provider`, `session`, `system`, or `internal`. |
| `event.action` | string | Stable identifier (`llm_request`, `channel_message_inbound`, …). |
| `event.outcome` | string \| omitted | `success`, `failure`, `unknown` (omitted when `unknown`). |
| `service.name` | string | Constant `"zeroclaw"`. |
| `service.version` | string | Crate version of the running daemon. |
| `trace_id` | hex string \| omitted | Per-turn correlation. One agent turn = one trace_id. |
| `span_id` | hex string \| omitted | Sub-span within a turn. |
| `zeroclaw.*` | flat string map | Alias-bound attribution (see below). |
| `message` | string \| omitted | Human-readable line body. |
| `attributes` | object \| omitted | Free-form per-action payload. |
| `schema_version` | u8 | Currently `2`. v1 rows migrate in-place on startup. |

### `zeroclaw.*` attribution

The Rust source of truth is `ATTRIBUTION_FIELDS` + `COMPOSITE_PREFIXES`
in `crates/zeroclaw-log/src/event.rs`. The `/api/logs` response carries
the canonical list as `attribution_keys`; fetch it instead of
hard-coding.

Plain fields (`ATTRIBUTION_FIELDS`) carry a single string each.
Composite prefixes get three keys: `<prefix>`, `<prefix>_type`,
`<prefix>_alias` (e.g. `channel = "discord.glados"`,
`channel_type = "discord"`, `channel_alias = "glados"`). Filters can
match either coarse or precise.

When a tracing call sets a composite-prefix field to a bare type (no
`.`), only the `_type` slot is populated, that way a
`tracing::*!(model_provider = name, …)` call inside a span that
already carries the full `<type>.<alias>` composite doesn't clobber it
on the leaf→root merge.

## Querying

The dashboard's Logs page is the primary surface. Underneath:

```
GET /api/logs
```

Top-level filters (query params): `since_ts`, `until_ts`, `until_id`,
`action`, `category`, `outcome`, `severity_min`, `trace_id`, `q`
(substring across `message` + `attributes`), `hide_internal` (drops
`event.category = "internal"`), `limit`.

Every other `?<key>=<value>` is treated as a per-attribution equality
filter, the gateway validates the key against `is_attribution_field`
and rejects unknowns with `400`. The response includes
`attribution_keys: string[]`, so callers don't have to guess.

Examples:

<div class="os-tabs-src">

#### sh

```sh
# All WARN+ events since the daemon started.
curl "$ZEROCLAW_GATEWAY/api/logs?severity_min=13"

# A specific agent's events:
curl "$ZEROCLAW_GATEWAY/api/logs?agent_alias=glados"

# Discord traffic for one bot:
curl "$ZEROCLAW_GATEWAY/api/logs?channel=discord.glados"

# A single agent turn:
curl "$ZEROCLAW_GATEWAY/api/logs?trace_id=<value-from-a-prior-event>"
```

</div>

Pagination is reverse-cursor. The response includes
`next_cursor: [timestamp, id] | null`; pass these back as `until_ts` +
`until_id` to load older. `at_end: true` means the reader scanned the
whole file for the current filter.

The `/api/status` response includes `daemon_started_at: string` (RFC
3339), so a dashboard can default to "since daemon start" without an
extra round-trip.

## External log viewers

The JSONL schema is an OTel-logs + ECS hybrid: `@timestamp`,
`severity_number` + `severity_text`, `event.{category,action,outcome}`,
`service.{name,version}`, `attributes`, plus the `zeroclaw.*` vendor
namespace. Most log viewers ingest it with little or no transform.
Replace `<install>` with the absolute path to your install dir in the
examples below (typically `~/.zeroclaw` expanded).

### Grafana Loki

Promtail labels lift `agent_alias`, `channel`, and `severity_text` so
they're filterable in Grafana:

```yaml
scrape_configs:
  - job_name: zeroclaw
    static_configs:
      - targets: [localhost]
        labels:
          job: zeroclaw
          __path__: <install>/data/state/runtime-trace.jsonl
    pipeline_stages:
      - json:
          expressions:
            agent: zeroclaw.agent_alias
            channel: zeroclaw.channel
            level: severity_text
      - labels:
          agent:
          channel:
          level:
      - timestamp:
          source: '@timestamp'
          format: RFC3339
```

### OpenTelemetry Collector

The `filelog` receiver maps the schema directly. Export to any OTel
sink afterward (Tempo, Honeycomb, Datadog, etc.):

```yaml
receivers:
  filelog/zeroclaw:
    include: [<install>/data/state/runtime-trace.jsonl]
    operators:
      - type: json_parser
        timestamp:
          parse_from: attributes["@timestamp"]
          layout: '%Y-%m-%dT%H:%M:%S.%LZ'
        severity:
          parse_from: attributes.severity_number
```

### Kibana / Elastic

Ingest works as-is. Strict ECS pipelines expect `log.level` in place
of `severity_text`. A Filebeat ingest pipeline that renames
`severity_text` to `log.level` (and `severity_number` to
`log.syslog.severity.code`) covers the gap. `@timestamp` and
`event.{category,action,outcome}` are already in canonical positions.

### Vector / Fluent Bit

Both tail JSONL with a JSON parser stage; no schema transforms needed
before shipping to any backend.

## Terminal format

The daemon's stderr formatter prefixes every line with the closest
enclosing alias-bound identity:

- agent context → `[<agent_alias>]`
- channel-only context (channel listener, no agent yet) → `[<channel_composite>]` (e.g. `[discord.glados]`)
- otherwise → `[system]`

The span chain follows: `channel_listener{channel=discord.glados}: …`.
Span fields are visible inline.

## Schema migration

On startup, if `log_persistence` is enabled and the file exists, the
writer streams any schema-1 rows through an in-place migration to
schema-2 before the first append. Pure streaming, bounded by a
single line's allocation regardless of file size. The migrated file is
atomically renamed into place. Files already at v2 are left untouched.

If migration fails, the daemon logs a `warn` and continues writing v2
appends; the old v1 rows remain readable by tools that still
understand v1 but won't pass the v2 reader's deserializer.

## What is `internal`?

`event.category = "internal"` is the bucket for ops noise an operator
doesn't need on the dashboard by default: heartbeat ticks, idle
broadcasts, lossy sync retries, and the like. The dashboard's "Hide
internal" toggle (on by default) filters these.

Use it when you have a high-frequency event whose presence matters for
forensics but whose absence is the normal state. Don't use it as a
volume governor for genuine errors.

## Files of interest

- `crates/zeroclaw-log/src/event.rs`: the canonical `LogEvent` shape.
- `crates/zeroclaw-log/src/layer.rs`: the `tracing-subscriber` Layer
  that captures every `tracing::*` call and feeds the pipeline.
- `crates/zeroclaw-log/src/macro.rs`: `record!`, `scope!`, `spawn!`.
- `crates/zeroclaw-log/src/writer.rs`: append, rolling trim, and archive
  rotation.
- `crates/zeroclaw-log/src/reader.rs`: `/api/logs` reader.
- `crates/zeroclaw-log/src/config.rs`: `StoragePolicy`, `ToolIoPolicy`,
  `ResolvedPolicy`.
- `crates/zeroclaw-log/src/migrate.rs`: schema-1 → schema-2 streaming
  migration.
- `crates/zeroclaw-log/src/observer_bridge.rs`: typed `Observer`
  projection for Prometheus / OTel consumers.
- `crates/zeroclaw-gateway/src/api_logs.rs`: the HTTP adapter.

Touch the source before you trust the prose on this page.
