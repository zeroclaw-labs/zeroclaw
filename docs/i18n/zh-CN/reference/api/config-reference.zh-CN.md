# ZeroClaw 配置参考（面向运维人员）

本文档是常见配置部分和默认值的高信息量参考。

最后验证时间：**2026年2月21日**。

启动时的配置路径解析顺序：

1. `ZEROCLAW_WORKSPACE` 覆盖（如果设置）
2. 持久化的 `~/.zeroclaw/active_workspace.toml` 标记（如果存在）
3. 默认 `~/.zeroclaw/config.toml`

ZeroClaw 在启动时以 `INFO` 级别记录解析后的配置：

- `Config loaded` 包含字段：`path`、`workspace`、`source`、`initialized`

模式导出命令：

- `zeroclaw config schema`（将 JSON Schema 草案 2020-12 打印到 stdout）

## 核心键

| 键 | 默认值 | 说明 |
|---|---|---|
| `default_provider` | `openrouter` | 提供商 ID 或别名 |
| `default_model` | `anthropic/claude-sonnet-4-6` | 通过所选提供商路由的模型 |
| `default_temperature` | `0.7` | 模型温度 |

## `[observability]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `backend` | `none` | 可观测性后端：`none`、`noop`、`log`、`prometheus`、`otel`、`opentelemetry` 或 `otlp` |
| `otel_endpoint` | `http://localhost:4318` | 当后端为 `otel` 时使用的 OTLP HTTP 端点 |
| `otel_service_name` | `zeroclaw` | 发送到 OTLP 收集器的服务名称 |
| `runtime_trace_mode` | `none` | 运行时跟踪存储模式：`none`、`rolling` 或 `full` |
| `runtime_trace_path` | `state/runtime-trace.jsonl` | 运行时跟踪 JSONL 路径（除非绝对路径，否则相对于工作区） |
| `runtime_trace_max_entries` | `200` | 当 `runtime_trace_mode = \"rolling\"` 时保留的最大事件数 |

注意事项：

- `backend = \"otel\"` 使用带有阻塞导出器客户端的 OTLP HTTP 导出，因此可以从非 Tokio 上下文安全地发送跨度和指标。
- 别名值 `opentelemetry` 和 `otlp` 映射到同一个 OTel 后端。
- 运行时跟踪旨在调试工具调用失败和格式错误的模型工具负载。它们可能包含模型输出文本，因此在共享主机上默认保持禁用。
- 查询运行时跟踪：
  - `zeroclaw doctor traces --limit 20`
  - `zeroclaw doctor traces --event tool_call_result --contains \"error\"`
  - `zeroclaw doctor traces --id <trace-id>`

示例：

```toml
[observability]
backend = \"otel\"
otel_endpoint = \"http://localhost:4318\"
otel_service_name = \"zeroclaw\"
runtime_trace_mode = \"rolling\"
runtime_trace_path = \"state/runtime-trace.jsonl\"
runtime_trace_max_entries = 200
```

## 环境提供商覆盖

提供商选择也可以通过环境变量控制。优先级为：

1. `ZEROCLAW_PROVIDER`（显式覆盖，非空时始终优先）
2. `PROVIDER`（旧版回退，仅当配置提供商未设置或仍为 `openrouter` 时应用）
3. `config.toml` 中的 `default_provider`

容器用户操作说明：

- 如果你的 `config.toml` 设置了显式自定义提供商，如 `custom:https://.../v1`，则 Docker/容器环境中的默认 `PROVIDER=openrouter` 将不再替换它。
- 当你有意让运行时环境覆盖非默认配置的提供商时，请使用 `ZEROCLAW_PROVIDER`。

## `[agent]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `compact_context` | `true` | 为 true 时：bootstrap_max_chars=6000，rag_chunk_limit=2。适用于 13B 或更小的模型 |
| `max_tool_iterations` | `10` | 跨 CLI、网关和渠道的每条用户消息的最大工具调用循环轮次 |
| `max_history_messages` | `50` | 每个会话保留的最大对话历史消息数 |
| `parallel_tools` | `false` | 在单次迭代中启用并行工具执行 |
| `tool_dispatcher` | `auto` | 工具调度策略 |
| `tool_call_dedup_exempt` | `[]` | 免除轮次内重复调用抑制的工具名称 |

注意事项：

