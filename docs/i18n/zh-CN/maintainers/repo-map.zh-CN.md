# ZeroClaw 仓库地图

ZeroClaw 是一个以 Rust 为优先开发语言的自主代理运行时。它从消息平台接收消息，经由 LLM 路由，执行工具调用，持久化内存，并返回响应。它还可以控制硬件外设并作为长期运行的守护进程。

## 运行时流程

```
用户消息 (Telegram/Discord/Slack/...)
        │
        ▼
   ┌─────────┐     ┌────────────┐
   │ 渠道(Channel) │────▶│   代理(Agent)    │  (src/agent/)
   └─────────┘     │  循环(Loop)      │
                   │            │◀──── 内存加载器（加载相关上下文）
                   │            │◀──── 系统提示词构建器
                   │            │◀──── 查询分类器（模型路由）
                   └─────┬──────┘
                         │
                         ▼
                   ┌───────────┐
                   │  提供商(Provider)  │  (LLM: Anthropic, OpenAI, Gemini, 等)
                   └─────┬─────┘
                         │
                    是否为工具调用？
                    ┌────┴────┐
                    ▼         ▼
               ┌────────┐  文本响应
               │  工具(Tools)  │     │
               └────┬───┘     │
                    │         │
                    ▼         ▼
              将结果反馈     通过渠道发送
              给 LLM         返回响应
```

---

## 顶层布局

```
zeroclaw/
├── src/                  # Rust 源代码（运行时核心）
├── crates/robot-kit/     # 硬件机器人套件的独立 crate
├── tests/                # 集成/端到端测试
├── benches/              # 基准测试（代理循环）
├── docs/contributing/extension-examples.md  # 扩展示例（自定义提供商/渠道/工具/内存）
├── firmware/             # Arduino、ESP32、Nucleo 开发板的嵌入式固件
├── web/                  # Web UI（Vite + TypeScript）
├── dev/                  # 本地开发工具（Docker、CI 脚本、沙箱）
├── scripts/              # CI 脚本、发布自动化、引导脚本
├── docs/                 # 文档系统（多语言、运行时参考）
├── .github/              # CI 工作流、PR 模板、自动化
├── playground/           # （空，实验性临时空间）
├── Cargo.toml            # 工作区清单
├── Dockerfile            # 容器构建文件
├── docker-compose.yml    # 服务编排
├── flake.nix             # Nix 开发环境
└── install.sh            # 一键安装脚本
```

---

## src/ — 模块详解

### 入口点

| 文件 | 行数 | 角色 |
|---|---|---|
| `main.rs` | 1,977 | CLI 入口点。Clap 解析器，命令分发。所有 `zeroclaw <子命令>` 路由都在此处。 |
| `lib.rs` | 436 | 模块声明、可见性（`pub` 与 `pub(crate)`）、库和二进制文件之间共享的 CLI 命令枚举（`ServiceCommands`、`ChannelCommands`、`SkillCommands` 等）。 |

### 核心运行时

| 模块 | 关键文件 | 角色 |
|---|---|---|
| `agent/` | `agent.rs`、`loop_.rs` (5.6k)、`dispatcher.rs`、`prompt.rs`、`classifier.rs`、`memory_loader.rs` | **大脑。** `AgentBuilder` 组合提供商+工具+内存+观察者。`loop_.rs` 运行多轮工具调用循环。分发器处理原生与 XML 工具调用解析。分类器将查询路由到不同模型。 |
| `config/` | `schema.rs` (7.6k)、`mod.rs`、`traits.rs` | **所有配置结构体。** 每个子系统的配置都位于 `schema.rs` 中 —— 提供商、渠道、内存、安全、网关、工具、硬件、调度等。从 TOML 文件加载。 |
| `runtime/` | `native.rs`、`docker.rs`、`wasm.rs`、`traits.rs` | **平台适配器。** `RuntimeAdapter` 特征抽象了 shell 访问、文件系统、存储路径、内存预算。原生模式 = 直接访问操作系统。Docker 模式 = 容器隔离。WASM 模式 = 实验性支持。 |

### LLM 提供商

