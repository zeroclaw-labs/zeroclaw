# SOP Observability & Audit

This page covers where SOP execution evidence is stored and how to inspect it.

## 1. Audit Persistence

SOP audit entries are persisted via `SopAuditLogger` into the configured Memory backend, category `sop`.

Common key patterns:

- `sop_run_{run_id}`: run snapshot (start + completion updates)
- `sop_step_{run_id}_{step_number}`: per-step result
- `sop_approval_{run_id}_{step_number}`: operator approval record
- `sop_timeout_approve_{run_id}_{step_number}`: timeout auto-approval record

## 2. Inspection Paths

### 2.1 Run logs

Every provider call, tool call, and lifecycle event emitted inside an SOP step
inherits the stable `zeroclaw.sop_run_id` attribution. The gateway run-detail
page displays these events as a timeline. The CLI and ZeroCode Logs pane query
the same persisted, scrubbed runtime trace rather than maintaining separate SOP
log storage:

Each completed, failed, or skipped step also emits a structured result event.
Its message contains a short result summary and its `attributes.output` contains
up to 4,096 characters of credential-scrubbed output, together with the step
number, status, effective agent, and captured tool-call count. The canonical
untruncated step result remains in the SOP audit record; the bounded projection
keeps the shared runtime log safe to render and search.

<div class="os-tabs-src">

#### sh

```sh
zeroclaw sop logs <run-id>
zeroclaw sop logs <run-id> --limit 500 --json
```

</div>

In ZeroCode, open Logs and press `r` to enter a run ID; press `R` to clear the
run filter. API clients can use `GET /api/logs?sop_run_id=<run-id>` and paginate
with `next_cursor_line_offset` just like any other log query.

The reader includes a narrow compatibility bridge for events written before
`zeroclaw.sop_run_id` existed when those rows carried `attributes.run_id` or a
legacy SOP audit start/finish message. Provider and tool events from older runs
cannot be reconstructed if they never carried the run ID.

Run logs follow the runtime trace's configured persistence and retention policy.
The gateway reads only the current active JSONL file, so a sufficiently old run
may have no retained rows after rolling trim or rotation. The UI and CLI report
when persistence is disabled; rotated archives remain offline diagnostic
artifacts.

An `observability-otel` build with `backend = "otel"` also exports these same
canonical events as standard OTLP/HTTP protobuf LogRecords. Provider, tool, and
step rows retain `zeroclaw.sop_run_id` as an OTel attribute, so a collector or
vendor can query the same run without replacing the local dashboard/CLI/TUI
path. See [Logs & observability](../ops/observability.md#native-otlp-logs-observability-otel).

### 2.2 Definition-level CLI

<div class="os-tabs-src">

#### sh

```sh
zeroclaw sop list
zeroclaw sop validate [name]
zeroclaw sop show <name>
```

</div>

### 2.3 Runtime run-state tools

SOP run state is queried from in-agent tools:

- `sop_status`: active/finished runs and optional metrics
- `sop_status` with `include_gate_status: true`: trust phase and gate evaluator state (when available)
- `sop_approve`: approve waiting run step
- `sop_advance`: submit step result and move run forward

## 3. Metrics

- `/metrics` exposes observer metrics when `[observability] backend = "prometheus"`.
- Current exported names are `zeroclaw_*` families (general runtime metrics).
- SOP-specific aggregates are available through `sop_status` with `include_metrics: true`.
