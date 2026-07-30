# SOP Fan-In: Overview

A **fan-in** is an external event source that starts SOP runs. Each source delivers events to the SOP engine through `dispatch_sop_event`, which matches every event against every loaded SOP's triggers and starts runs for those that match.

One ZeroClaw instance can bind several fan-ins at once: an MQTT topic, a filesystem path, and an AMQP routing key can all feed the same engine without separate processes. Each source has a dedicated guide below.

## How dispatch works

- **One matcher path:** a single matcher evaluates every trigger type, so matching behaves the same regardless of source.
- **Run-start audit:** started runs are persisted via `SopAuditLogger`.
- **Headless execution:** cron triggers drive their runs to completion. The maintenance tick hands each started `ExecuteStep` or `DeterministicStep` to the headless run driver, which executes the step as the SOP's own agent. Sources that still have no live driver log `ExecuteStep` actions as pending through `process_headless_results` instead of executing them.
- **Headless ownership:** a headless step runs as the agent named by the step's `agent`, falling back to the SOP's `agent`. There is no ambient agent turn to inherit an identity from, so a procedure with no owner is **rejected at authoring time** and refused at dispatch; it is never run as some other configured agent.
- **Headless tool scope:** a headless step turn is narrowed exactly like a live one: the SOP control tools (`sop_execute`, `sop_advance`, `sop_approve`) are always removed so a step cannot drive its own run, and the step's declared `scope` is applied on top when `sop.step_scope_enforce` is set.
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
| **Headless ownership** | A headless SOP must declare an owning `agent`; authoring blocks the save and the driver refuses the step rather than borrowing another configured agent |
| **Headless tool scope** | Headless step turns always exclude the SOP control tools and honour the step's declared `scope` under `step_scope_enforce` |
| **Headless driver lifetime** | Cron drivers are owned by the daemon generation that started them and are drained (then aborted after 30s) before a reload rebuilds the config and SOP engine |
| **Headless dispatch** | Sources with no live driver log run progression instead of auto-executing `ExecuteStep` |

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| SOP never starts from a live source | trigger pattern mismatch or a failing `condition` | Verify the trigger pattern matches the delivered event; check the `condition` against the payload |
| SOP started but a step did not execute | a source with no live driver (webhook, peripheral, calendar) reached `ExecuteStep` | Use cron or a live source, run an agent loop for `ExecuteStep`, or design the run to pause on approvals |
| Cron SOP starts, then fails every run with "no owning agent" | the procedure declares no `agent`, and a headless run has none to inherit | Set `agent` in `SOP.toml` (or on the step) to a configured agent alias |
| A cron step cannot call a tool it used interactively | the step's `scope` excludes it under `step_scope_enforce`, or it is a SOP control tool | Widen the step's `scope`; the SOP control tools stay excluded by design |
| Webhook, peripheral, or calendar trigger never fires | event source not wired into the dispatcher | Use a live source ([MQTT](./mqtt.md), [Filesystem](./filesystem.md), [AMQP](./amqp.md)) or start the run with [`sop_execute`](./manual.md) |
| Cron trigger never fires | maintenance tick not running (no `zeroclaw daemon` or `zeroclaw channel start`; standalone `gateway start` does not run it), `sops_dir` unset, or `maintenance_interval_secs = 0` | Run `zeroclaw daemon` (or `zeroclaw channel start`) with `sop.sops_dir` set and `sop.maintenance_interval_secs` non-zero (default `60`) |

## See also

- [Syntax](../syntax.md): the full `SOP.toml` and `SOP.md` format
- [How SOPs run](../how-it-works.md)
- [Channels: Overview](../../channels/overview.md): the transport side of MQTT, filesystem, and AMQP
