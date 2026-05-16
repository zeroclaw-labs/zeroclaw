# 设计：通过旁路 Observer 推送 Agent 执行进度

**日期：** 2026-05-17
**状态：** 草案

## 摘要

把一次用户消息触发的 Agent 执行过程中的进度（skill/tool 调用、LLM
推理、错误、生命周期边界）实时推送回原始 channel。首个落地目标是
WuKongIM，通过自定义应用命令 `la_status_update` 投递；其他 channel
继承 `Channel::send_status_update` 的默认 no-op 实现，不受任何影响。

实现物理隔离在新 crate `zeroclaw-progress-observer` 中，便于在上游
开源版本 merge 时把整套功能作为可控的加法层叠加，最大程度减少冲突
面。共享类型（`StatusUpdate`、`StatusPhase`、`Channel::send_status_update`）
留在 `zeroclaw-api` ——trait 方法必须依赖的类型不能放进下游 crate，
否则会形成循环依赖。新 crate 不依赖 `zeroclaw-config` 也不依赖
`zeroclaw-runtime`，保持小而独立。

## 动机

目前一次较长的 Agent 执行从用户视角是个黑盒：消息发出后到最终回答
之间没有任何反馈。现有的 `ChannelNotifyObserver`
（`crates/zeroclaw-channels/src/orchestrator/mod.rs:125`）确实在
`ToolCallStart` 时会发一条 emoji 文本进 thread，但它只针对单个事件、
只支持工具启动这一类，呈现为普通聊天消息，用户依然没法知道"现在
进行到哪一步"。

我们想要一个更丰富、更结构化的进度通道：每次执行有一个唯一 ID，每个
关键事件推一条带 ID 的状态消息，与最终回答消息明确分离。WuKongIM
客户端已经支持按 `mid` 聚合渲染消息，所以最直接的路径就是在 Rust
侧加一个旁路 Observer，每个关键事件 emit 一条带 `mid` 的自定义
cmd 消息。

## 非目标

- 不引入 `Task` 一等资源、不暴露 `/api/tasks` 端点、不持久化任何
  历史状态。`execution_id` 的生命周期严格等于一次
  `process_channel_message` 调用。
- 不替换 `ChannelNotifyObserver`。两个 observer 并存，原有 emoji
  文本流完全保留、行为不变。
- 不在 WuKongIM 客户端引入任何"语义解析"。`desc` 字段必须是
  render-ready 文本，客户端逐字展示即可。
- 不做对真实 WuKongIM 服务器的端到端集成测试。
- 不在通用层做 throttle、batch、限流。如果上线后真出现刷屏问题，
  把缓解措施放进 WuKongIM channel 适配器内部，不污染通用 observer。

## 架构总览

```
runtime 配置 [progress_observer]                Channel 配置
  enabled + 6 个事件子开关                       progress_streaming
        │                                              │
        └──────────────┐                ┌──────────────┘
                       ▼                ▼
        orchestrator::process_channel_message
          1. execution_id = Uuid::new_v4()
          2. if ctx.progress_cfg.enabled：构造
             ProgressReportingObserver（toggles 从 cfg 派生）
          3. 与现有 observer 链组合（ChannelNotifyObserver 并存）
          4. 照常运行 tool-call loop
                       │
                       ▼ ObserverEvent 流
        ProgressReportingObserver（zeroclaw-progress-observer crate）
          - event_to_status(event, toggles) → Option<StatusUpdate>
          - Some(u)：tokio::spawn channel.send_status_update(...)
          - None：仅透传 inner.record_event(event)
          - 总是透传 inner.record_event(event)
                       │
                       ▼ Channel::send_status_update（默认 no-op）
        WuKongImChannel override
          - 检查 WuKongImConfig.progress_streaming → 关则 Ok(()) 静默
          - 序列化 la_status_update JSON
          - 通过新增的 send_cmd_message helper 投递
                       │
                       ▼
        WuKongIM 传输 → 客户端按 mid 聚合渲染
```

