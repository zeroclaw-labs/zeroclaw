# SOP 食谱

运行时支持的 `SOP.toml` + `SOP.md` 格式的实用 SOP 模板。

## 1. 人在回路部署

`SOP.toml`：

```toml
[sop]
name = \"deploy-prod\"
description = \"带显式审批门控的手动部署\"
version = \"1.0.0\"
priority = \"high\"
execution_mode = \"supervised\"
max_concurrent = 1

[[triggers]]
type = \"manual\"
```

`SOP.md`：

```md
## 步骤

1. **验证** — 检查健康指标和发布约束。
   - 工具：http_request

2. **部署** — 执行部署命令。
   - 工具：shell
   - 需要确认：true
```

## 2. IoT 告警处理器（MQTT）

`SOP.toml`：

```toml
[sop]
name = \"high-temp-alert\"
description = \"处理高温遥测告警\"
version = \"1.0.0\"
priority = \"critical\"
execution_mode = \"priority_based\"

[[triggers]]
type = \"mqtt\"
topic = \"sensors/temp/alert\"
condition = \"$.temperature_c >= 85\"
```

`SOP.md`：

```md
## 步骤

1. **分析** — 读取此 SOP 上下文中的 `Payload:` 部分并确定严重程度。
   - 工具：memory_recall

2. **通知** — 发送包含站点/设备/严重程度摘要的告警。
   - 工具：pushover
```

## 3. 每日摘要（Cron）

`SOP.toml`：

```toml
[sop]
name = \"daily-summary\"
description = \"生成每日运营摘要\"
version = \"1.0.0\"
priority = \"normal\"
execution_mode = \"supervised\"

[[triggers]]
type = \"cron\"
expression = \"0 9 * * *\"
```

`SOP.md`：

```md
## 步骤

1. **收集日志** — 收集最近的错误和警告。
   - 工具：file_read

2. **总结** — 生成简洁的事件和趋势摘要。
   - 工具：memory_store
```
