# 配置参考（简体中文）

这是 Wave 1 首版本地化页面，用于查阅核心配置键、默认值与风险边界。

英文原文：

- [../../config-reference.md](../../config-reference.md)

## 适用场景

- 新环境初始化配置
- 排查配置项冲突与回退策略
- 审核安全相关配置与默认值

## 使用建议

- 配置键保持英文，避免本地化改写键名。
- 生产行为以英文原文定义为准。
- 新增配置：`observability.runtime_trace_record_http`，用于记录 LLM HTTP 请求/响应明细（`llm_http_request` / `llm_http_response`），默认值 `false`，仅在 `runtime_trace_mode` 为 `rolling` 或 `full` 时生效。Payload 会脱敏敏感字段，但 trace 文件仍属敏感数据。请求/响应/头部过大时会被截断。生产环境建议禁用，详见英文原文。

## 更新说明（2026-03-03）

- `[agent]` 新增 `allowed_tools` 与 `denied_tools`：
  - `allowed_tools` 非空时，只向主代理暴露白名单工具。
  - `denied_tools` 在白名单过滤后继续移除工具。
- 未匹配的 `allowed_tools` 项会被跳过（调试日志提示），不会导致启动失败。
- 若同时配置 `allowed_tools` 与 `denied_tools` 且最终将可执行工具全部移除，启动会快速失败并给出明确错误。
- 详细字段表与示例见英文原文 `config-reference.md` 的 `[agent]` 小节。

## `[observability]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `backend` | `none` | 可观测性后端：`none`、`noop`、`log`、`prometheus`、`otel`、`opentelemetry` 或 `otlp` |
| `otel_endpoint` | `http://localhost:4318` | OTLP HTTP 端点，当 backend 为 `otel` 时使用 |
| `otel_service_name` | `zeroclaw` | 发送到 OTLP 收集器的服务名称 |
| `runtime_trace_mode` | `none` | 运行时追踪存储模式：`none`、`rolling` 或 `full` |
| `runtime_trace_path` | `state/runtime-trace.jsonl` | 运行时追踪 JSONL 路径（相对工作空间，除非是绝对路径） |
| `runtime_trace_max_entries` | `200` | 当 `runtime_trace_mode = "rolling"` 时保留的最大事件数 |
| `runtime_trace_record_http` | `false` | 将详细的 LLM HTTP 请求/响应事件（`llm_http_request` / `llm_http_response`）记录到运行时追踪 |

备注：

- `backend = "otel"` 使用阻塞导出客户端进行 OTLP HTTP 导出，以便从非 Tokio 上下文安全发送 span 和 metric。
- 别名值 `opentelemetry` 和 `otlp` 映射到同一个 OTel 后端。
- 运行时追踪用于调试工具调用失败和格式错误的模型工具 payload。它们可能包含模型输出文本，因此在共享主机上默认保持禁用。
- `runtime_trace_record_http` 仅在 `runtime_trace_mode` 为 `rolling` 或 `full` 时生效。
  - HTTP 追踪 payload 会脱敏常见敏感字段（例如 Authorization 头部和类 token 的请求体/查询字段），但仍需将追踪文件视为敏感运营数据。
  - 对于流式请求，为了提高效率，会跳过响应体捕获；请求体仍会捕获（受大小限制约束）。
  - 请求/响应/头部值过大时会被截断。然而，带有大响应的高流量 LLM 流量仍可能显著增加内存使用和追踪文件大小。
  - 建议在生产环境中禁用 HTTP 追踪。
- 查询运行时追踪：
  - `zeroclaw doctor traces --limit 20`
  - `zeroclaw doctor traces --event tool_call_result --contains \"error\"`
  - `zeroclaw doctor traces --event llm_http_response --contains \"500\"`
  - `zeroclaw doctor traces --id <trace-id>`

示例：

```toml
[observability]
backend = "otel"
otel_endpoint = "http://localhost:4318"
otel_service_name = "zeroclaw"
runtime_trace_mode = "rolling"
runtime_trace_path = "state/runtime-trace.jsonl"
runtime_trace_max_entries = 200
runtime_trace_record_http = true
```