- 设置 `max_tool_iterations = 0` 会回退到安全默认值 `10`。
- 如果渠道消息超过此值，运行时返回：`Agent exceeded maximum tool iterations (<value>)`。
- 在 CLI、网关和渠道工具循环中，当待处理调用不需要审批门控时，多个独立工具调用默认会并发执行；结果顺序保持稳定。
- `parallel_tools` 适用于 `Agent::turn()` API 表面。它不控制 CLI、网关或渠道处理程序使用的运行时循环。
- `tool_call_dedup_exempt` 接受精确工具名称数组。此处列出的工具允许在同一轮次中使用相同参数多次调用，绕过重复数据删除检查。示例：`tool_call_dedup_exempt = [\"browser\"]`。

## `[security.otp]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `enabled` | `false` | 为敏感操作/域启用 OTP 门控 |
| `method` | `totp` | OTP 方法（`totp`、`pairing`、`cli-prompt`） |
| `token_ttl_secs` | `30` | TOTP 时间步长窗口（秒） |
| `cache_valid_secs` | `300` | 最近验证的 OTP 代码的缓存窗口 |
| `gated_actions` | `[\"shell\",\"file_write\",\"browser_open\",\"browser\",\"memory_forget\"]` | 受 OTP 保护的工具操作 |
| `gated_domains` | `[]` | 需要 OTP 的显式域模式（`*.example.com`、`login.example.com`） |
| `gated_domain_categories` | `[]` | 域预设类别（`banking`、`medical`、`government`、`identity_providers`） |

注意事项：

- 域模式支持通配符 `*`。
- 类别预设在验证期间扩展为精选的域集。
- 无效的域 glob 或未知类别在启动时快速失败。
- 当 `enabled = true` 且不存在 OTP 密钥时，ZeroClaw 会生成一个并打印一次注册 URI。

示例：

```toml
[security.otp]
enabled = true
method = \"totp\"
token_ttl_secs = 30
cache_valid_secs = 300
gated_actions = [\"shell\", \"browser_open\"]
gated_domains = [\"*.chase.com\", \"accounts.google.com\"]
gated_domain_categories = [\"banking\"]
```

## `[security.estop]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `enabled` | `false` | 启用紧急停止状态机和 CLI |
| `state_file` | `~/.zeroclaw/estop-state.json` | 持久化 estop 状态路径 |
| `require_otp_to_resume` | `true` | 恢复操作前需要 OTP 验证 |

注意事项：

- Estop 状态被原子持久化并在启动时重新加载。
- 损坏/不可读的 estop 状态回退到故障关闭 `kill_all`。
- 使用 CLI 命令 `zeroclaw estop` 启动，`zeroclaw estop resume` 清除级别。

## `[agents.<name>]`

委托子代理配置。`[agents]` 下的每个键定义一个主代理可以委托的命名子代理。

| 键 | 默认值 | 用途 |
|---|---|---|
| `provider` | _必填_ | 提供商名称（例如 `"ollama"`、`"openrouter"`、`"anthropic"`） |
| `model` | _必填_ | 子代理的模型名称 |
| `system_prompt` | 未设置 | 子代理的可选系统提示覆盖 |
| `api_key` | 未设置 | 可选 API 密钥覆盖（当 `secrets.encrypt = true` 时加密存储） |
| `temperature` | 未设置 | 子代理的温度覆盖 |
| `max_depth` | `3` | 嵌套委托的最大递归深度 |
| `agentic` | `false` | 为子代理启用多轮工具调用循环模式 |
| `allowed_tools` | `[]` | 代理模式的工具白名单 |
| `max_iterations` | `10` | 代理模式的最大工具调用迭代次数 |

注意事项：

- `agentic = false` 保留现有的单次提示→响应委托行为。
- `agentic = true` 要求 `allowed_tools` 中至少有一个匹配条目。
- `delegate` 工具从子代理白名单中排除，以防止可重入委托循环。

```toml
[agents.researcher]
provider = \"openrouter\"
model = \"anthropic/claude-sonnet-4-6\"
system_prompt = \"You are a research assistant.\"
max_depth = 2
agentic = true
allowed_tools = [\"web_search\", \"http_request\", \"file_read\"]
max_iterations = 8

[agents.coder]
provider = \"ollama\"
model = \"qwen2.5-coder:32b\"
temperature = 0.2
```

## `[runtime]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `reasoning_enabled` | 未设置（`None`） | 为支持显式控制的提供商提供全局推理/思考覆盖 |

注意事项：

