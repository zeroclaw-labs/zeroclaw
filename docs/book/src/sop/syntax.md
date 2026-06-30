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

Schema enforcement fails closed: invalid step input prevents the step from
starting, and invalid step output is routed through the step's `on_failure`
policy. Routing enforcement replaces linear `current_step + 1` advancement in
LLM and deterministic runs. Tool-scope enforcement narrows the live step turn's
available tools and blocks scoped-out calls at dispatch.

Untrusted trigger topic and payload text is capped, normalized, screened, and
framed before it reaches step context. Framing is always on; the warning text can
be hidden, but raw external trigger text is not interpolated into the model
context.

## 4. Trigger Types

| Type | Fields | Notes |
|---|---|---|
| `manual` | none | Triggered by tool `sop_execute` (not a `zeroclaw sop run` CLI command). |
| `webhook` | `path` | Exact match against the event `path`. Defined and matched, but no live event source is wired (see [Connectivity](./connectivity.md)). |
| `mqtt` | `topic`, optional `condition` | MQTT topic supports `+` and `#` wildcards. |
| `filesystem` | `path`, optional `events`, optional `condition` | `path` supports `*`/`**`/`?` globs; a bare directory matches changes at or under it. `events` narrows by kind (`created`, `modified`, `deleted`, `renamed`); empty matches all. |
| `cron` | `expression` | Supports 5, 6, or 7 fields (5-field gets seconds prepended internally). |
| `peripheral` | `board`, `signal`, optional `condition` | Matches `"{board}/{signal}"`. |

## 5. Condition Syntax

`condition` is evaluated fail-closed (invalid condition/payload => no match).

- JSON path comparisons: `$.value > 85`, `$.status == "critical"`
- Direct numeric comparisons: `> 0` (useful for simple payloads)
- Operators: `>=`, `<=`, `!=`, `>`, `<`, `==`

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
