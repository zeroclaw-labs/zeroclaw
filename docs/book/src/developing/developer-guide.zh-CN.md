# ZeroClaw 开发者指南

> 本文档面向参与 ZeroClaw 开发的工程师，涵盖项目架构、Crate 体系、开发规范、扩展开发流程、测试体系及常用命令。
> 阅读本文档后，你应具备独立进行 Channel / Tool / Runtime / Gateway 开发的基础认知。

---

## 目录

- [1. 项目概览](#1-项目概览)
- [2. 目录结构](#2-目录结构)
- [3. 架构设计](#3-架构设计)
- [4. Crate 体系](#4-crate-体系)
- [5. Feature Flag 体系](#5-feature-flag-体系)
- [6. 构建配置](#6-构建配置)
- [7. 代码规范](#7-代码规范)
- [8. 扩展开发指南](#8-扩展开发指南)
  - [8.1 新增 Channel](#81-新增-channel)
  - [8.2 新增 Tool](#82-新增-tool)
  - [8.3 Runtime 改进](#83-runtime-改进)
  - [8.4 Gateway 增强](#84-gateway-增强)
- [9. 架构红线](#9-架构红线)
- [10. 测试体系](#10-测试体系)
- [11. CI/CD 流水线](#11-cicd-流水线)
- [12. 风险分级与 PR 规范](#12-风险分级与-pr-规范)
- [13. 常用命令速查](#13-常用命令速查)

---

## 1. 项目概览

ZeroClaw 是一个 **生产级模块化自主 Agent 运行时**，使用 Rust 构建。

**核心能力：**

- 30+ 消息平台集成（Discord、Telegram、Slack、微信、钉钉等）
- 多 LLM Provider 支持（Claude、GPT、Ollama 等）
- 可扩展的工具系统（Shell、文件、浏览器、自定义工具）
- HTTP/WebSocket 网关与 Web 仪表盘
- 硬件外设控制（USB、GPIO、I2C/SPI）
- WASM 插件系统
- SQLite + 向量嵌入的记忆系统

**技术栈：**

| 领域       | 选型                                          |
| ---------- | --------------------------------------------- |
| 语言       | Rust (Edition 2024, MSRV 1.87)                |
| 异步运行时 | `tokio` 1.50 (最小 feature set)               |
| HTTP 服务  | `axum` 0.8 + `hyper` 1.0                      |
| HTTP 客户端| `reqwest` 0.12 (rustls-tls)                   |
| CLI        | `clap` 4.5 (derive)                           |
| 序列化     | `serde` + `serde_json`                        |
| 配置       | `toml` / `toml_edit`                          |
| 加密       | `chacha20poly1305` + `ring`                   |
| 数据库     | `rusqlite` 0.37 (bundled SQLite, optional)    |
| TUI        | `ratatui` 0.30 + `crossterm` 0.29             |
| 日志       | `tracing` + `tracing-subscriber` (env-filter) |
| 锁         | `parking_lot` 0.12 (非中毒式)                 |
| 错误处理   | `anyhow` (应用层) + `thiserror` (库层)        |

**当前版本：** 0.7.0

---

## 2. 目录结构

```
zeroclaw/
├── Cargo.toml                 # Workspace 根配置（18 个成员）
├── Cargo.lock                 # 锁定的依赖版本
├── src/
│   ├── main.rs                # CLI 二进制入口
│   ├── lib.rs                 # 库模块导出
│   ├── agent/                 # Agent 生命周期与循环
│   ├── channels/              # 30+ 消息平台驱动实现
│   ├── providers/             # LLM Provider 实现
│   ├── tools/                 # 工具实现
│   ├── config/                # 配置加载与 schema
│   ├── memory/                # 记忆管理
│   ├── security/              # 安全策略
│   ├── gateway/               # 网关 (feature-gated)
│   ├── commands/              # CLI 子命令
│   ├── observability/         # 监控与可观测性
│   ├── cron/                  # 定时任务
│   ├── hooks/                 # 事件钩子
│   ├── skills/                # 技能系统
│   ├── sop/                   # 标准操作流程
│   ├── plugins/               # WASM 插件 (feature-gated)
│   ├── hardware/              # 硬件管理
│   ├── peripherals/           # 硬件外设
│   └── ...                    # 其他子模块
│
├── crates/                    # 14 个库 Crate
│   ├── zeroclaw-api/          # 核心 trait 定义
│   ├── zeroclaw-config/       # 配置 schema 与密钥管理
│   ├── zeroclaw-providers/    # LLM Provider 库
│   ├── zeroclaw-memory/       # 记忆后端
│   ├── zeroclaw-infra/        # 共享基础设施
│   ├── zeroclaw-channels/     # 消息通道库
│   ├── zeroclaw-tools/        # 工具库
│   ├── zeroclaw-runtime/      # 运行时核心
│   ├── zeroclaw-gateway/      # HTTP/WS 网关
│   ├── zeroclaw-tui/          # TUI 引导向导
│   ├── zeroclaw-plugins/      # WASM 插件系统
│   ├── zeroclaw-hardware/     # USB 发现与外设
│   ├── zeroclaw-tool-call-parser/ # LLM 工具调用解析
│   └── zeroclaw-macros/       # 过程宏
│
├── apps/tauri/                # Tauri 桌面应用
├── firmware/                  # 嵌入式固件 (ESP32, Arduino, Pico 等)
├── tests/                     # 多层级测试套件
├── benches/                   # Criterion 基准测试
├── docs/                      # 文档系统 (含 i18n)
├── dev/                       # 开发环境与 CI 脚本
├── scripts/                   # 构建与发布自动化
├── web/                       # Web 仪表盘静态资源
├── marketplace/               # 插件市场
├── fuzz/                      # Fuzzing 测试
│
├── AGENTS.md                  # 开发规范（所有 Agent 共享）
├── CLAUDE.md                  # Claude Code 专用指令
├── CONTRIBUTING.md            # 贡献指南
├── Justfile                   # Just 命令运行器
├── rustfmt.toml               # 代码格式化配置
├── clippy.toml                # Clippy lint 阈值
├── .editorconfig              # 编辑器配置
├── deny.toml                  # 依赖审计配置
└── release-plz.toml           # 发布自动化配置
```

---

## 3. 架构设计

### 微内核 + Trait 驱动

ZeroClaw 采用 **微内核架构**，所有核心能力通过 7 个扩展点 trait 定义，具体实现通过工厂模式注册。

```
                    ┌─────────────────────────┐
                    │    zeroclaw-api (trait)  │
                    │  Provider │ Channel │ Tool│
                    │  Memory │ Observer │ ...  │
                    └────────────┬────────────┘
                                 │ 依赖方向（向内）
        ┌────────────┬───────────┼───────────┬────────────┐
        ▼            ▼           ▼           ▼            ▼
   providers     channels     tools      memory       gateway
   (Claude,      (Telegram,   (Shell,    (SQLite,     (HTTP/WS
    GPT, ...)     Discord,..) File,...)   Markdown,..) REST API)
```

### 7 个核心扩展点

| Trait                | 文件位置                                            | 职责             |
| -------------------- | --------------------------------------------------- | ---------------- |
| `Provider`           | `crates/zeroclaw-api/src/provider.rs`               | LLM 后端适配     |
| `Channel`            | `crates/zeroclaw-api/src/channel.rs`                | 消息平台集成     |
| `Tool`               | `crates/zeroclaw-api/src/tool.rs`                   | Agent 工具       |
| `Memory`             | `crates/zeroclaw-api/src/memory_traits.rs`          | 记忆持久化       |
| `Observer`           | `crates/zeroclaw-api/src/observability_traits.rs`   | 可观测性         |
| `RuntimeAdapter`     | `crates/zeroclaw-api/src/runtime_traits.rs`         | 运行时适配       |
| `Peripheral`         | `crates/zeroclaw-api/src/peripherals_traits.rs`     | 硬件外设         |

### 稳定性分级

| 等级           | 含义                                       | 适用 Crate                                                    |
| -------------- | ------------------------------------------ | ------------------------------------------------------------- |
| **Stable**     | 受 breaking-change 策略保护（v1.0.0 后）   | 暂无（`zeroclaw-api` 将于 v1.0.0 升级）                       |
| **Beta**       | MINOR 版可破坏性变更，需 changelog         | `config`, `providers`, `memory`, `infra`, `tool-call-parser`, `macros` |
| **Experimental** | 无稳定性保证                             | `api`, `channels`, `tools`, `runtime`, `gateway`, `tui`, `plugins`, `hardware` |

**原则**：分级只能提升，不可降级。

---

## 4. Crate 体系

### 依赖关系图

```
zeroclaw (根 crate / CLI 二进制)
├── zeroclaw-api          ← 核心 trait，所有 crate 的契约层
├── zeroclaw-config       ← 配置 schema、密钥管理、配置合并
├── zeroclaw-providers    ← LLM Provider 实现
├── zeroclaw-memory       ← 记忆后端实现
├── zeroclaw-infra        ← 共享基础设施（防抖、会话、看门狗）
├── zeroclaw-channels     ← 消息通道实现
├── zeroclaw-tools        ← 工具实现
├── zeroclaw-runtime      ← Agent 循环、安全、Cron、SOP
├── zeroclaw-gateway      ← HTTP/WS 网关 (optional)
├── zeroclaw-tui          ← TUI 引导向导 (optional)
├── zeroclaw-plugins      ← WASM 插件系统 (optional)
├── zeroclaw-hardware     ← USB 发现与外设 (optional)
├── zeroclaw-tool-call-parser ← LLM 工具调用解析
└── zeroclaw-macros       ← 过程宏（配置字段派生）
```

### 各 Crate 职责明细

| Crate | 核心内容 |
| ----- | -------- |
| `zeroclaw-api` | `Provider`, `Channel`, `Tool`, `Memory`, `Observer`, `RuntimeAdapter`, `Peripheral` trait 定义 |
| `zeroclaw-config` | `ZeroClawConfig` schema、TOML 解析、环境变量覆盖、ChaCha20Poly1305 加密密钥 |
| `zeroclaw-providers` | Claude / GPT / Ollama 等 Provider 实现、Auth 服务、多模态处理 |
| `zeroclaw-memory` | Markdown 文件后端、SQLite 后端、Embedding 向量合并 |
| `zeroclaw-infra` | 防抖 (`debounce`)、会话管理 (`sessions`)、停滞看门狗 (`stall_watchdog`) |
| `zeroclaw-channels` | 30+ 平台驱动、会话存储、链接富化、语音转写/合成 |
| `zeroclaw-tools` | Shell / File / Memory / Browser 等工具实现 |
| `zeroclaw-runtime` | Agent 主循环、安全策略引擎、Cron 调度、SOP 引擎、技能系统、可观测性管道 |
| `zeroclaw-gateway` | Axum 路由、REST API、WebSocket、Webhook、Web 仪表盘、配对码认证 |
| `zeroclaw-tool-call-parser` | JSON / XML / GLM / MiniMax / Perl 风格工具调用解析 |
| `zeroclaw-macros` | `#[derive]` 宏用于配置字段自动派生 |
| `zeroclaw-plugins` | WASM 宿主、插件清单、签名验证 |
| `zeroclaw-hardware` | USB 设备发现、I2C/SPI/GPIO 外设驱动、串口通信 |
| `zeroclaw-tui` | Ratatui + Crossterm 实现的终端引导向导 |

---

## 5. Feature Flag 体系

项目定义了 **70+ 个 feature flag**，按类别组织：

### 核心子系统

| Feature              | 默认 | 说明                         |
| -------------------- | ---- | ---------------------------- |
| `agent-runtime`      | Yes  | 完整运行时：channels, tools, gateway, TUI, 子系统 |
| `gateway`            | No   | HTTP/WebSocket 网关服务      |
| `tui-onboarding`     | No   | TUI 引导向导                 |
| `schema-export`      | No   | 配置 JSON Schema 生成        |

### 可观测性

| Feature                    | 默认 | 说明               |
| -------------------------- | ---- | ------------------ |
| `observability-prometheus` | Yes  | Prometheus 指标    |
| `observability-otel`       | No   | OpenTelemetry 集成 |

### 消息通道（30+）

`channel-telegram`, `channel-discord`, `channel-slack`, `channel-email`, `channel-signal`, `channel-irc`, `channel-matrix`, `channel-nostr`, `channel-bluesky`, `channel-twitter`, `channel-reddit`, `channel-dingtalk`, `channel-qq`, `channel-lark`, `channel-mattermost`, `channel-wati`, `channel-wecom`, `channel-webhook`, `channel-whatsapp-cloud`, `channel-voice-call` 等。

### 平台特性

| Feature             | 说明                       |
| ------------------- | -------------------------- |
| `hardware`          | USB 设备发现               |
| `peripheral-rpi`    | Raspberry Pi GPIO          |
| `sandbox-landlock`  | Linux Landlock 沙箱        |
| `sandbox-bubblewrap`| Bubblewrap 沙箱            |
| `browser-native`    | 浏览器自动化 (Fantoccini)  |
| `plugins-wasm`      | WASM 插件系统              |
| `probe`             | CMSIS-DAP 调试             |
| `rag-pdf`           | PDF RAG 支持               |
| `webauthn`          | WebAuthn 认证              |

### CI 元特性

- `ci-all` — 启用所有 feature，用于 CI 全量测试。

### 开发提示

新增 Channel 或平台能力时，需在 `Cargo.toml` 中添加对应的 feature flag，并在 `ci-all` 中包含它。

---

## 6. 构建配置

### 构建 Profile

| Profile        | 用途         | 优化级别   | LTO   | codegen-units | 特点                   |
| -------------- | ------------ | ---------- | ----- | ------------- | ---------------------- |
| `dev`          | 日常开发     | 0          | 无    | 默认          | 增量编译               |
| `release`      | 生产部署     | z (体积)   | fat   | 1             | strip, panic=abort     |
| `release-fast` | 高性能机器   | 默认       | fat   | 8             | 16GB+ RAM 推荐         |
| `ci`           | CI 测试      | 默认       | thin  | 16            | 并行最大化             |
| `dist`         | 分发包       | z (体积)   | fat   | 1             | release 的体积变体     |

### 日常开发构建

```bash
# Debug 构建
cargo build

# Release 构建
cargo build --release --locked

# 指定 feature 构建
cargo build --features "channel-telegram,gateway"

# 全 feature 检查（CI 等效）
cargo check --features ci-all
```

---

## 7. 代码规范

### 格式化 (rustfmt.toml)

| 规则                          | 值     |
| ----------------------------- | ------ |
| `max_width`                   | 100    |
| `tab_spaces`                  | 4      |
| `hard_tabs`                   | false  |
| `use_field_init_shorthand`    | true   |
| `use_try_shorthand`           | true   |
| `reorder_imports`             | true   |
| `reorder_modules`             | true   |
| `match_arm_leading_pipes`     | Never  |

### Lint 阈值 (clippy.toml)

| 规则                            | 阈值   |
| ------------------------------- | ------ |
| `cognitive-complexity-threshold`| 30     |
| `too-many-arguments-threshold`  | 10     |
| `too-many-lines-threshold`      | 200    |
| `array-size-threshold`          | 65536  |

### 编辑器配置 (.editorconfig)

| 文件类型       | 缩进    | 最大宽度 |
| -------------- | ------- | -------- |
| Rust (*.rs)    | 4 空格  | 100      |
| Markdown       | 2 空格  | 80       |
| TOML/YAML/JSON | 2 空格  | —        |
| Python         | 4 空格  | 100      |
| Shell          | 2 空格  | —        |

### 提交规范

- 使用 **Conventional Commits** 格式：`feat(scope):`, `fix(scope):`, `chore:`, `docs:` 等
- 偏好小 PR（XS/S/M 标签）
- 每个 PR 只关注一件事，不混合 feature + refactor + infra
- 永远不提交密钥、个人数据或真实身份信息

### 命名约定

- 测试中使用中性占位符：`user_a`, `test_user`, `project_bot`, `example.com`
- 如需身份上下文，使用 ZeroClaw 域名空间：`ZeroClawAgent`, `ZeroClawOperator`, `zeroclaw_user`

---

## 8. 扩展开发指南

### 通用注册模式

所有扩展点遵循相同的接入流程：

1. **实现 trait** — 在对应 `src/*/` 目录创建实现文件
2. **注册到工厂** — 在模块的工厂函数中注册（如 `default_tools()`、Provider match arm）
3. **更新配置** — 如需配置项，更新 `src/config/schema.rs`
4. **编写测试** — 覆盖工厂接线和错误路径

### 8.1 新增 Channel

**风险等级：** 中

**核心 trait：** `Channel`（定义于 `crates/zeroclaw-api/src/channel.rs`）

**必须实现的方法：**

| 方法            | 说明                     |
| --------------- | ------------------------ |
| `name()`        | 返回通道名称             |
| `send()`        | 发送消息到平台           |
| `listen()`      | 监听平台消息（长轮询/WS）|

**可选覆盖的方法（有默认实现）：**

`health_check()`, `start_typing()`, `stop_typing()`, `send_draft()`, `update_draft()`, `finalize_draft()`, `cancel_draft()`, `add_reaction()`, `remove_reaction()`

**开发步骤：**

```
1. 在 src/channels/ 下创建 my_channel.rs
2. 实现 Channel trait
3. 在 src/channels/mod.rs 中注册
4. 在 src/config/schema.rs 的 ChannelsConfig 中添加配置
5. 在 Cargo.toml 中添加 feature flag: channel-my-channel
6. 在 ci-all feature 中包含新 flag
7. 编写测试：auth/allowlist/health 行为覆盖
8. 保持 send/listen/health_check/typing 语义与现有实现一致
```

**代码模板：**

```rust
use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::mpsc;

pub struct MyChannel {
    api_key: String,
    allowed_users: Vec<String>,
    client: reqwest::Client,
}

#[async_trait]
impl Channel for MyChannel {
    fn name(&self) -> &str {
        "my_channel"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        // 调用平台 API 发送消息
        todo!()
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        // 长轮询或 WebSocket 监听
        // 过滤 allowed_users
        // 通过 tx.send() 转发消息
        todo!()
    }

    async fn health_check(&self) -> bool {
        // 调用平台 API 验证凭据有效性
        todo!()
    }
}
```

### 8.2 新增 Tool

**风险等级：** 中

**核心 trait：** `Tool`（定义于 `crates/zeroclaw-api/src/tool.rs`）

**必须实现的方法：**

| 方法                   | 说明                           |
| ---------------------- | ------------------------------ |
| `name()`               | 工具名称（唯一标识）           |
| `description()`        | 工具描述（供 LLM 理解用途）    |
| `parameters_schema()`  | JSON Schema 参数定义           |
| `execute()`            | 执行工具逻辑，返回 `ToolResult`|

`spec()` 方法有默认实现，组合上述三者。

**开发步骤：**

```
1. 在 src/tools/ 下创建 my_tool.rs
2. 实现 Tool trait
3. 在 src/tools/mod.rs 的 default_tools() 中注册
4. 编写测试：参数校验、正常路径、错误路径
5. 严格校验和清洗所有输入（安全要求）
6. 返回结构化 ToolResult，避免在运行时路径中 panic
```

**代码模板：**

```rust
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct MyTool;

#[async_trait]
impl Tool for MyTool {
    fn name(&self) -> &str {
        "my_tool"
    }

    fn description(&self) -> &str {
        "工具的功能描述，LLM 据此决定何时调用"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "param1": { "type": "string", "description": "参数说明" }
            },
            "required": ["param1"]
        })
    }

    async fn execute(&self, args: Value) -> Result<ToolResult> {
        let param1 = args["param1"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'param1' parameter"))?;

        // 执行逻辑...

        Ok(ToolResult {
            success: true,
            output: format!("结果: {param1}"),
            error: None,
        })
    }
}
```

**共享状态工具的额外要求：**

如果工具持有长生命周期的共享状态，必须遵循 ADR-004 规范：

- 使用 `Arc<RwLock<T>>` handle 模式
- 构造时接受 handle，不创建全局/静态可变状态
- 使用 `ClientId`（由 daemon 提供）命名空间隔离 per-client 状态
- 安全敏感状态（凭据、配额）必须 per-client 隔离
- 配置变更时使缓存校验失效

### 8.3 Runtime 改进

**风险等级：** 高

**涉及 Crate：** `zeroclaw-runtime`（`crates/zeroclaw-runtime/`）

Runtime 是 Agent 的核心引擎，包含：

| 模块         | 职责                           |
| ------------ | ------------------------------ |
| Agent 循环   | 消息接收 → LLM 调用 → 工具执行 → 响应发送 |
| 安全策略     | 工具调用审批、沙箱、权限控制   |
| Cron 调度    | 定时任务管理                   |
| SOP 引擎     | 标准操作流程执行               |
| 技能系统     | 可组合的能力单元               |
| 可观测性     | Metrics、Tracing、Health       |

**高风险变更额外要求：**

```
1. PR 中包含威胁/风险说明（threat/risk notes）
2. PR 中包含回滚策略（rollback strategy）
3. 增加或更新失败模式和边界条件的测试
4. 可观测性数据不得包含敏感信息
5. 安全相关变更（security/ 目录）需额外审查
```

**开发注意事项：**

- `security/` 子目录的任何变更自动归类为高风险
- 不得静默弱化安全/访问控制约束
- 修改 Agent 循环时注意并发安全和资源泄漏
- 可观测性管道变更需确保不引入性能回归

### 8.4 Gateway 增强

**风险等级：** 高

**涉及 Crate：** `zeroclaw-gateway`（`crates/zeroclaw-gateway/`）

Gateway 基于 Axum 构建，提供：

| 组件         | 说明                           |
| ------------ | ------------------------------ |
| REST API     | Agent 管理、记忆查询、配置操作 |
| WebSocket    | 实时消息流                     |
| Webhook      | 外部平台回调接入               |
| Web 仪表盘   | 静态资源服务                   |
| 配对码认证   | 安全的客户端配对               |

**高风险变更额外要求：**

```
1. 与 Runtime 改进相同的威胁/风险/回滚要求
2. 新增 API 端点需考虑认证和授权
3. WebSocket 变更需考虑连接管理和资源限制
4. 变更访问控制边界需额外审查
```

**开发注意事项：**

- Gateway 通过 `gateway` feature flag 可选启用
- 新增路由遵循 Axum Router 模式
- API 变更视为公共契约，需考虑向后兼容
- Webhook 端点需验证请求来源签名

---

## 9. 架构红线

以下规则为项目架构的硬性约束，违反将导致 PR 被拒：

### 依赖方向

```
✅ 具体实现 → trait/config/util 层
❌ trait/config/util 层 → 具体实现
❌ 具体实现 A → 具体实现 B（跨子系统）
```

**禁止跨子系统耦合：**
- Provider 代码不得导入 Channel 内部实现
- Tool 代码不得直接修改 Gateway 策略
- Channel 代码不得引用特定 Provider 的类型

### 模块职责

| 目录             | 唯一职责       | 禁止混入               |
| ---------------- | -------------- | ----------------------- |
| `agent/`         | 编排（orchestration） | 传输、模型 I/O   |
| `channels/`      | 传输（transport）     | 编排、策略       |
| `providers/`     | 模型 I/O              | 传输、工具执行   |
| `security/`      | 策略（policy）        | 传输、模型 I/O   |
| `tools/`         | 执行（execution）     | 编排、传输       |

### 抽象原则

- **三次使用规则**：新共享抽象需至少三个实际调用者后才可引入
- **最小补丁**：不做推测性抽象，不为假设的未来需求设计
- **配置即契约**：配置键视为公共 API，需记录默认值、兼容性影响、迁移路径

### 反模式清单

| 反模式                       | 说明                                     |
| ---------------------------- | ---------------------------------------- |
| 为小便利引入重依赖           | 评估依赖的传递成本                       |
| 静默弱化安全约束             | 安全变更必须显式声明                     |
| 推测性 config/feature flag   | 只在有实际需求时添加                     |
| 格式化变更混入功能变更       | 分开提交                                 |
| 修改不相关模块               | PR 范围应精确                             |
| 绕过失败检查无说明           | 必须解释为什么跳过                       |
| 在重构中隐藏行为变更         | 行为变更必须显式声明                     |
| 测试/文档中使用真实身份信息  | 使用中性占位符                           |

---

## 10. 测试体系

### 五层测试分类

| 层级           | 测试什么                   | 外部边界         | 位置                         | 运行命令                               |
| -------------- | -------------------------- | ---------------- | ---------------------------- | -------------------------------------- |
| **Unit**       | 单个函数/结构体            | 全部 mock        | `src/**/*.rs` 内 `#[cfg(test)]` | `cargo test --lib`                     |
| **Component**  | 单个子系统                 | 子系统真实，其余 mock | `tests/component/`           | `cargo test --test component`          |
| **Integration**| 多组件协作                 | 内部真实，外部 mock  | `tests/integration/`         | `cargo test --test integration`        |
| **System**     | 全链路请求→响应            | 仅外部 API mock  | `tests/system/`              | `cargo test --test system`             |
| **Live**       | 完整栈 + 真实外部服务      | 无 mock          | `tests/live/`                | `cargo test --test live -- --ignored`  |

### 共享测试基础设施 (tests/support/)

| 模块                 | 内容                                                       |
| -------------------- | ---------------------------------------------------------- |
| `mock_provider.rs`   | `MockProvider`(FIFO), `RecordingProvider`, `TraceLlmProvider` |
| `mock_tools.rs`      | `EchoTool`, `CountingTool`, `FailingTool`, `RecordingTool` |
| `mock_channel.rs`    | `TestChannel`（捕获发送、记录 typing 事件）                |
| `helpers.rs`         | `make_memory()`, `build_agent()`, `text_response()` 等     |
| `trace.rs`           | `LlmTrace` 类型 + `LlmTrace::from_file()` 加载 fixture    |
| `assertions.rs`      | `verify_expects()` 声明式断言                              |

### JSON Trace Fixture 机制

用于替代内联 mock 的声明式对话脚本，存储在 `tests/fixtures/traces/`：

```json
{
  "model_name": "test-name",
  "turns": [
    {
      "user_input": "User message",
      "steps": [
        {
          "response": {
            "type": "text",
            "content": "LLM response",
            "input_tokens": 20,
            "output_tokens": 10
          }
        }
      ]
    }
  ],
  "expects": {
    "response_contains": ["expected text"],
    "tools_used": ["echo"],
    "max_tool_calls": 1
  }
}
```

### 新增测试决策树

```
测试单个子系统的隔离行为？  → tests/component/
测试多组件的协作？          → tests/integration/
测试完整消息流？            → tests/system/
需要真实 API 密钥？         → tests/live/ (加 #[ignore])
```

---

## 11. CI/CD 流水线

### PR 门控 (ci-run.yml)

```
┌─────────────┐    ┌──────────────────┐    ┌────────────────┐
│  rustfmt     │    │  clippy (ci-all) │    │  bench 编译    │
│  格式检查    │    │  全 feature lint │    │  验证可编译    │
└──────┬───────┘    └────────┬─────────┘    └───────┬────────┘
       │                     │                      │
       ▼                     ▼                      ▼
┌──────────────┐    ┌──────────────────┐    ┌────────────────┐
│  严格增量    │    │  cargo-nextest   │    │  多平台构建    │
│  delta lint  │    │  全测试套件     │    │  Ubuntu/macOS  │
│              │    │  (mold linker)   │    │  /Windows      │
└──────┬───────┘    └────────┬─────────┘    └───────┬────────┘
       │                     │                      │
       ▼                     ▼                      ▼
┌──────────────┐    ┌──────────────────┐
│  全 feature  │    │  文档质量检查    │
│  cargo check │    │  markdown lint   │
│              │    │  + 链接完整性    │
└──────┬───────┘    └────────┬─────────┘
       │                     │
       └─────────┬───────────┘
                 ▼
         ┌───────────────┐
         │  Gate（复合）  │
         │  全部通过才合并│
         └───────────────┘
```

**CI 环境：**
- Rust toolchain: 1.93.0 (stable)
- 缓存: Swatinem/rust-cache + Cargo registry
- Linux 链接器: mold (加速链接)

### 本地 CI 验证

```bash
# 完整流水线
./dev/ci.sh all

# 分步执行
./dev/ci.sh lint           # Clippy
./dev/ci.sh lint-delta     # 增量 lint（仅变更行）
./dev/ci.sh lint-strict    # 严格 lint
./dev/ci.sh test           # 测试
./dev/ci.sh build          # 构建
./dev/ci.sh deny           # 依赖策略检查
./dev/ci.sh audit          # 安全审计
```

### 发布流水线

```
PR 合并到 master
    │
    ├── release-beta-on-push.yml  → 自动 Beta 发布
    │
    ├── release-stable-manual.yml → 手动触发稳定版
    │
    ├── publish-crates-auto.yml   → crate 发布到 crates.io
    │
    └── 分发渠道
        ├── pub-aur.yml           → Arch Linux AUR
        ├── pub-homebrew-core.yml → Homebrew
        └── pub-scoop.yml        → Windows Scoop
```

---

## 12. 风险分级与 PR 规范

### 风险分级

| 等级     | 范围                                                                      | 验证要求         |
| -------- | ------------------------------------------------------------------------- | ---------------- |
| **低**   | 文档、chore、纯测试变更                                                   | 基础 CI          |
| **中**   | `crates/*/src/**` 行为变更（不涉及安全/边界）                             | 基础 CI + 测试   |
| **高**   | `zeroclaw-runtime/src/**`（尤其 `security/`）、`zeroclaw-gateway/src/**`、`zeroclaw-tools/src/**`、`.github/workflows/**` | CI + 威胁说明 + 回滚策略 + 额外测试 |

### PR 检查清单

```markdown
## 提交前必检项

- [ ] `cargo fmt --all -- --check` 通过
- [ ] `cargo clippy --all-targets -- -D warnings` 通过
- [ ] `cargo test` 通过
- [ ] 新增代码有对应测试
- [ ] 无密钥/个人数据泄露（`git diff --cached` 复查）
- [ ] PR 标题使用 Conventional Commits 格式
- [ ] PR 仅包含单一关注点
- [ ] 高风险变更包含威胁说明和回滚策略
```

### 分支策略

- `master` 是唯一默认分支（`main` 已永久删除）
- 从非 `master` 分支开发，PR 目标为 `master`
- 不直接推送到 `master`

### PR 模板要点

PR 描述需包含：Summary、Validation（验证方式）、Security（安全影响）、Compatibility（兼容性）、Rollback（回滚方案）、i18n（国际化影响）。

---

## 13. 常用命令速查

### 日常开发

```bash
# 构建
cargo build                              # Debug 构建
cargo build --release --locked           # Release 构建
cargo check                              # 快速类型检查
cargo check --features ci-all            # 全 feature 检查

# 格式化
cargo fmt --all                          # 格式化所有代码
cargo fmt --all -- --check               # 检查格式

# Lint
cargo clippy --all-targets -- -D warnings

# 测试
cargo test                               # 全部测试
cargo test --lib                         # 仅单元测试
cargo test --test component              # 组件测试
cargo test --test integration            # 集成测试
cargo test --test system                 # 系统测试
cargo test --test live -- --ignored      # 实时测试

# 文档
cargo doc --open                         # 生成并打开文档
```

### Just 快捷命令

```bash
just fmt          # 格式化
just lint         # Clippy
just test         # 测试
just ci           # fmt-check + lint + test
just build        # Release 构建
just clean        # 清理构建产物
just doc          # 生成文档
just audit        # 安全审计
just deny         # 依赖策略检查
```

### Git 工作流

```bash
# 启用 pre-push hook
git config core.hooksPath .githooks

# 创建功能分支
git checkout -b feat/my-feature

# 提交（Conventional Commits）
git commit -m "feat(channels): add my-channel integration"

# 推送（hook 自动执行质量门控）
git push -u origin feat/my-feature
```

### Docker 开发

```bash
dev/cli.sh up        # 启动容器
dev/cli.sh agent     # 进入 agent 容器
dev/cli.sh shell     # 进入沙箱容器
dev/cli.sh build     # 重新构建
dev/cli.sh ci        # 运行 CI
dev/cli.sh clean     # 清理卷
```

---

## 附录：关键文档索引

| 文档                                         | 内容                         |
| -------------------------------------------- | ---------------------------- |
| `AGENTS.md`                                  | 开发规范总纲                 |
| `CONTRIBUTING.md`                            | 贡献指南                     |
| `docs/contributing/change-playbooks.md`      | 各类变更的操作手册           |
| `docs/contributing/extension-examples.md`    | 扩展点的完整代码示例         |
| `docs/contributing/testing.md`               | 测试指南                     |
| `docs/contributing/pr-discipline.md`         | PR 纪律与隐私规则            |
| `docs/contributing/pr-workflow.md`           | PR 工作流详解                |
| `docs/contributing/reviewer-playbook.md`     | 代码审查指南                 |
| `docs/contributing/label-registry.md`        | GitHub 标签分类              |
| `docs/contributing/release-process.md`       | 发布流程                     |
| `docs/contributing/ci-map.md`                | CI 工作流文档                |
| `docs/architecture/adr-004-*.md`             | 工具共享状态所有权 ADR       |
| `.github/pull_request_template.md`           | PR 模板                      |
