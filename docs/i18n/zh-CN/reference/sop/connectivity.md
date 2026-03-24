# SOP 连接与事件扇入

本文档描述外部事件如何触发 SOP 运行。

## 快速路径

- [MQTT 集成](#2-mqtt-集成)
- [Webhook 集成](#3-webhook-集成)
- [Cron 集成](#4-cron-集成)
- [安全默认值](#5-安全默认值)
- [故障排除](#6-故障排除)

## 1. 概述

ZeroClaw 通过统一的 SOP 调度器（`dispatch_sop_event`）路由 MQTT/webhook/cron/外围设备事件。

关键行为：

- **一致的触发器匹配：** 所有事件源使用同一个匹配器路径。
- **运行启动审计：** 已启动的运行通过 `SopAuditLogger` 持久化。
- **无头安全：** 在非代理循环上下文中，`ExecuteStep` 操作会被记录为待处理（不会静默执行）。

## 2. MQTT 集成

### 2.1 配置

在 `config.toml` 中配置 broker 访问：

```toml
[channels_config.mqtt]
broker_url = \"mqtts://broker.example.com:8883\"  # 明文使用 mqtt://
client_id = \"zeroclaw-agent-1\"
topics = [\"sensors/alert\", \"ops/deploy/#\"]
qos = 1
username = \"mqtt-user\"      # 可选
password = \"mqtt-password\"  # 可选
use_tls = true              # 必须与 scheme 匹配（mqtts:// => true）
```

### 2.2 触发器定义

在 `SOP.toml` 中：

```toml
[[triggers]]
type = \"mqtt\"
topic = \"sensors/alert\"
condition = \"$.severity >= 2\"
```

MQTT  payload 会被转发到 SOP 事件 payload（`event.payload`），然后显示在步骤上下文中。

## 3. Webhook 集成

### 3.1 端点

- **`POST /sop/{*rest}`**：仅 SOP 端点。如果没有 SOP 匹配则返回 `404`。无 LLM 回退。
- **`POST /webhook`**：聊天端点。首先尝试 SOP 调度；如果不匹配，回退到正常 LLM 流程。

路径匹配与配置的 webhook 触发器路径精确匹配。

示例：

- SOP 中的触发器路径：`path = \"/sop/deploy\"`
- 匹配请求：`POST /sop/deploy`

### 3.2 授权

启用配对时（默认），提供：

1. `Authorization: Bearer <token>`（来自 `POST /pair`）
2. 可选第二层：配置 webhook 密钥时提供 `X-Webhook-Secret: <secret>`

### 3.3 幂等性

使用：

`X-Idempotency-Key: <unique-key>`

默认值：

- TTL：300秒
- 重复响应：`200 OK` 带 `\"status\": \"duplicate\"`

幂等性密钥按端点命名空间区分（`/webhook` 和 `/sop/*` 分开）。

### 3.4 示例请求

```bash
curl -X POST http://127.0.0.1:3000/sop/deploy \
  -H \"Authorization: Bearer <token>\" \
  -H \"X-Idempotency-Key: $(uuidgen)\" \
  -H \"Content-Type: application/json\" \
  -d '{\"message\":\"deploy-service-a\"}'
```

典型响应：

```json
{
  \"status\": \"accepted\",
  \"matched_sops\": [\"deploy-pipeline\"],
  \"source\": \"sop_webhook\",
  \"path\": \"/sop/deploy\"
}
```

## 4. Cron 集成

调度器使用基于窗口的检查评估缓存的 cron 触发器。

- **基于窗口：** 不会遗漏 `(last_check, now]` 内的事件。
- **每个刻度每个表达式最多一次：** 如果一个轮询窗口内有多个触发点，仅调度一次。

触发器示例：

```toml
[[triggers]]
type = \"cron\"
expression = \"0 0 8 * * *\"
```

Cron 表达式支持 5、6 或 7 个字段。

## 5. 安全默认值

| 功能 | 机制 |
|---|---|
| **MQTT 传输** | `mqtts://` + `use_tls = true` 实现 TLS 传输 |
| **Webhook 认证** | 配对 bearer 令牌（默认需要），可选共享密钥头 |
| **速率限制** | webhook 路由的单客户端限制（`webhook_rate_limit_per_minute`，默认 `60`） |
| **幂等性** | 基于头的重复数据删除（`X-Idempotency-Key`，默认 TTL `300s`） |
| **Cron 验证** | 无效的 cron 表达式在解析/缓存构建期间失败关闭 |

## 6. 故障排除

| 症状 | 可能原因 | 修复 |
|---|---|---|
| **MQTT** 连接错误 | broker URL/TLS 不匹配 | 验证 scheme + TLS 标志配对（`mqtt://`/`false`、`mqtts://`/`true`） |
| **Webhook** `401 Unauthorized` | 缺少 bearer 或无效密钥 | 重新配对令牌（`POST /pair`）并验证 `X-Webhook-Secret`（如果配置） |
| **`/sop/*` 返回 404** | 触发器路径不匹配 | 确保 `SOP.toml` 使用精确路径（例如 `/sop/deploy`） |
| **SOP 已启动但步骤未执行** | 无活动代理循环的无头触发器 | 运行代理循环执行 `ExecuteStep`，或设计运行在审批点暂停 |
| **Cron 未触发** | 守护进程未运行或表达式无效 | 运行 `zeroclaw daemon`；检查日志中的 cron 解析警告 |