- `reasoning_enabled = false` 为支持的提供商显式禁用提供商端推理（当前为 `ollama`，通过请求字段 `think: false`）。
- `reasoning_enabled = true` 为支持的提供商显式请求推理（`ollama` 上为 `think: true`）。
- 未设置时保持提供商默认值。

## `[skills]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `open_skills_enabled` | `false` | 选择加入社区 `open-skills` 仓库的加载/同步 |
| `open_skills_dir` | 未设置 | `open-skills` 的可选本地路径（启用时默认为 `$HOME/open-skills`） |
| `prompt_injection_mode` | `full` | 技能提示详细程度：`full`（内联指令/工具）或 `compact`（仅名称/描述/位置） |
| `whitelist` | `[]` | 技能名称白名单。空列表加载所有技能（默认）。非空时仅加载指定技能。适用于减少低上下文模型的系统提示大小。 |

注意事项：

- 安全优先默认：除非 `open_skills_enabled = true`，否则 ZeroClaw **不会**克隆或同步 `open-skills`。
- 环境覆盖：
  - `ZEROCLAW_OPEN_SKILLS_ENABLED` 接受 `1/0`、`true/false`、`yes/no`、`on/off`。
  - `ZEROCLAW_OPEN_SKILLS_DIR` 非空时覆盖仓库路径。
  - `ZEROCLAW_SKILLS_PROMPT_MODE` 接受 `full` 或 `compact`。
- 启用标志的优先级：`ZEROCLAW_OPEN_SKILLS_ENABLED` → `config.toml` 中的 `skills.open_skills_enabled` → 默认 `false`。
- 建议在低上下文本地模型上使用 `prompt_injection_mode = \"compact\"`，以减少启动提示大小，同时按需保留技能文件可用。
- 技能加载和 `zeroclaw skills install` 都会应用静态安全审计。包含符号链接、类脚本文件、高风险 shell  payload 片段或不安全 markdown 链接遍历的技能会被拒绝。

## `[composio]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `enabled` | `false` | 启用 Composio 托管 OAuth 工具 |
| `api_key` | 未设置 | `composio` 工具使用的 Composio API 密钥 |
| `entity_id` | `default` | 连接/执行调用时发送的默认 `user_id` |

注意事项：

- 向后兼容性：旧版 `enable = true` 被接受为 `enabled = true` 的别名。
- 如果 `enabled = false` 或缺少 `api_key`，则不会注册 `composio` 工具。
- ZeroClaw 请求 Composio v3 工具时使用 `toolkit_versions=latest`，并使用 `version=\"latest\"` 执行工具，以避免过时的默认工具版本。
- 典型流程：调用 `connect`，完成浏览器 OAuth，然后为所需工具操作运行 `execute`。
- 如果 Composio 返回缺少连接账户引用错误，请调用 `list_accounts`（可选带 `app`）并将返回的 `connected_account_id` 传递给 `execute`。

## `[cost]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `enabled` | `false` | 启用成本跟踪 |
| `daily_limit_usd` | `10.00` | 每日支出限额（美元） |
| `monthly_limit_usd` | `100.00` | 每月支出限额（美元） |
| `warn_at_percent` | `80` | 当支出达到限额的此百分比时发出警告 |
| `allow_override` | `false` | 允许请求使用 `--override` 标志超出预算 |

注意事项：

- 当 `enabled = true` 时，运行时跟踪每个请求的成本估算并强制执行每日/每月限额。
- 达到 `warn_at_percent` 阈值时，会发出警告但请求继续。
- 达到限额时，请求会被拒绝，除非 `allow_override = true` 且传递了 `--override` 标志。

## `[identity]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `format` | `openclaw` | 身份格式：`"openclaw"`（默认）或 `"aieos"` |
| `aieos_path` | 未设置 | AIEOS JSON 文件路径（相对于工作区） |
| `aieos_inline` | 未设置 | 内联 AIEOS JSON（替代文件路径） |

注意事项：

- 使用 `format = \"aieos\"` 搭配 `aieos_path` 或 `aieos_inline` 来加载 AIEOS / OpenClaw 身份文档。
- 应仅设置 `aieos_path` 或 `aieos_inline` 中的一个；`aieos_path` 优先。

## `[multimodal]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `max_images` | `4` | 每个请求接受的最大图像标记数 |
| `max_image_size_mb` | `5` | base64 编码前的单图像大小限制 |
| `allow_remote_fetch` | `false` | 允许从标记中获取 `http(s)` 图像 URL |

