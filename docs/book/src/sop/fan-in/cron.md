# SOP Fan-In: Cron

> **Wired.** Cron triggers are dispatched by the daemon's periodic SOP maintenance tick, so this is a poller rather than a per-schedule timer. Firing needs a running `zeroclaw daemon` or gateway built with the `agent-runtime` feature, `sop.sops_dir` set, and `sop.maintenance_interval_secs` non-zero (default `60`). Schedules are parsed once at startup, so a SOP added while the daemon is running needs a reload before its cron trigger takes effect.

A cron trigger fires on a time window described by a cron expression. Invalid expressions fail closed during parsing and cache build.

## Trigger

{{#sop-trigger cron}}

## See also

- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md)