核心不变式：

- 每条 channel 消息一个 `execution_id`；不暴露在 `ObserverEvent`
  中，不持久化，不出现在任何 API 表面。
- Observer 是 per-message 实例化的，把 `Arc<dyn Channel>`、recipient、
  thread anchor 直接捕获到自身字段里，事件触发时无需全局路由表。
- 进度投递是 best-effort 且 fire-and-forget；失败永远不影响主 Agent
  loop。
- 最终回答仍由现有 `Channel::send` 投递，呈现为独立文本气泡，与
  任何带相同 `mid` 的 `la_status_update` 在客户端逻辑上无关联。
  客户端的"按 mid 聚合"只作用于 status_update 流，最终回答自成
  一气泡。

## Crate 拓扑

### 新 crate：`crates/zeroclaw-progress-observer/`

```toml
[package]
name    = "zeroclaw-progress-observer"
version = { workspace = true }
edition = { workspace = true }

[dependencies]
zeroclaw-api = { path = "../zeroclaw-api" }
tokio        = { workspace = true, features = ["rt", "sync"] }
tracing      = { workspace = true }
async-trait  = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "test-util"] }
```

新 crate 刻意不引入：

- `zeroclaw-config`：toggles 用值语义的 `ProgressEventToggles` 接收，
  config → toggles 的翻译由调用方完成。
- `zeroclaw-runtime`：`Observer` trait 的源头在 `zeroclaw-api`
  （`observability_traits.rs`），新 crate 直接依赖源头定义即可。
- 任何具体 channel crate。

公开 API：

```rust
pub struct ProgressReportingObserver { /* 私有字段 */ }
impl ProgressReportingObserver {
    pub fn new(
        execution_id: String,
        target_channel: Arc<dyn Channel>,
        recipient: String,
        thread_ts: Option<String>,
        toggles: ProgressEventToggles,
        inner: Arc<dyn Observer>,
    ) -> Self;
}
impl Observer for ProgressReportingObserver { /* ... */ }

#[derive(Clone, Debug, Default)]
pub struct ProgressEventToggles {
    pub agent_start:     bool,
    pub agent_end:       bool,
    pub tool_call_start: bool,
    pub tool_call:       bool,
    pub llm_thinking:    bool,
    pub error:           bool,
}
```

内部 helper（`event_to_status`、`event_to_desc`、`summarize_tool_args`）
均为 crate-private。

### `zeroclaw-api` 新增（`src/channel.rs`）

```rust
#[derive(Clone, Debug)]
pub struct StatusUpdate {
    pub execution_id: String,
    pub phase:        StatusPhase,
    pub name:         String,
    pub desc:         String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusPhase {
    AgentStart,
    LlmThinking,
    ToolStart,
    ToolDone { success: bool, elapsed_ms: u64 },
    Error,
    AgentEnd,
}

#[async_trait::async_trait]
pub trait Channel: /* 现有 trait bound */ {
    // ... 现有方法 ...

    /// 投递一条旁路进度更新。默认 no-op，让不 opt-in 的 channel 不
    /// 受影响。
    async fn send_status_update(
        &self,
        _recipient: &str,
        _thread_ts: Option<&str>,
        _update: StatusUpdate,
    ) -> Result<()> {
        Ok(())
    }
}
```

### `zeroclaw-config` 新增（`src/schema.rs`）

```rust
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ProgressObserverConfig {
    pub enabled:         bool,
    pub agent_start:     bool,
    pub agent_end:       bool,
    pub tool_call_start: bool,
    pub tool_call:       bool,
    pub llm_thinking:    bool,
    pub error:           bool,
}

impl Default for ProgressObserverConfig {
    fn default() -> Self {
        Self {
            enabled:         true,   // 主开关默认开
            agent_start:     false,  // 每个事件子开关默认全关
            agent_end:       false,
            tool_call_start: false,
            tool_call:       false,
            llm_thinking:    false,
            error:           false,
        }
    }
}

pub struct Config {
    // ... 现有字段 ...
    #[serde(default)]
    pub progress_observer: ProgressObserverConfig,
}
```

