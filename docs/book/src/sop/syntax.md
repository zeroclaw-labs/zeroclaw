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
- `- next:` and `- depends_on:` declare route metadata for non-linear runs.
- `- on_failure:` accepts `fail`, `retry:<count>`, or `goto:<step>`.
- `- mode:` overrides the SOP execution mode for that step.

### Step Contract Metadata

Step contracts are optional. When present, `input` and `output` accept a compact
JSON object with `type`, `required`, `properties`, and `items` fields. The
supported primitive types are `object`, `array`, `string`, `number`, `integer`,
`boolean`, and `null`.

The `[sop]` config reserves the enforcement knobs for this contract surface:

| Field | Default | Effect |
|---|---:|---|
| `step_schema_enforce` | `true` | Enables fail-closed schema validation once the engine enforcement slice is active. |
| `step_scope_enforce` | `false` | Enables per-step tool-scope filtering once the turn-loop filter slice is active. |
| `step_mandatory_tools` | `["sop_advance", "sop_approve", "sop_status"]` | Keeps lifecycle tools available while scope enforcement is enabled. |
| `max_step_visits` | `256` | Bounds routed runs that revisit one step. |
| `max_step_retries` | `2` | Bounds retries requested by a step failure policy. |

This metadata is parsed and preserved by the SOP loader. Route replacement,
schema enforcement, and turn-loop tool-scope filtering are activated by later
runtime slices.

## 4. Trigger Types

| Type | Fields | Notes |
|---|---|---|
| `manual` | none | Triggered by tool `sop_execute` (not a `zeroclaw sop run` CLI command). |
| `webhook` | `path` | Exact match against the event `path`. Defined and matched, but no live event source is wired (see [Connectivity](./connectivity.md)). |
| `mqtt` | `topic`, optional `condition` | MQTT topic supports `+` and `#` wildcards. |
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