注意事项：

- 运行时接受用户消息中的图像标记，语法为：``[IMAGE:<source>]``。
- 支持的源：
  - 本地文件路径（例如 ``[IMAGE:/tmp/screenshot.png]``）
  - 数据 URI（例如 ``[IMAGE:data:image/png;base64,...]``）
  - 仅当 `allow_remote_fetch = true` 时支持远程 URL
- 允许的 MIME 类型：`image/png`、`image/jpeg`、`image/webp`、`image/gif`、`image/bmp`。
- 当活动提供商不支持视觉时，请求会失败并返回结构化能力错误（`capability=vision`），而不是静默丢弃图像。

## `[browser]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `enabled` | `false` | 启用 `browser_open` 工具（在系统浏览器中打开 URL 而不抓取） |
| `allowed_domains` | `[]` | `browser_open` 允许的域（精确/子域匹配，或 `"*"` 表示所有公共域） |
| `session_name` | 未设置 | 浏览器会话名称（用于代理浏览器自动化） |
| `backend` | `agent_browser` | 浏览器自动化后端：`"agent_browser"`、`"rust_native"`、`"computer_use"` 或 `"auto"` |
| `native_headless` | `true` | rust-native 后端的无头模式 |
| `native_webdriver_url` | `http://127.0.0.1:9515` | rust-native 后端的 WebDriver 端点 URL |
| `native_chrome_path` | 未设置 | rust-native 后端的可选 Chrome/Chromium 可执行文件路径 |

### `[browser.computer_use]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `endpoint` | `http://127.0.0.1:8787/v1/actions` | 计算机使用操作的 sidecar 端点（操作系统级鼠标/键盘/截图） |
| `api_key` | 未设置 | 计算机使用 sidecar 的可选 bearer 令牌（加密存储） |
| `timeout_ms` | `15000` | 每个操作的请求超时（毫秒） |
| `allow_remote_endpoint` | `false` | 允许计算机使用 sidecar 的远程/公共端点 |
| `window_allowlist` | `[]` | 转发给 sidecar 策略的可选窗口标题/进程白名单 |
| `max_coordinate_x` | 未设置 | 基于坐标的操作的可选 X 轴边界 |
| `max_coordinate_y` | 未设置 | 基于坐标的操作的可选 Y 轴边界 |

注意事项：

- 当 `backend = \"computer_use\"` 时，代理将浏览器操作委托给 `computer_use.endpoint` 处的 sidecar。
- `allow_remote_endpoint = false`（默认）拒绝任何非环回端点，以防止意外公共暴露。
- 使用 `window_allowlist` 限制 sidecar 可以交互的操作系统窗口。

## `[http_request]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `enabled` | `false` | 启用 `http_request` 工具用于 API 交互 |
| `allowed_domains` | `[]` | HTTP 请求允许的域（精确/子域匹配，或 `"*"` 表示所有公共域） |
| `max_response_size` | `1000000` | 最大响应大小（字节，默认：1 MB） |
| `timeout_secs` | `30` | 请求超时（秒） |

注意事项：

- 默认拒绝：如果 `allowed_domains` 为空，所有 HTTP 请求都会被拒绝。
- 使用精确域或子域匹配（例如 `"api.example.com"`、`"example.com"`），或 `"*"` 允许任何公共域。
- 即使配置了 `"*"`，本地/私有目标仍然被阻止。

## `[google_workspace]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `enabled` | `false` | 启用 `google_workspace` 工具 |
| `credentials_path` | 未设置 | Google 服务账号或 OAuth 凭据 JSON 的路径 |
| `default_account` | 未设置 | 传递给 `gws` 的 `--account` 默认 Google 账号 |
| `allowed_services` | （内置列表） | 代理可访问的服务：`drive`、`gmail`、`calendar`、`sheets`、`docs`、`slides`、`tasks`、`people`、`chat`、`classroom`、`forms`、`keep`、`meet`、`events` |
| `rate_limit_per_minute` | `60` | 每分钟最大 `gws` 调用次数 |
| `timeout_secs` | `30` | 每次调用超时时间（秒） |
| `audit_log` | `false` | 为每次 `gws` 调用记录 `INFO` 日志 |

### `[[google_workspace.allowed_operations]]`

非空时，仅精确匹配的调用通过。当 `service`、`resource`、`sub_resource` 和 `method` 全部一致时，条目匹配。
为空时（默认），`allowed_services` 内的所有组合均可用。