默认行为：observer 被挂载，但每个事件分支都早退 `None`，因此默认
情况下**不产出任何 StatusUpdate**，直到用户显式打开子开关。"挂载
但安静"的代价仅是每条 `ObserverEvent` 多走一次 `match` 分支判断，
远小于 spawn 或 IO 开销。

### `zeroclaw-channel-wukongim` 新增

Schema 字段：

```rust
pub struct WuKongImConfig {
    // ... 现有字段 ...
    #[serde(default)]
    pub progress_streaming: bool,   // channel 级 opt-in，默认 false
}
```

Channel impl：

```rust
#[async_trait::async_trait]
impl Channel for WuKongImChannel {
    async fn send_status_update(
        &self,
        recipient: &str,
        _thread_ts: Option<&str>,
        update: StatusUpdate,
    ) -> anyhow::Result<()> {
        if !self.config.progress_streaming {
            return Ok(());
        }
        let (channel_id, channel_type) = parse_recipient(recipient);
        let payload = serde_json::json!({
            "cmd":     "la_status_update",
            "content": phase_to_content(&update.phase),
            "param": {
                "mid":  update.execution_id,
                "name": update.name,
                "desc": update.desc,
            }
        });
        self.send_cmd_message(&channel_id, channel_type, payload).await
    }
}
```

新增的 `send_cmd_message` helper（实现细节见下方"实现前置任务"
一节）。

### `zeroclaw-channels/src/orchestrator/mod.rs` 接线

在 `process_channel_message` 中、当前 `ChannelNotifyObserver` 构造点
（约 `mod.rs:3400-3410`）附近：

```rust
use zeroclaw_progress_observer::{
    ProgressReportingObserver, ProgressEventToggles,
};

let execution_id = uuid::Uuid::new_v4().to_string();

let progress_observer: Option<Arc<dyn Observer>> = if ctx.progress_cfg.enabled {
    let toggles = ProgressEventToggles::from(&ctx.progress_cfg);
    Some(Arc::new(ProgressReportingObserver::new(
        execution_id.clone(),
        Arc::clone(&target_channel),
        msg.reply_target.clone(),
        followup_thread_id(&msg),
        toggles,
        Arc::clone(&ctx.observer),
    )))
} else {
    None
};

// 组合：notify_observer 套 progress_observer（如果有），再套 base。
// ChannelNotifyObserver 的行为保留不变。
```

`From<&ProgressObserverConfig> for ProgressEventToggles` 转换 impl
放在 orchestrator（或 orchestrator 旁的小适配模块），让新 crate
与 config 类型解耦。

## 实现前置任务（precursor）

### P-1：新增 `WuKongImChannel::send_cmd_message` helper

现状（已查证）：

- `send_text_message`（`channel.rs:502`）使用
  `encode_text_payload` 把文本 base64 后塞进 `SendParams.payload`
- `Channel::send` 实现（`channel.rs:533`）同样走 base64 文本路径
- 底层都是 `send_rpc("send", SendParams { payload, ... })`
- **没有**直接发送 cmd-shaped JSON payload 的现成 helper

接收侧（`channel.rs:367` 附近的 `la_init_helloworld` 解析）期望
payload 是 JSON 对象（含 `cmd`/`content`/`param` 字段），所以发送
侧需要新增：

```rust
async fn send_cmd_message(
    &self,
    channel_id: &str,
    channel_type: u8,
    cmd_payload: serde_json::Value,
) -> anyhow::Result<()>
```

**WuKongIM 服务器 payload 格式确认（待实现阶段验证）**：

`SendParams.payload` 字段当前都是 `serde_json::Value::String(base64)`。
服务器是否接受 `serde_json::Value::Object` 作为 payload 取决于
WuKongIM 协议定义。两种可能的实现路径：

- (a) **直接 JSON object** —— `payload: cmd_payload` —— 如果服务器
  允许任意结构化 payload，这是最自然的方式
