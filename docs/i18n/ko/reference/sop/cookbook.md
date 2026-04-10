# SOP Cookbook

런타임에서 지원하는 `SOP.toml` + `SOP.md` 형식의 실용적인 SOP 템플릿입니다.

## 1. 사람이 참여하는 배포

`SOP.toml`:

```toml
[sop]
name = "deploy-prod"
description = "Manual deployment with explicit approval gate"
version = "1.0.0"
priority = "high"
execution_mode = "supervised"
max_concurrent = 1

[[triggers]]
type = "manual"
```

`SOP.md`:

```md
## Steps

1. **Verify** — Check health metrics and rollout constraints.
   - tools: http_request

2. **Deploy** — Execute deployment command.
   - tools: shell
   - requires_confirmation: true
```

## 2. IoT 알림 핸들러 (MQTT)

`SOP.toml`:

```toml
[sop]
name = "high-temp-alert"
description = "Handle high temperature telemetry alerts"
version = "1.0.0"
priority = "critical"
execution_mode = "priority_based"

[[triggers]]
type = "mqtt"
topic = "sensors/temp/alert"
condition = "$.temperature_c >= 85"
```

`SOP.md`:

```md
## Steps

1. **Analyze** — Read the `Payload:` section in this SOP context and determine severity.
   - tools: memory_recall

2. **Notify** — Send an alert with site/device/severity summary.
   - tools: pushover
```

## 3. 일간 요약 (Cron)

`SOP.toml`:

```toml
[sop]
name = "daily-summary"
description = "Generate daily operational summary"
version = "1.0.0"
priority = "normal"
execution_mode = "supervised"

[[triggers]]
type = "cron"
expression = "0 9 * * *"
```

`SOP.md`:

```md
## Steps

1. **Collect Logs** — Gather recent errors and warnings.
   - tools: file_read

2. **Summarize** — Produce concise incident and trend summary.
   - tools: memory_store
```
