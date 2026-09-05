# Logs & observability

Every event ZeroClaw emits flows through one crate: `zeroclaw-log`. The crate owns the on-disk JSONL schema, the in-process broadcast stream the dashboard reads, the optional bridge to the typed `Observer` (Prometheus / OTel), and the macros (`record!`, `scope!`, `spawn!`) that subsystems call.

This page covers what an operator needs: configuration, where the log lives,
the shape of the events, and how to query them.

## Config (`[observability]`)

Defaults: `log_persistence = "rolling"`, `log_persistence_max_entries = 200`,
`log_tool_io = "redacted"`, `log_tool_io_truncate_bytes = 40960`,
`log_llm_request_payload = "off"`. A fresh
install produces a 200-event rolling JSONL at
`~/.zeroclaw/data/state/runtime-trace.jsonl`, and the dashboard's Logs page
works without further configuration.

`log_persistence = "none"` disables persistence entirely but does not gate the broadcast stream used by dashboard SSE. The optional typed `Observer` bridge is also independent of persistence, but it receives canonical log events only when explicitly bound; the current production bootstrap does not install that binding.

Persistence is best-effort rather than a transactional audit guarantee. The Observer bridge, when bound, and broadcast delivery happen before the event is offered to a bounded background-writer queue. A full queue or worker write failure can leave an event out of JSONL. Periodic sync covers the current active file; daily rotation before a new UTC day's first append and size rotation after a threshold-crossing append can rename the active file without first syncing it, so the cadence does not bound durability for a just-rotated archive. See [Logging architecture](../architecture/logging.md#delivery-surfaces-have-different-guarantees) for the separate delivery contracts.

### Archive rotation (`log_persistence = "rotating"`)

`rotating` retains every event by rotating the active file rather than trimming it. The active file is renamed to a timestamped archive on a size, daily-boundary, or entry-count trigger. Old archives are pruned by count and age after each rotation.

| Key | Default | Effect |
| --- | --- | --- |
| `log_persistence_max_bytes` | `0` | Rotate once an append leaves the active file at or above this many bytes. `0` disables size rotation. |
| `log_persistence_rotate_daily` | `true` | Before the first event of a new UTC day, archive a file whose last write fell on an earlier day. |
| `log_persistence_max_entries_per_segment` | `0` | Rotate once the segment's non-empty line count reaches this cap. In steady state each archive holds exactly this many entries. When first enabled on an existing log, the file is archived whole (one over-cap transition); steady state resumes on the next rotation. `0` disables entry-count rotation. |
| `log_persistence_retention_max_files` | `7` | Keep at most this many archives; after a rotation the oldest beyond the cap are deleted. `0` keeps all. |
| `log_persistence_retention_max_age_days` | `0` | Delete archives older than this many days after a rotation. `0` disables age-based cleanup. |

Archives sit next to the active file and keep its extension, with a sortable
UTC stamp inserted before that extension. For example, `runtime-trace.jsonl`
rotates to `runtime-trace.0000000001-20260624-031500.jsonl`. The sequence
prefix (`0000000001`) is written at rotation time and determines reader-side
ordering; it never repeats across restarts. Archives written before sequence
numbering existed keep their old shape (`runtime-trace.20260624-031500.jsonl`)
and sort before every numbered archive.

The dashboard and `GET /api/logs` now read the active file **and** all retained
archives as one logical event stream, merging them oldest-archive-first and
returning events newest-first. The API exposes a segment-aware cursor
(`next_segment_cursor`) alongside the existing byte-offset cursor
(`next_cursor_line_offset`); pass `?until_segment_cursor=` on subsequent
requests to paginate across segment boundaries. Old byte-offset cursors remain
valid and are interpreted as an offset into the active file.

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
- The content policy is bound to the observer/config instance, not to the process. There is no process-global OTel content policy: each `OtelObserver` derives an immutable content config from `ObservabilityConfig` at construction and consults it at the OTel export boundary. Multiple observers in the same process keep independent policies: a later observer cannot override or silence an earlier one's privacy setting (no last-writer-wins, no cross-observer drift).

### Turn-nested memory and RAG spans (`observability-otel`)

