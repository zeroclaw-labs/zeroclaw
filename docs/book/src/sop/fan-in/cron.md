# SOP Fan-In: Cron

> **Wired.** Cron triggers are dispatched by the periodic SOP maintenance tick, so this is a poller rather than a per-schedule timer. Firing needs that tick, which a `zeroclaw daemon` or the `zeroclaw channel start` supervisor spawns (built with the `agent-runtime` feature, `sop.sops_dir` set, and `sop.maintenance_interval_secs` non-zero, default `60`). Standalone `zeroclaw gateway start` does **not** spawn the maintenance tick, so it does not fire cron triggers; a gateway hosted inside the daemon does, because the daemon spawns the tick. Schedules are parsed once at startup, so a SOP added while the daemon is running needs a reload before its cron trigger takes effect.

A cron trigger fires on a time window described by a cron expression. Invalid expressions fail closed during parsing and cache build.

## Trigger

{{#sop-trigger cron}}

## Execution

A cron-started run is driven to completion without an agent loop attached. The maintenance tick hands each started `ExecuteStep` to the headless run driver, which executes the step body as an agent turn and advances the run; `DeterministicStep` actions route through the engine's deterministic driver instead. A run that reaches an approval gate or checkpoint parks there, exactly as it would under a live agent.

Two constraints apply to the step turn, both of them fail-closed:

- **The SOP must name its owning agent.** Set `agent` in `SOP.toml` (or `agent` on an individual step, which overrides it) to a configured agent alias. A cron run has no ambient agent turn to inherit an identity from, so an unowned procedure is rejected when it is authored, and the driver refuses the step rather than running it as some other configured agent. The step runs with that agent's provider, workspace, tools, and risk profile.
- **The step's tool scope is enforced.** The SOP control tools (`sop_execute`, `sop_advance`, `sop_approve`) are always removed from a step turn, so an automatic run cannot drive itself. When `sop.step_scope_enforce` is set, the step's declared `scope` narrows the surface further.

In-flight cron drivers belong to the daemon generation that started them. On reload the daemon stops the maintenance tick, lets running drivers finish under the configuration they started with (up to 30 seconds, then aborts them), and only then rebuilds the config and SOP engine, so no step straddles two configurations.

## See also

- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md)
