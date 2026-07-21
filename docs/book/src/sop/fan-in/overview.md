# SOP Fan-In: Overview

A **fan-in** is an external event source that starts SOP runs. Each source delivers events to the SOP engine through `dispatch_sop_event`, which matches every event against every loaded SOP's triggers and starts runs for those that match.

One ZeroClaw instance can bind several fan-ins at once: an MQTT topic, a filesystem path, and an AMQP routing key can all feed the same engine without separate processes. Each source has a dedicated guide below.

## How dispatch works

- **One matcher path:** a single matcher evaluates every trigger type, so matching behaves the same regardless of source.
- **Run-start audit:** started runs are persisted via `SopAuditLogger`.
- **Headless safety:** in non-agent-loop contexts, `process_headless_results` logs `ExecuteStep` actions as pending instead of silently executing them.
- **Untrusted input:** topic and payload text are capped, normalized, prompt-guard screened, and framed before reaching model context.

## Sources

Every SOP trigger type, its fields, and its dispatch status, projected directly from the `SopTrigger` registry:

{{#sop-trigger-index}}

Each source has a dedicated guide in the sidebar. Live sources (delivered by a running listener) start runs as events arrive; cron triggers are dispatched by the daemon's periodic SOP maintenance tick; agent-initiated runs start from inside an agent turn via [`sop_execute`](./manual.md); the remaining defined-but-unwired sources (webhook, peripheral, calendar) validate and match but have no live event source routing into the dispatcher yet.

## Security defaults

| Concern | Mechanism |
|---|---|
| **MQTT transport** | `mqtts://` with `use_tls = true` for TLS transport |
| **Filesystem roots** | Broad roots (`/`, `/home`, `/etc`, `/var`, `/proc`, `/sys`, `/dev`, `/tmp`) rejected at config validation unless `allow_broad_roots`; include/exclude globs scope events |
| **Filesystem symlinks** | Symlink event paths are rejected before any metadata, hash, or content read by default; `follow_symlinks = true` opts in but still requires the canonical target to resolve inside a watched root |
| **Untrusted trigger input** | Topic and payload text are capped, normalized, prompt-guard screened, and framed before model context |
| **Unsafe trigger block** | `untrusted_input_guard = "block"` refuses unsafe untrusted events with `BlockedUnsafe`; default `warn` audits and allows |
| **Cron validation** | Invalid cron expressions fail closed during parsing and cache build |
| **Headless dispatch** | Headless callers log run progression instead of auto-executing `ExecuteStep` |

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| SOP never starts from a live source | trigger pattern mismatch or a failing `condition` | Verify the trigger pattern matches the delivered event; check the `condition` against the payload |
| SOP started but a step did not execute | headless trigger without an active agent loop | Run an agent loop for `ExecuteStep`, or design the run to pause on approvals |
| Webhook, peripheral, or calendar trigger never fires | event source not wired into the dispatcher | Use a live source ([MQTT](./mqtt.md), [Filesystem](./filesystem.md), [AMQP](./amqp.md)) or start the run with [`sop_execute`](./manual.md) |
| Cron trigger never fires | maintenance tick not running (no `zeroclaw daemon` or `zeroclaw channel start`; standalone `gateway start` does not run it), `sops_dir` unset, or `maintenance_interval_secs = 0` | Run `zeroclaw daemon` (or `zeroclaw channel start`) with `sop.sops_dir` set and `sop.maintenance_interval_secs` non-zero (default `60`) |

## See also

- [Syntax](../syntax.md): the full `SOP.toml` and `SOP.md` format
- [How SOPs run](../how-it-works.md)
- [Channels: Overview](../../channels/overview.md): the transport side of MQTT, filesystem, and AMQP
