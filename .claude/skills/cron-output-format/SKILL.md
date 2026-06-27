---
name: cron-output-format
description: "Cron shell job output format control for ZeroClaw. Trigger when working on cron output formatting, `output_format`, raw vs wrapped shell output, or cron job configuration."
---

# Cron Output Format

Cron shell jobs support a configurable `output_format` field that controls how the command's stdout/stderr is presented.

## Values

| Value | Behavior |
|---|---|
| `"wrapped"` (default) | Includes `status=`, `stdout:`, `stderr:` labels |
| `"raw"` | Returns only stdout on success, or `exit code: N` + stderr on failure |

## How It Works

The `output_format` is defined in the **config schema** (`CronJobDecl`) as the source of truth. At runtime, `run_job_command_with_timeout()` resolves it from `config.cron` by job ID (Single Source of Truth — no cached field on `CronJob`).

For imperative jobs created via API/CLI (no config entry), the format defaults to `"wrapped"`.

## Design Constraints

- **Single Source of Truth**: `output_format` lives on `CronJobDecl` (config), never cached on the runtime `CronJob` struct
- Resolved from config on demand via `config.cron.get(&job.id)` at execution time
- Togglable from the web control panel shell job form (Wrapped / Raw toggle)