- (b) **cmd JSON 整体 base64 编码** —— 把 cmd_payload 序列化为
  字符串后 base64，塞进 `payload: Value::String(base64)` —— 与文本
  消息路径对称

实现阶段第一步：对照 WuKongIM 文档或观察已知工作的客户端流量，确定
走 (a) 还是 (b)。如果文档不明确，**默认尝试 (a)**，失败再退到 (b)。
这个决策结果应当反映在最终代码注释里。

### P-2：客户端 `la_status_update` 解析约定

不在本 spec 实现范围，但必须文档化为外部依赖：

- WuKongIM 客户端 app 需要解析 `cmd == "la_status_update"` 的应用
  消息
- 客户端按 `param.mid` 分组聚合显示进度区域
- `param.desc` 字段逐字渲染，不做模板填充或翻译
- 最终回答（普通文本消息）独立显示为新气泡，不在进度区域内

## `StatusUpdate` 字段映射

单一函数 `event_to_status(event, toggles) -> Option<StatusUpdate>`
在 `zeroclaw-progress-observer/src/lib.rs` 内。每个分支由对应的
toggle 守卫；关闭的事件早退 `None`。

| `ObserverEvent`                              | `name`        | `desc`（自成话中文）                                | 子开关             |
|----------------------------------------------|---------------|-----------------------------------------------------|--------------------|
| `AgentStart { provider, model }`             | `"agent"`     | `"Agent 启动（{provider}/{model}）"`                | `agent_start`      |
| `LlmRequest { provider, model, msgs_count }` | `"llm"`       | `"正在调用大模型推理（{msgs_count} 条消息）"`       | `llm_thinking`     |
| `ToolCallStart { tool: "shell", args }`      | `"shell"`     | `"执行命令：{截断后的 command}"`                    | `tool_call_start`  |
| `ToolCallStart { tool: "web_search", .. }`   | `"web_search"`| `"搜索：{截断后的 query}"`                          | `tool_call_start`  |
| `ToolCallStart { tool: "read_file", .. }`    | `"read_file"` | `"读取文件：{path}"`                                | `tool_call_start`  |
| `ToolCallStart { tool: "http", .. }`         | `"http"`      | `"HTTP 请求：{url}"`                                | `tool_call_start`  |
| `ToolCallStart { tool, args (其他) }`        | `tool`        | `"调用工具：{tool}"`                                | `tool_call_start`  |
| `ToolCall { tool, success: true, duration }` | `tool`        | `"{tool} 执行完成（{elapsed_ms}ms）"`               | `tool_call`        |
| `ToolCall { tool, success: false }`          | `tool`        | `"{tool} 执行失败"`                                 | `tool_call`        |
| `Error { component, message }`               | `"error"`     | `"{component} 出现错误：{截断后的 message}"`        | `error`            |
| `AgentEnd { .. }`                            | `"agent"`     | `"处理完成"`                                        | `agent_end`        |

**注意：现有 `ObserverEvent::ToolCall` 不携带 `error` 字段**（只有
`tool` / `duration` / `success`），所以失败分支只能产出
`"{tool} 执行失败"`。如需更详细的错误描述，需后续给 `ToolCall`
加字段，超出本 spec 范围。

**`desc` 不变式**：必须是中文、自成话、≤120 字符、不含半截字符串
或需要前后拼接才成话的片段。客户端把 `desc` 当 render-ready 文本
处理，不解析、不模板填充、不翻译。

参数提取复用 `ChannelNotifyObserver` 的 key 优先级逻辑
（`command` → `query` → `path` → `url`，兜底为截断后的 JSON）。
提取 helper 复制一份到新 crate，而不是从 orchestrator 反向暴露
出来 —— 这样保持新 crate 自包含、`ChannelNotifyObserver` 不需要
改动。

## 启动诊断日志

`ProgressReportingObserver::new` 在所有子开关都为 false 时打印一条
一次性诊断日志：