| 键 | 是否必填 | 用途 |
|---|---|---|
| `service` | 是 | 服务标识符（须匹配 `allowed_services` 中的条目） |
| `resource` | 是 | 顶层资源名称（Gmail 为 `users`，Drive 为 `files`，Calendar 为 `events`） |
| `sub_resource` | 否 | 4 段 gws 命令的子资源。Gmail 操作使用 `gws gmail users <sub_resource> <method>`，因此 Gmail 条目需填写 `sub_resource` 才能在运行时匹配。Drive、Calendar 等使用 3 段命令，省略此字段。 |
| `methods` | 是 | 该资源/子资源上允许的一个或多个方法名称 |

Gmail 所有操作使用 `gws gmail users <sub_resource> <method>` 格式。未填写 `sub_resource` 的 Gmail 条目在运行时将永远无法匹配。Drive 和 Calendar 使用 3 段命令，省略 `sub_resource`。

```toml
[google_workspace]
enabled = true
default_account = "owner@company.com"
allowed_services = ["gmail"]
audit_log = true

[[google_workspace.allowed_operations]]
service = "gmail"
resource = "users"
sub_resource = "drafts"
methods = ["list", "get", "create", "update"]
```

## `[gateway]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `host` | `127.0.0.1` | 绑定地址 |
| `port` | `42617` | 网关监听端口 |
| `require_pairing` | `true` | bearer 认证前需要配对 |
| `allow_public_bind` | `false` | 阻止意外公共暴露 |

## `[autonomy]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `level` | `supervised` | `read_only`、`supervised` 或 `full` |
| `workspace_only` | `true` | 除非显式禁用，否则拒绝绝对路径输入 |
| `allowed_commands` | _shell 执行必填_ | 可执行名称、显式可执行路径或 `"*"` 的白名单 |
| `forbidden_paths` | 内置保护列表 | 显式路径拒绝列表（默认包含系统路径 + 敏感点目录） |
| `allowed_roots` | `[]` | 规范化后允许在工作区外的额外根路径 |
| `max_actions_per_hour` | `20` | 每个策略的操作预算 |
| `max_cost_per_day_cents` | `500` | 每个策略的支出防护 |
| `require_approval_for_medium_risk` | `true` | 中等风险命令的审批门控 |
| `block_high_risk_commands` | `true` | 高风险命令的硬阻止 |
| `auto_approve` | `[]` | 始终自动批准的工具操作 |
| `always_ask` | `[]` | 始终需要批准的工具操作 |

注意事项：

- `level = \"full\"` 跳过 shell 执行的中等风险审批门控，同时仍强制执行配置的防护规则。
- 即使 `workspace_only = false`，访问工作区外也需要 `allowed_roots`。
- `allowed_roots` 支持绝对路径、`~/...` 和工作区相对路径。
- `allowed_commands` 条目可以是命令名称（例如 `"git"`）、显式可执行路径（例如 `"/usr/bin/antigravity"`）或 `"*"` 以允许任何命令名称/路径（风险门控仍然适用）。
- Shell 分隔符/运算符解析是引号感知的。引用参数内的 `;` 等字符被视为文字，而不是命令分隔符。
- 未引用的 Shell 链接/运算符仍由策略检查强制执行（`;`、`|`、`&&`、`||`、后台链接和重定向）。

```toml
[autonomy]
workspace_only = false
forbidden_paths = [\"/etc\", \"/root\", \"/proc\", \"/sys\", \"~/.ssh\", \"~/.gnupg\", \"~/.aws\"]
allowed_roots = [\"~/Desktop/projects\", \"/opt/shared-repo\"]
```

## `[memory]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `backend` | `sqlite` | `sqlite`、`lucid`、`markdown`、`none` |
| `auto_save` | `true` | 仅持久化用户声明的输入（排除助手输出） |
| `embedding_provider` | `none` | `none`、`openai` 或自定义端点 |
| `embedding_model` | `text-embedding-3-small` | 嵌入模型 ID，或 `hint:<name>` 路由 |
| `embedding_dimensions` | `1536` | 所选嵌入模型的预期向量大小 |
| `vector_weight` | `0.7` | 混合排序向量权重 |
| `keyword_weight` | `0.3` | 混合排序关键词权重 |

注意事项：

- 内存上下文注入忽略旧的 `assistant_resp*` 自动保存键，以防止旧模型生成的摘要被视为事实。

## `[[model_routes]]` 和 `[[embedding_routes]]`

