# SOP Cookbook

Practical SOP templates in the runtime-supported `SOP.toml` + `SOP.md` format.

## 1. Human-in-the-Loop Deployment

The `SOP.toml` defines the trigger and steps (see [Syntax](./syntax.md)). The `SOP.md` body:

```md
## Steps

1. **Verify** — Check health metrics and rollout constraints.
   - tools: http_request

2. **Deploy** — Execute deployment command.
   - tools: shell
   - requires_confirmation: true
```

## 2. IoT Alert Handler (MQTT)

The `SOP.toml` defines the trigger and steps (see [Syntax](./syntax.md)). The `SOP.md` body:

```md
## Steps

1. **Analyze** — Read the `Payload:` section in this SOP context and determine severity.
   - tools: memory_recall

2. **Notify** — Send an alert with site/device/severity summary.
   - tools: pushover
```

## 3. Daily Digest (Cron)

The `SOP.toml` defines the trigger and steps (see [Syntax](./syntax.md)). The `SOP.md` body:

```md
## Steps

1. **Collect Logs** — Gather recent errors and warnings.
   - tools: file_read

2. **Summarize** — Produce concise incident and trend summary.
   - tools: memory_store
```
