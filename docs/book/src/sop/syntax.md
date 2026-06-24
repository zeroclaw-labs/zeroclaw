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
```

Parser behavior:

- Numbered items (`1.`, `2.`, ...) define step order.
- Leading bold text (`**Title**`) becomes step title.
- `- tools:` maps to `suggested_tools`.
- `- requires_confirmation: true` enforces approval for that step.

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