使用路由提示，以便集成可以在模型 ID 演变时保持稳定的名称。

### `[[model_routes]]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `hint` | _必填_ | 任务提示名称（例如 `"reasoning"`、`"fast"`、`"code"`、`"summarize"`） |
| `provider` | _必填_ | 要路由到的提供商（必须匹配已知提供商名称） |
| `model` | _必填_ | 与该提供商一起使用的模型 |
| `api_key` | 未设置 | 此路由提供商的可选 API 密钥覆盖 |
| `context_window` | 未设置 | 此路由的可选上下文窗口大小覆盖（token 数）。适用于限制 VRAM 有限后端的上下文。 |

### `[[embedding_routes]]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `hint` | _必填_ | 路由提示名称（例如 `"semantic"`、`"archive"`、`"faq"`） |
| `provider` | _必填_ | 嵌入提供商（`"none"`、`"openai"` 或 `"custom:<url>"`） |
| `model` | _必填_ | 与该提供商一起使用的嵌入模型 |
| `dimensions` | 未设置 | 此路由的可选嵌入维度覆盖 |
| `api_key` | 未设置 | 此路由提供商的可选 API 密钥覆盖 |

```toml
[memory]
embedding_model = \"hint:semantic\"

[[model_routes]]
hint = \"reasoning\"
provider = \"openrouter\"
model = \"provider/model-id\"

[[embedding_routes]]
hint = \"semantic\"
provider = \"openai\"
model = \"text-embedding-3-small\"
dimensions = 1536
```

升级策略：

1. 保持提示稳定（`hint:reasoning`、`hint:semantic`）。
2. 仅更新路由条目中的 `model = \"...new-version...\"`。
3. 在重启/部署前使用 `zeroclaw doctor` 验证。

自然语言配置路径：

- 在正常代理聊天期间，要求助手用自然语言重新配置路由。
- 运行时可以通过工具 `model_routing_config`（默认值、场景和委托子代理）持久化这些更新，无需手动编辑 TOML。

示例请求：

- `Set conversation to provider kimi, model moonshot-v1-8k.`
- `Set coding to provider openai, model gpt-5.3-codex, and auto-route when message contains code blocks.`
- `Create a coder sub-agent using openai/gpt-5.3-codex with tools file_read,file_write,shell.`

## `[query_classification]`

自动模型提示路由 — 基于内容模式将用户消息映射到 `[[model_routes]]` 提示。

| 键 | 默认值 | 用途 |
|---|---|---|
| `enabled` | `false` | 启用自动查询分类 |
| `rules` | `[]` | 分类规则（按优先级顺序评估） |

`rules` 中的每个规则：

| 键 | 默认值 | 用途 |
|---|---|---|
| `hint` | _必填_ | 必须匹配 `[[model_routes]]` 提示值 |
| `keywords` | `[]` | 不区分大小写的子字符串匹配 |
| `patterns` | `[]` | 区分大小写的文字匹配（用于代码块、`"fn "` 等关键词） |
| `min_length` | 未设置 | 仅当消息长度 ≥ N 字符时匹配 |
| `max_length` | 未设置 | 仅当消息长度 ≤ N 字符时匹配 |
| `priority` | `0` | 优先级更高的规则先检查 |

```toml
[query_classification]
enabled = true

[[query_classification.rules]]
hint = \"reasoning\"
keywords = [\"explain\", \"analyze\", \"why\"]
min_length = 200
priority = 10

[[query_classification.rules]]
hint = \"fast\"
keywords = [\"hi\", \"hello\", \"thanks\"]
max_length = 50
priority = 5
```

## `[channels_config]`

顶级渠道选项在 `channels_config` 下配置。

| 键 | 默认值 | 用途 |
|---|---|---|
| `message_timeout_secs` | `300` | 渠道消息处理的基本超时（秒）；运行时会根据工具循环深度扩展（最多 4 倍） |
| `reply_precheck` | `false` | 在群组对话中回复前运行 LLM 回复意图预检。为 `true` 时，代理会进行轻量分类调用以决定是否回复。私聊始终绕过此检查。 |
| `max_concurrent_per_channel` | `4` | 每个渠道的最大并发 LLM 请求数。较低的值可防止后端 slot 饱和（例如 llama.cpp `--parallel 3`）。 |

示例：

