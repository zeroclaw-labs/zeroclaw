# DaemonClaw 命令参考文档

本参考文档派生自当前 CLI 界面（`daemonclaw --help`）。

最后验证时间：**2026年3月26日**。

## 顶级命令

| 命令 | 用途 |
|---|---|
| `onboard` | 快速或交互式初始化工作区/配置 |
| `agent` | 运行交互式聊天或单消息模式 |
| `gateway` | 启动 webhook 和 WhatsApp HTTP 网关 |
| `acp` | 启动 ACP（Agent Control Protocol）stdio 服务器 |
| `daemon` | 启动受监管的运行时（网关 + 渠道 + 可选心跳/调度器） |
| `service` | 管理用户级操作系统服务生命周期 |
| `doctor` | 运行诊断和新鲜度检查 |
| `status` | 打印当前配置和系统摘要 |
| `estop` | 启动/恢复紧急停止级别并检查 estop 状态 |
| `cron` | 管理计划任务 |
| `models` | 刷新提供商模型目录 |
| `providers` | 列出提供商 ID、别名和活动提供商 |
| `channel` | 管理渠道和渠道健康检查 |
| `integrations` | 检查集成详情 |
| `skills` | 列出/安装/移除技能 |
| `migrate` | 从外部运行时导入（当前支持 OpenClaw） |
| `config` | 导出机器可读的配置模式 |
| `completions` | 生成 shell 补全脚本到 stdout |
| `hardware` | 发现和检查 USB 硬件 |
| `peripheral` | 配置和烧录外围设备 |

## 命令组

### `onboard`

- `daemonclaw onboard`
- `daemonclaw onboard --channels-only`
- `daemonclaw onboard --force`
- `daemonclaw onboard --reinit`
- `daemonclaw onboard --api-key <KEY> --provider <ID> --memory <sqlite|lucid|markdown|none>`
- `daemonclaw onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none>`
- `daemonclaw onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none> --force`

`onboard` 安全行为：

- 如果 `config.toml` 已存在，引导程序提供两种模式：
  - 完整引导（覆盖 `config.toml`）
  - 仅更新提供商（更新提供商/模型/API 密钥，同时保留现有渠道、隧道、内存、钩子和其他设置）
- 在非交互式环境中，现有 `config.toml` 会导致安全拒绝，除非传递 `--force`。
- 当你只需要轮换渠道令牌/白名单时，使用 `daemonclaw onboard --channels-only`。
- 使用 `daemonclaw onboard --reinit` 重新开始。这会备份现有配置目录并添加时间戳后缀，然后从头创建新配置。

### `agent`

- `daemonclaw agent`
- `daemonclaw agent -m \"Hello\"`
- `daemonclaw agent --provider <ID> --model <MODEL> --temperature <0.0-2.0>`
- `daemonclaw agent --peripheral <board:path>`

提示：

- 在交互式聊天中，你可以用自然语言要求更改路由（例如“对话使用 kimi，编码使用 gpt-5.3-codex”）；助手可以通过工具 `model_routing_config` 持久化这些设置。

### `acp`

- `daemonclaw acp`
- `daemonclaw acp --max-sessions <N>`
- `daemonclaw acp --session-timeout <SECONDS>`

启动 ACP（Agent Control Protocol）服务器，用于 IDE 和工具集成。

- 使用标准输入/输出的 JSON-RPC 2.0
- 支持方法：`initialize`、`session/new`、`session/prompt`、`session/stop`
- 实时流式传输代理推理、工具调用和内容通知
- 默认最大会话数：10
- 默认会话超时：3600 秒（1 小时）

### `gateway` / `daemon`

- `daemonclaw gateway [--host <HOST>] [--port <PORT>]`
- `daemonclaw daemon [--host <HOST>] [--port <PORT>]`

### `estop`

- `daemonclaw estop`（启动 `kill-all`）
- `daemonclaw estop --level network-kill`
- `daemonclaw estop --level domain-block --domain \"*.chase.com\" [--domain \"*.paypal.com\"]`
- `daemonclaw estop --level tool-freeze --tool shell [--tool browser]`
- `daemonclaw estop status`
- `daemonclaw estop resume`
- `daemonclaw estop resume --network`
- `daemonclaw estop resume --domain \"*.chase.com\"`
- `daemonclaw estop resume --tool shell`
- `daemonclaw estop resume --otp <123456>`

注意事项：

- `estop` 命令需要 `[security.estop].enabled = true`。
- 当 `[security.estop].require_otp_to_resume = true` 时，`resume` 需要 OTP 验证。
- 如果省略 `--otp`，OTP 提示会自动出现。

### `service`

- `daemonclaw service install`
- `daemonclaw service start`
- `daemonclaw service stop`
- `daemonclaw service restart`
- `daemonclaw service status`
- `daemonclaw service uninstall`

