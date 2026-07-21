# SOP Fan-In: Cron

> **Wired.** Cron triggers are dispatched by the periodic SOP maintenance tick, so this is a poller rather than a per-schedule timer. Firing needs that tick, which a `zeroclaw daemon` or the `zeroclaw channel start` supervisor spawns (built with the `agent-runtime` feature, `sop.sops_dir` set, and `sop.maintenance_interval_secs` non-zero, default `60`). Standalone `zeroclaw gateway start` does **not** spawn the maintenance tick, so it does not fire cron triggers; a gateway hosted inside the daemon does, because the daemon spawns the tick. Schedules are parsed once at startup, so a SOP added while the daemon is running needs a reload before its cron trigger takes effect.

A cron trigger fires on a time window described by a cron expression. Invalid expressions fail closed during parsing and cache build.

## Trigger

{{#sop-trigger cron}}

## See also

- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md)
