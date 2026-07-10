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

The `[sop]` table holds metadata and execution controls:

```toml
[sop]
name = "deploy-prod"
description = "Production deployment pipeline"
version = "0.1.0"              # optional, default "0.1.0"
priority = "normal"            # optional: low, normal, high, critical
execution_mode = "supervised"  # optional: auto, supervised, step_by_step, priority_based, deterministic
cooldown_secs = 0              # optional, default 0; minimum seconds between runs
max_concurrent = 1             # optional, default 1; max parallel runs of this SOP
deterministic = false          # optional; when true, overrides execution_mode to "deterministic"
```

### `[sop]` fields

| Field | Default | Effect |
|---|---:|---|
| `name` | *required* | SOP identifier used in CLI and tool calls. |
| `description` | *required* | Human-readable purpose. |
| `version` | `"0.1.0"` | Semantic version for tracking changes. |
| `priority` | `"normal"` | Scheduling priority: `low`, `normal`, `high`, `critical`. Affects `priority_based` execution mode. |
| `execution_mode` | `"supervised"` | Agent autonomy level (see below). |
| `cooldown_secs` | `0` | Minimum seconds between consecutive runs of this SOP. |
| `max_concurrent` | `1` | Maximum number of this SOP's runs that may execute in parallel. |
| `deterministic` | `false` | Shortcut: when `true`, sets `execution_mode = "deterministic"`. |

### Execution modes

| Mode | Behavior |
|---|---|
| `auto` | Execute all steps without human approval. |
| `supervised` | Request approval before starting, then execute all steps. |
| `step_by_step` | Request approval before each step. |
| `priority_based` | `critical`/`high` → auto; `normal`/`low` → supervised. |
| `deterministic` | Execute steps sequentially without LLM round-trips. Step outputs pipe as inputs to the next step. Checkpoint steps pause for human approval. |

### `[[triggers]]`

One or more trigger definitions. Each trigger has a `type` field that selects the variant:

```toml
[[triggers]]
type = "mqtt"
topic = "sensors/temperature/#"
condition = "$.value > 85"

[[triggers]]
type = "filesystem"
path = "config/*.toml"
events = ["modified"]
condition = "$.changed > 0"
```

See [Trigger Types](#4-trigger-types) for all available trigger types and their fields.

### `[[steps]]` (TOML-inline steps)

Steps may be defined directly in `SOP.toml` as an alternative to `SOP.md`:

```toml
[[steps]]
number = 1
title = "Preflight"
body = "Check service health and release window."
suggested_tools = ["http_request"]

[[steps]]
number = 2
title = "Deploy"
body = "Run deployment command."
suggested_tools = ["shell"]
requires_confirmation = true
on_failure = "retry:2"
```

When both `[[steps]]` in TOML and `## Steps` in `SOP.md` are present, the `SOP.md` steps take precedence.

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
- `- allow-tools:` and `- deny-tools:` define an explicit per-step tool scope.
  Group names expand: `fs`/`filesystem` → read_file, write_file, edit_file;
  `web`/`network` → http_request, web_search;
  `shell`/`terminal` → shell;
  `sop`/`sop-control` → sop_execute, sop_advance, sop_approve, sop_status.
- `- input:` and `- output:` attach JSON Schema-like step boundary contracts.
- `- next:` and `- depends_on:` route non-linear runs. Ineligible routed steps
  are marked `skipped` and leave the run `pending` instead of dispatching.
- `- when:` adds a guard condition evaluated against accumulated run data
  (same syntax as trigger `condition`). The step is marked `pending` instead of
  dispatched when the guard does not hold. Example: `- when: $.steps.1.ok == true`
- `- on_failure:` accepts `fail`, `retry:<count>`, or `goto:<step>` and is
  enforced for reported step failures and output schema failures.
- `- mode:` overrides the SOP execution mode for that step.
- `- kind:` sets the step type: `execute` (default), `checkpoint` (pauses for
  human approval), or `capability` (executed by the SOP capability registry).
- `- capability:` identifies the capability to invoke when `kind: capability`.
  Built-in capabilities: `noop`, `wait`, `json.validate`. Adapter-injected
  capabilities: `shell.exec`, `git.status`, `git.diff`, `notify.channel`.
- `- with:` supplies capability input parameters (JSON object). Merged with
  the piped step input under the `input` key.

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

For the live-versus-unwired status of each source and the transport details, see [SOP Fan-In](./fan-in/overview.md).

## 5. Condition Syntax

`condition` is evaluated fail-closed (invalid condition/payload => no match).

- JSON path comparisons: `$.value > 85`, `$.status == "critical"`
- Nested path comparisons: `$.sensor.temperature >= 30`, `$.event.type != "heartbeat"`
- Direct numeric comparisons: `> 0` (useful for simple payloads)
- Operators: `>=`, `<=`, `!=`, `>`, `<`, `==`

The same syntax applies to:

- Trigger `condition` fields — evaluated against the incoming event payload.
- Step `when` guards — evaluated against accumulated run data
  (e.g., `$.steps.3.ok == true` checks whether step 3 completed successfully).

### Condition examples

```toml
# MQTT trigger: only fire when the temperature reading exceeds threshold
[[triggers]]
type = "mqtt"
topic = "sensors/temperature/#"
condition = "$.value > 85"

# Filesystem trigger: only when files were actually changed
[[triggers]]
type = "filesystem"
path = "config/*.toml"
events = ["modified"]
condition = "$.changed > 0"

# Peripheral trigger: match a specific board signal with a threshold
[[triggers]]
type = "peripheral"
board = "rpi-4b"
signal = "gpio-17"
condition = "> 0"

# Channel trigger: only for opened PRs on a specific alias
[[triggers]]
type = "channel"
topic = "git.upstream:pull_request.opened"
condition = "$.action == "opened""
```

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