```rust
if !toggles.any_enabled() {
    tracing::info!(
        target: "zeroclaw::progress_observer",
        "progress observer attached with all event toggles disabled; \
         configure [progress_observer] subkeys to enable"
    );
}
```

理由：默认配置就是"主开关开、子开关全关" → observer 挂载但不
产出。没有这条日志，运维或开发者会陷入"为什么没反应"的困惑。

打印发生在 observer **构造时**而非事件路径上 —— 即 per-message
最多打一条；如果觉得太频繁，可以加一个 `OnceLock` 让进程内只
打一次。本 spec 默认 per-message 一次（构造时打），实现阶段如果
日志噪声明显则改进。

## 时序示例：一次完整执行

用户发"README 里有几个 TODO"到 WuKongIM，所有子开关已打开：

| T   | 事件                                    | Observer 动作                                              |
|-----|-----------------------------------------|------------------------------------------------------------|
| T0  | `ChannelMessage` 到达                   | 生成 execution_id = `exec-9c4f...`                         |
| T1  | `AgentStart{anthropic/sonnet}`          | spawn → la_status_update {mid, name="agent", desc="Agent 启动（anthropic/sonnet）"} |
| T2  | `LlmRequest{..., msgs_count=8}`         | spawn → la_status_update {mid, name="llm", desc="正在调用大模型推理（8 条消息）"} |
| T3  | LLM 决定调用 shell                      | （无事件）                                                 |
| T4  | `ToolCallStart{shell, "grep -c TODO"}`  | spawn → la_status_update {mid, name="shell", desc="执行命令：grep -c TODO README.md"} |
| T5  | shell 完成（42 ms）                     | （事件尚未到）                                             |
| T6  | `ToolCall{shell, success=true, 42ms}`   | spawn → la_status_update {mid, name="shell", desc="shell 执行完成（42ms）"} |
| T7  | `LlmRequest{..., msgs_count=9}` （二轮）| spawn → la_status_update {mid, name="llm", desc="正在调用大模型推理（9 条消息）"} |
| T8  | LLM 返回最终回答                        | （无事件）                                                 |
| T9  | `AgentEnd`                              | spawn → la_status_update {mid, name="agent", desc="处理完成"} |
| T10 | `Channel::send(最终回答)`               | 普通文本气泡，不带 mid，与进度流独立                       |

T1-T9 的 status update 共享 `mid = exec-9c4f...`；WuKongIM 客户端
把它们聚合到一个进度区域。T10 的最终回答是独立气泡。

## 错误处理

| 失败点                                       | 行为                                                                |
|----------------------------------------------|---------------------------------------------------------------------|
| `send_status_update` 网络/超时               | `tracing::warn!`，丢弃这条 update，不重试                           |
| `send_status_update` channel opt-in 关闭     | 静默 `Ok(())`                                                       |
| `serde_json` 序列化失败                      | `tracing::warn!`，丢弃                                              |
| `tokio::spawn` 任务 panic                    | 仅 tracing 暴露，不向上传播                                         |
| Agent 主流程发出 `Error` 事件                | **额外**推一条 `phase=Error` 的 status update，不替代 orchestrator 现有错误展示路径 |
| `/stop` 中断                                 | Observer 随 turn 一起 drop；已 spawn 的任务自然完成或被取消，无特殊处理 |

刻意不做的事：重试、强保证消息到达顺序（除了客户端按 `mid` 聚合
内部排序）、过往 update 持久化。

## 并发模型

`Observer::record_event` 是同步签名。每次产出都通过：

```rust
let channel = Arc::clone(&self.target_channel);
let recip   = self.recipient.clone();
let thread  = self.thread_ts.clone();
tokio::spawn(async move {
    if let Err(e) = channel.send_status_update(&recip, thread.as_deref(), update).await {
        tracing::warn!(error = %e, "progress status_update failed");
    }
});
```

含义：

- status update 不保证按事件触发顺序到达（网络重排可能发生）。
  客户端在同一个 `mid` 内按服务器时间戳排序兜底。