### `cron`

- `daemonclaw cron list`
- `daemonclaw cron add <expr> [--tz <IANA_TZ>] <command>`
- `daemonclaw cron add-at <rfc3339_timestamp> <command>`
- `daemonclaw cron add-every <every_ms> <command>`
- `daemonclaw cron once <delay> <command>`
- `daemonclaw cron remove <id>`
- `daemonclaw cron pause <id>`
- `daemonclaw cron resume <id>`

注意事项：

- 修改计划/cron 操作需要 `cron.enabled = true`。
- 用于创建计划的 Shell 命令 payload（`create` / `add` / `once`）在作业持久化前会经过安全命令策略验证。

### `models`

- `daemonclaw models refresh`
- `daemonclaw models refresh --provider <ID>`
- `daemonclaw models refresh --force`

`models refresh` 当前支持以下提供商 ID 的实时目录刷新：`openrouter`、`openai`、`anthropic`、`groq`、`mistral`、`deepseek`、`xai`、`together-ai`、`gemini`、`ollama`、`llamacpp`、`sglang`、`vllm`、`astrai`、`venice`、`fireworks`、`cohere`、`moonshot`、`glm`、`zai`、`qwen` 和 `nvidia`。

### `doctor`

- `daemonclaw doctor`
- `daemonclaw doctor models [--provider <ID>] [--use-cache]`
- `daemonclaw doctor traces [--limit <N>] [--event <TYPE>] [--contains <TEXT>]`
- `daemonclaw doctor traces --id <TRACE_ID>`

`doctor traces` 从 `observability.runtime_trace_path` 读取运行时工具/模型诊断信息。

### `channel`

- `daemonclaw channel list`
- `daemonclaw channel start`
- `daemonclaw channel doctor`
- `daemonclaw channel bind-telegram <IDENTITY>`
- `daemonclaw channel add <type> <json>`
- `daemonclaw channel remove <name>`

运行时聊天内命令（渠道服务器运行时的 Telegram/Discord）：

- `/models`
- `/models <provider>`
- `/model`
- `/model <model-id>`
- `/new`

渠道运行时还会监视 `config.toml` 并热应用以下更新：
- `default_provider`
- `default_model`
- `default_temperature`
- `api_key` / `api_url`（针对默认提供商）
- `reliability.*` 提供商重试设置

`add/remove` 当前会引导你回到托管安装/手动配置路径（尚未支持完整的声明式修改）。

### `integrations`

- `daemonclaw integrations info <name>`

### `skills`

- `daemonclaw skills list`
- `daemonclaw skills audit <source_or_name>`
- `daemonclaw skills install <source>`
- `daemonclaw skills remove <name>`

`<source>` 接受 git 远程地址（`https://...`、`http://...`、`ssh://...` 和 `git@host:owner/repo.git`）或本地文件系统路径。

`skills install` 在接受技能前始终会运行内置的静态安全审计。审计会阻止：
- 技能包内的符号链接
- 类脚本文件（`.sh`、`.bash`、`.zsh`、`.ps1`、`.bat`、`.cmd`）
- 高风险命令片段（例如管道到 Shell 的 payload）
- 逃出技能根目录、指向远程 markdown 或目标为脚本文件的 markdown 链接

在共享候选技能目录（或按名称已安装的技能）前，使用 `skills audit` 手动验证。

技能清单（`SKILL.toml`）支持 `prompts` 和 `[[tools]]`；两者都会在运行时注入到代理系统提示中，因此模型可以遵循技能指令而无需手动读取技能文件。

### `migrate`

- `daemonclaw migrate openclaw [--source <path>] [--dry-run]`

### `config`

- `daemonclaw config schema`

`config schema` 将完整 `config.toml` 契约的 JSON Schema（草案 2020-12）打印到 stdout。

### `completions`

- `daemonclaw completions bash`
- `daemonclaw completions fish`
- `daemonclaw completions zsh`
- `daemonclaw completions powershell`
- `daemonclaw completions elvish`

`completions` 设计为仅输出到 stdout，因此脚本可以直接被 source 而不会被日志/警告污染。

### `hardware`

- `daemonclaw hardware discover`
- `daemonclaw hardware introspect <path>`
- `daemonclaw hardware info [--chip <chip_name>]`

### `peripheral`

- `daemonclaw peripheral list`
- `daemonclaw peripheral add <board> <path>`
- `daemonclaw peripheral flash [--port <serial_port>]`
- `daemonclaw peripheral setup-uno-q [--host <ip_or_host>]`
- `daemonclaw peripheral flash-nucleo`

## 验证提示

要快速针对当前二进制文件验证文档：

```bash
daemonclaw --help
daemonclaw <command> --help
```