| 模块 | 关键文件 | 角色 |
|---|---|---|
| `providers/` | `traits.rs`、`mod.rs` (2.9k)、`reliable.rs`、`router.rs` + 11 个提供商文件 | **LLM 集成。** `Provider` 特征：`chat()`、`chat_with_system()`、`capabilities()`、`convert_tools()`。`mod.rs` 中的工厂函数根据名称创建提供商实例。`ReliableProvider` 为任意提供商包装了重试/回退链。`RoutedProvider` 根据分类器提示进行路由。 |

提供商：`anthropic`、`openai`、`openai_codex`、`openrouter`、`gemini`、`ollama`、`compatible`（OpenAI 兼容）、`copilot`、`bedrock`、`telnyx`、`glm`

### 消息渠道

| 模块 | 关键文件 | 角色 |
|---|---|---|
| `channels/` | `traits.rs`、`mod.rs` (6.6k) + 22 个渠道文件 | **输入/输出传输层。** `Channel` 特征：`send()`、`listen()`、`health_check()`、`start_typing()`、草稿更新。`mod.rs` 中的工厂函数将配置与渠道实例关联，管理每个发送者的对话历史（最多 50 条消息）。 |

渠道：`telegram` (4.6k)、`discord`、`slack`、`whatsapp`、`whatsapp_web`、`matrix`、`signal`、`email_channel`、`qq`、`dingtalk`、`lark`、`imessage`、`irc`、`nostr`、`mattermost`、`nextcloud_talk`、`wati`、`mqtt`、`linq`、`clawdtalk`、`cli`

### 工具（代理能力）

| 模块 | 关键文件 | 角色 |
|---|---|---|
| `tools/` | `traits.rs`、`mod.rs` (635) + 38 个工具文件 | **代理可执行的操作。** `Tool` 特征：`name()`、`description()`、`parameters_schema()`、`execute()`。两个注册表：`default_tools()`（6 个基础工具）和 `all_tools_with_runtime()`（完整集合，配置门控）。 |

工具类别：
- **文件/Shell**: `shell`、`file_read`、`file_write`、`file_edit`、`glob_search`、`content_search`
- **内存**: `memory_store`、`memory_recall`、`memory_forget`
- **Web**: `browser`、`browser_open`、`web_fetch`、`web_search_tool`、`http_request`
- **调度**: `cron_add`、`cron_list`、`cron_remove`、`cron_update`、`cron_run`、`cron_runs`、`schedule`
- **委托**: `delegate`（子代理生成）、`composio`（OAuth 集成）
- **硬件**: `hardware_board_info`、`hardware_memory_map`、`hardware_memory_read`
- **SOP**: `sop_execute`、`sop_advance`、`sop_approve`、`sop_list`、`sop_status`
- **实用工具**: `git_operations`、`image_info`、`pdf_read`、`screenshot`、`pushover`、`model_routing_config`、`proxy_config`、`cli_discovery`、`schema`

### 内存

| 模块 | 关键文件 | 角色 |
|---|---|---|
| `memory/` | `traits.rs`、`backend.rs`、`mod.rs` + 8 个后端文件 | **持久化知识。** `Memory` 特征：`store()`、`recall()`、`get()`、`list()`、`forget()`、`count()`。类别：核心、日常、对话、自定义。 |

后端：`sqlite`、`markdown`、`lucid`（混合 SQLite + 向量嵌入）、`qdrant`（向量数据库）、`postgres`、`none`

支持模块：`embeddings.rs`（向量嵌入生成）、`vector.rs`（向量操作）、`chunker.rs`（文本拆分）、`hygiene.rs`（清理）、`snapshot.rs`（备份）、`response_cache.rs`（缓存）、`cli.rs`（CLI 命令）

### 安全

| 模块 | 关键文件 | 角色 |
|---|---|---|
| `security/` | `policy.rs` (2.3k)、`secrets.rs`、`pairing.rs`、`prompt_guard.rs`、`leak_detector.rs`、`audit.rs`、`otp.rs`、`estop.rs`、`domain_matcher.rs` + 4 个沙箱文件 | **策略引擎与执行。** `SecurityPolicy`：自主级别（只读/监督/完全）、工作区限制、命令白名单、禁止路径、速率限制、成本上限。 |

沙箱：`bubblewrap.rs`、`firejail.rs`、`landlock.rs`、`docker.rs`、`detect.rs`（自动检测最佳可用沙箱）