- `[channels_config.telegram]`
- `[channels_config.discord]`
- `[channels_config.whatsapp]`
- `[channels_config.linq]`
- `[channels_config.nextcloud_talk]`
- `[channels_config.email]`
- `[channels_config.nostr]`

注意事项：

- 默认的 `300s` 针对设备上的 LLM（Ollama）进行了优化，这些 LLM 比云 API 慢。
- 运行时超时预算为 `message_timeout_secs * scale`，其中 `scale = min(max_tool_iterations, 4)`，最小值为 `1`。
- 这种缩放避免了第一个 LLM 轮次慢/重试但后续工具循环轮次仍需完成时的错误超时。
- 如果使用云 API（OpenAI、Anthropic 等），可以将其减少到 `60` 或更低。
- 低于 `30` 的值会被钳制到 `30`，以避免立即超时波动。
- 发生超时时，用户会收到：`⚠️ Request timed out while waiting for the model. Please try again.`
- 仅 Telegram 的中断行为由 `channels_config.telegram.interrupt_on_new_message` 控制（默认 `false`）。
  启用后，同一发送者在同一聊天中的较新消息会取消进行中的请求并保留被中断的用户上下文。
- 当 `zeroclaw channel start` 运行时，`default_provider`、`default_model`、`default_temperature`、`api_key`、`api_url` 和 `reliability.*` 的更新会在下一条入站消息时从 `config.toml` 热应用。

### `[channels_config.nostr]`

| 键 | 默认值 | 用途 |
|---|---|---|
| `private_key` | _必填_ | Nostr 私钥（十六进制或 `nsec1…` bech32）；当 `secrets.encrypt = true` 时静态加密 |
| `relays` | 见说明 | 中继 WebSocket URL 列表；默认为 `relay.damus.io`、`nos.lol`、`relay.primal.net`、`relay.snort.social` |
| `allowed_pubkeys` | `[]`（拒绝所有） | 发送者白名单（十六进制或 `npub1…`）；使用 `"*"` 允许所有发送者 |

注意事项：

- 同时支持 NIP-04（传统加密 DM）和 NIP-17（礼物包装私有消息）。回复自动镜像发送者的协议。
- `private_key` 是高价值密钥；生产环境中保持 `secrets.encrypt = true`（默认）。

详细的渠道矩阵和白名单行为请参见 [channels-reference.zh-CN.md](channels-reference.zh-CN.md)。

### `[channels_config.whatsapp]`

WhatsApp 在一个配置表下支持两个后端。

云 API 模式（Meta webhook）：

| 键 | 必填 | 用途 |
|---|---|---|
| `access_token` | 是 | Meta Cloud API bearer 令牌 |
| `phone_number_id` | 是 | Meta 电话号码 ID |
| `verify_token` | 是 | Webhook 验证令牌 |
| `app_secret` | 可选 | 启用 webhook 签名验证（`X-Hub-Signature-256`） |
| `allowed_numbers` | 推荐 | 允许的入站号码（`[]` = 拒绝所有，`"*"` = 允许所有） |

WhatsApp Web 模式（原生客户端）：

| 键 | 必填 | 用途 |
|---|---|---|
| `session_path` | 是 | 持久化 SQLite 会话路径 |
| `pair_phone` | 可选 | 配对码流程电话号码（仅数字） |
| `pair_code` | 可选 | 自定义配对码（否则自动生成） |
| `allowed_numbers` | 推荐 | 允许的入站号码（`[]` = 拒绝所有，`"*"` = 允许所有） |

注意事项：

- WhatsApp Web 需要构建标志 `whatsapp-web`。
- 如果同时存在云和 Web 字段，云模式优先以保持向后兼容性。

### `[channels_config.linq]`

用于 iMessage、RCS 和 SMS 的 Linq 合作伙伴 V3 API 集成。

| 键 | 必填 | 用途 |
|---|---|---|
| `api_token` | 是 | Linq 合作伙伴 API bearer 令牌 |
| `from_phone` | 是 | 发送电话号码（E.164 格式） |
| `signing_secret` | 可选 | 用于 HMAC-SHA256 签名验证的 Webhook 签名密钥 |
| `allowed_senders` | 推荐 | 允许的入站电话号码（`[]` = 拒绝所有，`"*"` = 允许所有） |

注意事项：

- Webhook 端点是 `POST /linq`。
- 设置时 `ZEROCLAW_LINQ_SIGNING_SECRET` 覆盖 `signing_secret`。
- 签名使用 `X-Webhook-Signature` 和 `X-Webhook-Timestamp` 头；过期时间戳（>300秒）会被拒绝。
- 完整配置示例请参见 [channels-reference.zh-CN.md](channels-reference.zh-CN.md)。