- 为 `AgentEnd` 触发的 update 原则上可能晚于最终文本气泡到达。
  这是可接受的：客户端把最终回答与 `mid` 进度区独立渲染，乱序
  只会让进度区"晚一拍才安顿"。

## 测试策略

单元测试，两个位置：

`zeroclaw-progress-observer/src/lib.rs`（新 crate 的 `mod tests`）：

- `event_to_status_returns_some_only_when_toggle_enabled`
- `event_to_status_produces_self_contained_desc_for_every_phase`
- `tool_call_start_for_known_tools_extracts_args_correctly`
- `tool_call_done_uses_success_or_failure_template`
- `observer_passes_through_to_inner_for_all_events`
- `observer_spawns_send_via_mock_channel`
- `observer_logs_startup_diagnostic_when_all_toggles_disabled`

Mock `Channel` 实现放在 crate 的 `dev-dependencies` / `tests/`
目录下，记录所有 `send_status_update` 调用便于断言。

`zeroclaw-channel-wukongim/src/channel.rs`（现有 `mod tests`）：

- `send_status_update_returns_ok_when_opt_in_disabled`
- `send_status_update_serializes_la_status_update_cmd_shape`
- `send_cmd_message_payload_format_matches_protocol`

非范围：

- 对真实 WuKongIM 服务的 E2E 测试（CI 跑不动）。
- 跨 channel 矩阵测试 —— 其他 channel 继承默认 no-op，传递性覆盖。

## 后续工作（不在本次范围）

- **desc 文案 i18n**：v1 硬编码中文。当本机制在上游被接纳、或新 crate
  允许引入轻量 locale 资源时，把 desc 模板迁移到统一的 i18n 设施
  （目前 `crates/zeroclaw-runtime/locales/zh-CN/tools.ftl` 是 runtime
  内部的 Fluent 资源，新 crate 不应反向依赖 runtime）。
- **`ToolCall` 增加 error 字段**：当前 ToolCall 失败分支只能产出
  通用文案。如要给用户看到具体失败原因，需要给 `ObserverEvent::ToolCall`
  加 `error_message: Option<String>` —— 是个跨 crate 改动，单独 PR
  推动。
- **跨 channel 适配**：Telegram、Discord、Slack 等如果将来想加进度
  推送，参照 WuKongIM 的 `send_status_update` 实现各自的协议适配。
- **客户端配套**：WuKongIM client app 解析 `la_status_update` 命令
  并按 `mid` 聚合的实现，由客户端团队推进。

## 变更文件清单（预期）

新增：

- `crates/zeroclaw-progress-observer/Cargo.toml`
- `crates/zeroclaw-progress-observer/src/lib.rs`
- `crates/zeroclaw-progress-observer/tests/mock_channel.rs` *（或内联
  到 `lib.rs` 的 `mod tests`）*

修改：

- 工作区根 `Cargo.toml` —— 添加新 crate 到 members 列表
- `crates/zeroclaw-api/src/channel.rs` —— `StatusUpdate`、
  `StatusPhase`、`Channel::send_status_update` 默认方法
- `crates/zeroclaw-config/src/schema.rs` ——
  `ProgressObserverConfig`、`Config::progress_observer` 字段
- `crates/zeroclaw-channels/src/orchestrator/mod.rs` ——
  生成 `execution_id`、构造 `ProgressReportingObserver`、组合到
  observer 链、根据 `ctx.progress_cfg.enabled` 门控
- `crates/zeroclaw-channel-wukongim/src/config/mod.rs` ——
  `WuKongImConfig::progress_streaming` 字段
- `crates/zeroclaw-channel-wukongim/src/channel.rs` ——
  override `send_status_update`、新增 `send_cmd_message` helper
- 工作区 `Cargo.lock`

刻意保留不变：

- `ChannelNotifyObserver`（旧 emoji 文本路径）—— 并存
- 任何其他 channel 适配器 —— 继承默认 no-op