### 网关（HTTP API）

| 模块 | 关键文件 | 角色 |
|---|---|---|
| `gateway/` | `mod.rs` (2.8k)、`api.rs` (1.4k)、`sse.rs`、`ws.rs`、`static_files.rs` | **Axum HTTP 服务器。** Webhook 接收器（WhatsApp、WATI、Linq、Nextcloud Talk）、REST API、SSE 流、WebSocket 支持。速率限制、幂等键、64KB 主体限制、30 秒超时。 |

### 硬件与外设

| 模块 | 关键文件 | 角色 |
|---|---|---|
| `peripherals/` | `traits.rs`、`mod.rs`、`serial.rs`、`rpi.rs`、`arduino_flash.rs`、`uno_q_bridge.rs`、`uno_q_setup.rs`、`nucleo_flash.rs`、`capabilities_tool.rs` | **硬件开发板抽象。** `Peripheral` 特征：`connect()`、`disconnect()`、`health_check()`、`tools()`。每个外设将其能力暴露为代理可以调用的工具。 |
| `hardware/` | `discover.rs`、`introspect.rs`、`registry.rs`、`mod.rs` | **USB 发现与开发板识别。** 扫描 VID/PID，匹配已知开发板，内省连接的设备。 |

### 可观测性

| 模块 | 关键文件 | 角色 |
|---|---|---|
| `observability/` | `traits.rs`、`mod.rs`、`log.rs`、`prometheus.rs`、`otel.rs`、`verbose.rs`、`noop.rs`、`multi.rs`、`runtime_trace.rs` | **指标与追踪。** `Observer` 特征：`log_event()`。复合观察者（`multi.rs`）将事件扇出到多个后端。 |

### 技能与 SkillForge

| 模块 | 关键文件 | 角色 |
|---|---|---|
| `skills/` | `mod.rs` (1.5k)、`audit.rs` | **用户/社区创作的能力。** 从 `~/.zeroclaw/workspace/skills/<name>/SKILL.md` 加载。CLI 命令：列表、安装、审计、移除。可选从开放技能仓库同步社区内容。 |
| `skillforge/` | `scout.rs`、`evaluate.rs`、`integrate.rs`、`mod.rs` | **技能发现与评估。** 搜寻技能，评估质量/适用性，集成到运行时。 |

### SOP（标准操作流程）

| 模块 | 关键文件 | 角色 |
|---|---|---|
| `sop/` | `engine.rs` (1.6k)、`metrics.rs` (1.5k)、`types.rs`、`dispatch.rs`、`condition.rs`、`gates.rs`、`audit.rs`、`mod.rs` | **工作流引擎。** 定义包含条件、门控（审批检查点）和指标的多步骤流程。代理可以执行、推进和审计 SOP 运行。 |

### 调度与生命周期

| 模块 | 关键文件 | 角色 |
|---|---|---|
| `cron/` | `scheduler.rs`、`schedule.rs`、`store.rs`、`types.rs`、`mod.rs` | **任务调度器。** Cron 表达式、一次性定时器、固定间隔。持久化存储。 |
| `heartbeat/` | `engine.rs`、`mod.rs` | **存活监控。** 对渠道/网关的定期健康检查。 |
| `daemon/` | `mod.rs` | **长期运行守护进程。** 同时启动网关 + 渠道 + 心跳 + 调度器。 |
| `service/` | `mod.rs` (1.3k) | **操作系统服务管理。** 通过 systemd 或 launchd 安装/启动/停止/重启。 |
| `hooks/` | `mod.rs`、`runner.rs`、`traits.rs`、`builtin/` | **生命周期钩子。** 在事件发生时运行用户脚本（工具执行前/后、消息接收等）。 |

### 支持模块