### `[channels_config.nextcloud_talk]`

原生 Nextcloud Talk 机器人集成（webhook 接收 + OCS 发送 API）。

| 键 | 必填 | 用途 |
|---|---|---|
| `base_url` | 是 | Nextcloud 基础 URL（例如 `https://cloud.example.com`） |
| `app_token` | 是 | 用于 OCS bearer 认证的机器人应用令牌 |
| `webhook_secret` | 可选 | 启用 webhook 签名验证 |
| `allowed_users` | 推荐 | 允许的 Nextcloud 参与者 ID（`[]` = 拒绝所有，`"*"` = 允许所有） |

注意事项：

- Webhook 端点是 `POST /nextcloud-talk`。
- 设置时 `ZEROCLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET` 覆盖 `webhook_secret`。
- 安装和故障排除请参见 [nextcloud-talk-setup.zh-CN.md](../../setup-guides/nextcloud-talk-setup.zh-CN.md)。

## `[hardware]`

用于物理世界访问的硬件向导配置（STM32、探针、串口）。

| 键 | 默认值 | 用途 |
|---|---|---|
| `enabled` | `false` | 是否启用硬件访问 |
| `transport` | `none` | 传输模式：`"none"`、`"native"`、`"serial"` 或 `"probe"` |
| `serial_port` | 未设置 | 串口路径（例如 `"/dev/ttyACM0"`） |
| `baud_rate` | `115200` | 串口波特率 |
| `probe_target` | 未设置 | 探针目标芯片（例如 `"STM32F401RE"`） |
| `workspace_datasheets` | `false` | 启用工作区数据手册 RAG（为 AI 引脚查找索引 PDF 原理图） |

注意事项：

- USB 串口连接使用 `transport = \"serial\"` 搭配 `serial_port`。
- 调试探针烧录（例如 ST-Link）使用 `transport = \"probe\"` 搭配 `probe_target`。
- 协议详情请参见 [hardware-peripherals-design.zh-CN.md](../../hardware/hardware-peripherals-design.zh-CN.md)。

## `[peripherals]`

更高级别的外围板配置。启用后，板卡会成为代理工具。

| 键 | 默认值 | 用途 |
|---|---|---|
| `enabled` | `false` | 启用外围支持（板卡成为代理工具） |
| `boards` | `[]` | 板卡配置 |
| `datasheet_dir` | 未设置 | 数据手册文档路径（相对于工作区）用于 RAG 检索 |

`boards` 中的每个条目：

| 键 | 默认值 | 用途 |
|---|---|---|
| `board` | _必填_ | 板卡类型：`"nucleo-f401re"`、`"rpi-gpio"`、`"esp32"` 等 |
| `transport` | `serial` | 传输：`"serial"`、`"native"`、`"websocket"` |
| `path` | 未设置 | 串口路径：`"/dev/ttyACM0"`、`"/dev/ttyUSB0"` |
| `baud` | `115200` | 串口波特率 |

```toml
[peripherals]
enabled = true
datasheet_dir = \"docs/datasheets\"

[[peripherals.boards]]
board = \"nucleo-f401re\"
transport = \"serial\"
path = \"/dev/ttyACM0\"
baud = 115200

[[peripherals.boards]]
board = \"rpi-gpio\"
transport = \"native\"
```

注意事项：

- 将按板卡命名的 `.md`/`.txt` 数据手册文件（例如 `nucleo-f401re.md`、`rpi-gpio.md`）放在 `datasheet_dir` 中用于 RAG 检索。
- 板卡协议和固件说明请参见 [hardware-peripherals-design.zh-CN.md](../../hardware/hardware-peripherals-design.zh-CN.md)。

## 安全相关默认值

- 默认拒绝的渠道白名单（`[]` 表示拒绝所有）
- 网关上默认需要配对
- 默认禁用公共绑定

## 验证命令

编辑配置后：

```bash
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
zeroclaw service restart
```

## 相关文档

- [channels-reference.zh-CN.md](channels-reference.zh-CN.md)
- [providers-reference.zh-CN.md](providers-reference.zh-CN.md)
- [operations-runbook.zh-CN.md](../../ops/operations-runbook.zh-CN.md)
- [troubleshooting.zh-CN.md](../../ops/troubleshooting.zh-CN.md)