`memory.recall`, `memory.store`, and `rag.retrieve` spans nest under the
`gen_ai.agent.invoke` turn span whenever the operation runs inside an
attributed agent turn, so a full turn (memory recall, autosave store,
LLM calls, tool calls) renders as one trace in Langfuse/Tempo. The three
events carry the same `channel` / `agent_alias` / `turn_id` triple as LLM
and tool events, exposed as `zeroclaw.channel`, `gen_ai.agent.name`, and
`zeroclaw.turn_id` span attributes.

Memory operations outside a correlated turn keep producing root spans: the
gateway REST memory store, and the `process_message` hardware-RAG
retrieval, which runs before the turn bracket opens and therefore stays a
root span carrying the matching `zeroclaw.turn_id` attribute (full nesting
of that span is tracked in #8844). A `turn_id` that no longer matches a
live turn also degrades to a root span rather than guessing a parent.

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

JSONL: one event per line, UTF-8, `0o600` permissions on Unix. The
hot path is non-blocking: `record_event` hands the serialized event
to a dedicated background thread (`zeroclaw-log-writer`) via a bounded
channel and returns immediately. The worker calls `sync_all` on a
periodic cadence: every 100 writes or every 1 second of wall-clock
time, whichever fires first, plus a final `sync_all` when the channel
closes on normal shutdown. This trades per-event durability (the prior
synchronous behaviour) for bounded write latency: a process crash may
lose up to one sync interval of pending writes. If the worker falls
behind, `record_event` drops the event with a `tracing::warn!` rather
than blocking the async runtime. Workers are per-process singletons;
disabling and re-enabling persistence via `init_from_config` drops the
old worker (channel close triggers its final sync and thread exit) and
spawns a fresh one.

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

Top-level filters (query params): `since_ts`, `until_ts`, `until_line_offset`, `until_segment_cursor`, `action`, `category`, `outcome`, `severity_min`, `trace_id`, `q` (substring across `message` + `attributes`), `hide_internal` (drops `event.category = "internal"`), `limit`. The legacy `until_id` field remains available for timestamp/ID cursor compatibility.

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

Log pagination walks backward with a segment-aware cursor. While `at_end` is false:
1. Prefer `next_segment_cursor`, passed back unchanged as `until_segment_cursor`. Treat it as an **opaque token**: its internal shape depends on which segment the page ended in and may change between releases. Clients must round-trip the returned string verbatim rather than constructing or parsing one. This is the only cursor that can advance once the oldest event in a page is in a rotated archive.
2. Fall back to `next_cursor_line_offset` passed back as `until_line_offset` when the segment cursor is absent. This is a plain byte offset into the active file and resolves to `null` when the oldest event is in an archive.
3. The legacy `next_cursor: [timestamp, id] | null` response remains for compatibility; passing it back as `until_ts` + `until_id` is deprecated because the lexicographic ID tie-break can silently skip events with the same timestamp.

Restart from the newest page after changing filters. Treat `at_end: true` as the signal to stop requesting older pages for that walk.

`at_end` is scoped to the segments the daemon could actually read. When a retained segment cannot be opened, it is logged, left out of the merged view, and the response sets `incomplete: true`. The page is still returned, but `at_end` then means "no older events among the segments that could be read" rather than "no older events exist". Present such a walk as partial rather than complete. The same applies to a single-event lookup: rather than reporting a miss as `not found`, the daemon says the event was not found *and* that part of the retained history was unreadable, so it may still exist. Older daemons omit the field; treat its absence as `false`.

`until_line_offset` is a position in the current active file. Archive rotation, startup migration, and a configured path change replace the bytes or active file it refers to; restart from the newest page after those boundaries.

`until_segment_cursor` is resilient across those boundaries, by different means depending on where the page ended:

- **A page ending in an archive** is addressed by the archive's own identity, which is fixed when the archive is written and never reassigned to different content. Subsequent rotations therefore cannot invalidate it. If retention has since deleted that archive, the reader reports the history as finished rather than silently resuming at an unrelated position.
- **A page ending in the active file** carries an anchor event id alongside the offset, because the active file's path is stable while its content is replaced on each rotation. On resume the reader checks that the event at the cursor boundary still matches the anchor; on a mismatch it searches the retained segments for that event and resumes from wherever it now lives, so pagination crosses a rotation without duplicating or skipping events. If the anchored event is gone entirely, the reader reports the history as finished.

A cursor issued by an older daemon, which named a segment by filename without an anchor, is still accepted. That form cannot say whether it means a rotated archive or the active file, so the reader tries the archives first, where a name is never reassigned, and falls back to the active file.

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
