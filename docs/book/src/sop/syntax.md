# SOP Syntax Reference

SOP definitions are loaded from subdirectories under `sops_dir`. When `sops_dir` is omitted from config, CLI commands fall back to `<workspace>/sops` for offline inspection, but runtime SOP execution is disabled.

## 1. Directory Layout

```text
<workspace>/sops/
  deploy-prod/
    SOP.toml
    SOP.md
```

Each SOP must have `SOP.toml`. `SOP.md` is optional, but runs with no parsed steps will fail validation.

## 2. `SOP.toml`

`SOP.toml` is the SOP manifest. It declares the procedure's metadata in a `[sop]` table and its event sources in a `[[triggers]]` array. Step definitions live in `SOP.md`, not in the manifest. This per-SOP `[sop]` table is distinct from the global engine `[sop]` config documented later under Step Contract Enforcement.

### The `[sop]` table

| Field | Required | Default | Description |
|---|---|---|---|
| `name` | yes | | SOP name. |
| `description` | yes | | Human-readable summary. |
| `version` | no | `0.1.0` | Semantic version string. |
| `priority` | no | `normal` | One of `low`, `normal`, `high`, `critical`. Drives mode resolution under `priority_based`. |
| `execution_mode` | no | `supervised` | One of `auto`, `supervised`, `step_by_step`, `priority_based`, `deterministic`. |
| `cooldown_secs` | no | `0` | Minimum seconds between runs of this SOP. |
| `max_concurrent` | no | `1` | Maximum concurrent runs of this SOP. |
| `deterministic` | no | `false` | When `true`, forces `deterministic` execution. |

`execution_mode` controls how a run asks for approval: `auto` runs end to end with no approval, `supervised` approves once at the start, `step_by_step` approves each step, `priority_based` picks `auto` for `critical` or `high` priority and `supervised` otherwise, and `deterministic` pipes each step's output into the next step's input with no LLM round-trip between them.

### The `[[triggers]]` array

Each trigger is a TOML table tagged by its lowercase `type`. The `type` selects which fields the trigger accepts; see Trigger Types below for the per-variant field reference. A manifest may declare more than one trigger.

```toml
[sop]
name = "test-sop"
description = "A test SOP"

[[triggers]]
type = "manual"

[[triggers]]
type = "webhook"
path = "/sop/test"
```

## 3. `SOP.md` Step Format

Steps are parsed from the `## Steps` section.

```md
## Steps

1. **Preflight** — Check service health and release window.
   - tools: http_request

2. **Deploy** — Run deployment command.
   - tools: shell
   - requires_confirmation: true
   - input: {"type":"object","required":["version"],"properties":{"version":{"type":"string"}}}
   - output: {"type":"object","required":["digest"],"properties":{"digest":{"type":"string"}}}
   - next: 3
```

Parser behavior:

- Numbered items (`1.`, `2.`, ...) define step order.
- Leading bold text (`**Title**`) becomes step title.
- `- tools:` maps to `suggested_tools`.
- `- requires_confirmation: true` enforces approval for that step.
- `- kind:` sets the step kind: `execute` (default) or `checkpoint`. A `checkpoint` step pauses the run for human approval before continuing.
- `- when:` is an optional routing guard that uses the same expression grammar as trigger `condition` (see Condition Syntax), evaluated against the accumulated run data. When the guard does not hold, the run completes and no further steps execute.
- `- allow-tools:` and `- deny-tools:` define an explicit per-step tool scope.
- `- input:` and `- output:` attach JSON Schema-like step boundary contracts.
- `- next:` and `- depends_on:` route non-linear runs. Ineligible routed steps
  are marked `skipped` and leave the run `pending` instead of dispatching.
- `- on_failure:` accepts `fail`, `retry:<count>`, or `goto:<step>` and is
  enforced for reported step failures and output schema failures.
- `- mode:` overrides the SOP execution mode for that step.

### Step Contract Enforcement

Step contracts are optional. When present, `input` and `output` accept a compact
JSON object with `type`, `required`, `properties`, and `items` fields. The
supported primitive types are `object`, `array`, `string`, `number`, `integer`,
`boolean`, and `null`.

The `[sop]` config controls enforcement:

| Field | Default | Effect |
|---|---:|---|
| `step_schema_enforce` | `true` | Validate declared step input/output schemas at engine boundaries. |
| `step_scope_enforce` | `false` | Treat per-step tool scopes as enforced filters instead of advisory hints. |
| `step_mandatory_tools` | `["sop_advance", "sop_approve", "sop_status"]` | Keep lifecycle tools available while scope enforcement is enabled. |
| `max_step_visits` | `256` | Stop routed runs that revisit one step too many times. |
| `max_step_retries` | `2` | Limit retries requested by a step failure policy. |
| `untrusted_payload_max_bytes` | `8192` | Cap untrusted trigger topic/payload text at a UTF-8 character boundary; `0` disables the cap. |
| `untrusted_input_guard` | `"warn"` | Prompt-guard action for untrusted trigger input: `warn`, `block`, or `sanitize`. |
| `untrusted_guard_sensitivity` | `0.7` | Sensitivity used by prompt-guard screening and outbound redaction. |
| `untrusted_frame_warning` | `true` | Include explanatory warning text in the untrusted-content frame. Frame boundaries remain enabled. |
| `untrusted_outbound_redact` | `true` | Enable shared outbound redaction for SOP content-safety consumers. |
| `procedural_memory_enabled` | `false` | Register the `sop_workshop` tool for proposal capture, review, and explicit SOP write-back. |