| 模块 | 关键文件 | 角色 |
|---|---|---|
| `onboard/` | `wizard.rs` (7.2k)、`mod.rs` | **首次运行设置向导。** 交互式或快速模式引导：提供商、API 密钥、渠道、内存后端。 |
| `auth/` | `profiles.rs`、`anthropic_token.rs`、`gemini_oauth.rs`、`openai_oauth.rs`、`oauth_common.rs` | **认证配置文件与 OAuth 流程。** 按提供商管理凭证。 |
| `approval/` | `mod.rs` | **审批工作流。** 对风险操作进行人工审批门控。 |
| `doctor/` | `mod.rs` (1.3k) | **诊断工具。** 检查守护进程健康状态、调度器新鲜度、渠道连通性。 |
| `health/` | `mod.rs` | **健康检查端点。** |
| `cost/` | `tracker.rs`、`types.rs`、`mod.rs` | **成本追踪。** 按会话和按日成本核算。 |
| `tunnel/` | `cloudflare.rs`、`ngrok.rs`、`tailscale.rs`、`custom.rs`、`none.rs`、`mod.rs` | **隧道适配器。** 通过 Cloudflare、ngrok、Tailscale 或自定义隧道暴露网关。 |
| `rag/` | `mod.rs` | **检索增强生成（Retrieval-Augmented Generation）。** PDF 提取、分块支持。 |
| `integrations/` | `registry.rs`、`mod.rs` | **集成注册表。** 第三方集成目录。 |
| `identity.rs` | (1.5k) | **代理身份。** 代理实例的名称、描述、角色设定。 |
| `multimodal.rs` | — | **多模态支持。** 图像/视觉处理配置。 |
| `migration.rs` | — | **数据迁移。** 从 OpenClaw 工作区导入。 |
| `util.rs` | — | **共享工具函数。** |

---

## src/ 之外的目录

| 目录 | 角色 |
|---|---|
| `crates/robot-kit/` | 硬件机器人套件功能的独立 Rust crate |
| `tests/` | 集成和端到端测试（代理循环、配置持久化、渠道路由、提供商解析、Webhook 安全） |
| `benches/` | 性能基准测试（`agent_benchmarks.rs`） |
| `docs/contributing/extension-examples.md` | 自定义提供商、渠道、工具和内存后端的扩展示例 |
| `firmware/` | 嵌入式固件：`arduino/`、`esp32/`、`esp32-ui/`、`nucleo/`、`uno-q-bridge/` |
| `web/` | Web UI 前端（Vite + TypeScript） |
| `dev/` | 本地开发：Docker Compose、CI 脚本（`ci.sh`）、配置模板、沙箱配置 |
| `scripts/` | CI 辅助工具、发布自动化、引导脚本、贡献者层级计算 |
| `docs/` | 文档系统：多语言（en/zh-CN/ja/ru/fr/vi）、运行时参考、运维操作手册、安全提案 |
| `.github/` | CI 工作流、PR 模板、Issue 模板、自动化 |

---

## 依赖方向

```
main.rs ──▶ agent/ ──▶ providers/  (LLM 调用)
               │──▶ tools/      (能力执行)
               │──▶ memory/     (上下文持久化)
               │──▶ observability/ (事件日志)
               │──▶ security/   (策略执行)
               │──▶ config/     (所有配置结构体)
               │──▶ runtime/    (平台抽象)
               │
main.rs ──▶ channels/ ──▶ agent/ (消息路由)
main.rs ──▶ gateway/  ──▶ agent/ (HTTP/WS 路由)
main.rs ──▶ daemon/   ──▶ gateway/ + channels/ + cron/ + heartbeat/

具体模块向内依赖于特征/配置。
特征从不导入具体实现。
```

---

## CLI 命令树

```
zeroclaw
├── onboard [--force] [--reinit] [--channels-only]     # 首次运行设置
├── agent [-m "msg"] [-p provider]        # 启动代理循环
├── daemon [-p port]                      # 完整运行时（网关+渠道+cron+心跳）
├── gateway [-p port]                     # 仅 HTTP API 服务器
├── channel {list|start|doctor|add|remove|bind-telegram}
├── skill {list|install|audit|remove}
├── memory {list|get|stats|clear}
├── cron {list|add|add-at|add-every|once|remove|update|pause|resume}
├── peripheral {list|add|flash|flash-nucleo|setup-uno-q}
├── hardware {discover|introspect|info}
├── service {install|start|stop|restart|status|uninstall}
├── doctor                                # 诊断工具
├── status                                # 系统概览
├── estop [--level] [status|resume]       # 紧急停止
├── migrate openclaw                      # 数据迁移
├── pair                                  # 设备配对
├── auth-profiles                         # 凭证管理
├── version / completions                 # 元命令
└── config {show|edit|validate|reset}
```
