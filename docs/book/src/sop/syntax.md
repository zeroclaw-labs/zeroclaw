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

## 2. Authoring Boundary

The file-backed representation still contains a manifest file plus `SOP.md`.
This page intentionally does not enumerate manifest fields or provide
hand-authored manifest examples.

Use this page for the syntax that remains visible when reviewing, validating, or
debugging SOPs: `SOP.md` step bullets, trigger field summaries generated from
the runtime schema, and `condition` expressions. Before running a generated or
checked-in SOP, validate it with `zeroclaw sop validate <name>`.

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

Routing and approval bullets can be combined in the same `SOP.md` steps:

```md
## Steps

1. **Classify event** — Inspect the incoming payload.
   - output: {"type":"object","required":["severity"],"properties":{"severity":{"type":"string"}}}
   - when: $.steps.1.severity == "critical"
   - next: 2

2. **Prepare summary** — Build the operator-facing remediation plan.
   - depends_on: 1
   - on_failure: retry:2
   - next: 3

3. **Approval gate** — Require explicit approval before changing state.
   - kind: checkpoint
   - requires_confirmation: true
   - next: 4

4. **Apply remediation** — Execute the approved action.
   - tools: shell
   - allow-tools: shell
   - on_failure: goto:5

5. **Notify operator** — Send a failure notice for follow-up.
   - tools: http_request
```

Parser behavior:

- Numbered items (`1.`, `2.`, ...) define step order.
- Leading bold text (`**Title**`) becomes step title.
- `- tools:` maps to `suggested_tools`.
- `- requires_confirmation: true` enforces approval for that step.
- `- kind:` accepts `execute` (default) or `checkpoint`. A checkpoint step
  pauses deterministic execution at that step. Use `requires_confirmation: true`
  when a step must require approval in any execution mode.
- `- allow-tools:` and `- deny-tools:` define an explicit per-step tool scope.
- `- input:` and `- output:` attach JSON Schema-like step boundary contracts.
- `- when:` is a routing guard evaluated against accumulated completed-step
  outputs after the current step finishes. When it does not match, the run
  completes instead of dispatching another step.
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

For the live-versus-unwired status of each source and the transport details, see [SOP Fan-In](./fan-in/overview.md).

## 5. Condition Syntax

Trigger `condition` fields and step `when:` guards use the same expression
grammar. Trigger conditions evaluate against the event payload. Step `when:`
guards evaluate against accumulated completed-step outputs in this shape:

```json
{
  "steps": {
    "1": {
      "severity": "critical"
    }
  }
}
```

Evaluation is fail-closed for invalid conditions, missing payloads, unresolved
JSON paths, and direct numeric comparisons whose payload or comparand is not a
number. An empty condition matches unconditionally.

### JSON Path Form

A condition beginning with `$` compares a value inside a JSON payload:
`$.path.to.field <op> <value>`.

| Expression | Payload | Matches |
|---|---|---|
| `$.value > 85` | `{"value":90}` | yes |
| `$.value >= 85` | `{"value":85}` | yes |
| `$.temp < 25` | `{"temp":20}` | yes |
| `$.temp <= 25` | `{"temp":25}` | yes |
| `$.status == "critical"` | `{"status":"critical"}` | yes |
| `$.status != "error"` | `{"status":"ok"}` | yes |
| `$.count == 42` | `{"count":42}` | yes |
| `$.data.sensor.value > 85` | `{"data":{"sensor":{"value":87.3}}}` | yes |
| `$.readings.1 == 20` | `{"readings":[10,20,30]}` | yes |
| `$.active == "true"` | `{"active":true}` | yes |
| `$.nonexistent > 0` | `{"value":90}` | no |

Path rules:

- Use dot-separated segments. Array elements use a numeric segment such as
  `$.readings.1`; bracket syntax is not supported.
- Missing keys, out-of-range array indexes, invalid JSON, and empty payloads
  fail closed.
- There are no wildcards, filters, recursive descent, or built-in variables.

### Direct Numeric Form

A condition with no leading `$` compares the whole payload as a number. This is
useful for scalar event payloads.

| Expression | Payload | Matches |
|---|---|---|
| `> 0` | `1` | yes |
| `> 0` | `0` | no |
| `>= 5` | `6` | yes |
| `< 100` | `50` | yes |
| `== 42` | `42` | yes |
| `!= 0` | `1` | yes |
| `> 3.14` | `3.15` | yes |
| `> 0` | `not a number` | no |

### Operators

A comparison uses one operator, matched longest-first: `>=`, `<=`, `!=`, `==`,
`>`, `<`. JSON-path comparisons try numeric comparison first. If both sides
parse as numbers, they compare numerically; otherwise values compare as strings.
Surrounding double quotes on the comparand are stripped, so quote string
literals: `$.status == "critical"`. Direct numeric conditions are numeric-only:
if either side does not parse as a number, there is no match.

A condition is a single comparison. Logical combinators such as `AND`, `OR`,
and `NOT` are not supported.

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