Schema enforcement fails closed: invalid step input prevents the step from
starting, and invalid step output is routed through the step's `on_failure`
policy. Routing enforcement replaces linear `current_step + 1` advancement in
LLM and deterministic runs. Tool-scope enforcement narrows the live step turn's
available tools and blocks scoped-out calls at dispatch.

Untrusted trigger topic and payload text is capped, normalized, screened, and
framed before it reaches step context. Framing is always on; the warning text can
be hidden, but raw external trigger text is not interpolated into the model
context.

Procedural memory is opt-in. When enabled, `sop_workshop` can create and inspect
stored SOP proposals, capture completed run context into a candidate procedure,
and apply an approved proposal to `SOP.toml`/`SOP.md`. Write-back only happens
through the explicit `apply` action.

## 4. Trigger Types

{{#sop-trigger-index}}

Each trigger is a `[[triggers]]` table tagged by its lowercase `type`. The fields each type accepts:

| `type` | Fields | Notes |
|---|---|---|
| `mqtt` | `topic`, `condition` (optional) | `topic` supports `+` (one level) and `#` (remaining levels). Live via the MQTT listener. |
| `webhook` | `path` | Matched exactly against the request path. No live route feeds it. |
| `cron` | `expression` | A cron expression. No scheduler feeds it. |
| `peripheral` | `board`, `signal`, `condition` (optional) | Matched as `board/signal`. No live listener. |
| `filesystem` | `path`, `events` (optional), `condition` (optional) | `path` is a glob (`*`, `**`, `?`); a bare directory matches anything under it. `events` is one or more of `created`, `modified`, `deleted`, `renamed`. Live via the filesystem watcher. |
| `calendar` | `calendar_source`, `calendar_ids` (optional) | `calendar_ids` scopes to specific calendars; empty matches all. No live poller. |
| `manual` | none | Agent-initiated via the `sop_execute` tool. |
| `amqp` | `routing_key`, `condition` (optional) | Topic-exchange semantics: `.`-delimited words, `*` one word, `#` zero or more words. Live via the AMQP consumer. |

For the live-versus-unwired status of each source and the transport details, see [SOP Fan-In](./fan-in/overview.md).

## 5. Condition Syntax

A trigger's `condition` is an optional expression evaluated against the event payload; the run starts only when it holds. Step `when:` guards use the same grammar, evaluated against the accumulated run data. Evaluation is fail-closed: a missing or empty payload, an unparseable condition, an unresolved path, or non-comparable values all mean no match, so the trigger does not fire. An empty condition matches unconditionally.

There are two forms.

### JSON path form

A condition that starts with `$` compares a value inside a JSON payload: `$.path.to.field <op> <value>`. The path is dot-separated object keys, with a bare number addressing an array element.

| Expression | Payload | Matches |
|---|---|---|
| `$.value > 85` | `{"value": 90}` | yes |
| `$.value >= 85` | `{"value": 85}` | yes |
| `$.temp < 25` | `{"temp": 20}` | yes |
| `$.temp <= 25` | `{"temp": 25}` | yes |
| `$.status == "critical"` | `{"status": "critical"}` | yes |
| `$.status != "error"` | `{"status": "ok"}` | yes |
| `$.count == 42` | `{"count": 42}` | yes |
| `$.data.sensor.value > 85` | `{"data": {"sensor": {"value": 87.3}}}` | yes |
| `$.readings.1 == 20` | `{"readings": [10, 20, 30]}` | yes |
| `$.active == "true"` | `{"active": true}` | yes |
| `$.nonexistent > 0` | `{"value": 90}` | no (unresolved path, fail-closed) |

Path rules:

- Dot-only paths. There is no bracket syntax: write `$.readings.1`, not `$.readings[1]`.
- No wildcards, no recursive descent, no filters. A missing key fails closed.
- A numeric segment addresses an array index.

Comparison rules:

- Numeric comparison is tried first. If both sides parse as numbers, they compare as numbers.
- Otherwise values compare as strings. Surrounding double quotes on the comparand are stripped, so quote string literals: `$.status == "critical"`.
- JSON booleans serialize as the strings `true` and `false`, so compare them as quoted strings: `$.active == "true"`.

### Direct form

A condition with no leading `$` compares the whole payload as a number. This suits peripheral triggers whose payload is a single scalar reading.

| Expression | Payload | Matches |
|---|---|---|
| `> 0` | `1` | yes |
| `> 0` | `0` | no |
| `>= 5` | `6` | yes |
| `< 100` | `50` | yes |
| `== 42` | `42` | yes |
| `!= 0` | `1` | yes |
| `> 3.14` | `3.15` | yes |

A non-numeric payload fails closed.

### Operators

A comparison uses one operator, matched longest-first: `>=`, `<=`, `!=`, `==`, `>`, `<`. There is no other operator.

### What is not supported

A condition is a single comparison. There are no logical combinators (`AND`, `OR`, `NOT`) and no built-in variables (no `$now`, `$run`, `$step`, and so on). To require multiple events, declare multiple triggers or nest SOPs.

## 6. Validation

Use:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw sop validate
zeroclaw sop validate <name>
```

</div>

Validation warns on empty names/descriptions, missing triggers, missing steps, and step numbering gaps.
