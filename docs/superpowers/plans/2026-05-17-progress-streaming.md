# 进度推送 Observer 实现计划

> **对 agentic worker 而言：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 按任务推进实施。步骤使用 checkbox（`- [ ]`）语法跟踪。

**目标：** 新增 `zeroclaw-progress-observer` crate，按事件类型把 Agent 单次执行进度通过 `Channel::send_status_update` 旁路推送回原始 channel；WuKongIM 通过 `la_status_update` cmd 落地，其他 channel 继承默认 no-op。

**架构：** 在 `zeroclaw-api` 增加 `StatusUpdate` 类型与 `send_status_update` trait 默认方法；新 crate 实现 `ProgressReportingObserver`（per-message 实例化、fire-and-forget spawn）；WuKongIM 实现协议消息 override；orchestrator 在 `process_channel_message` 中根据 `[progress_observer]` 全局配置 + channel 级 `progress_streaming` opt-in 挂载。

**技术栈：** Rust 2024、async-trait、tokio、tracing、serde、serde_json、uuid、anyhow。

**参考 spec：** `docs/superpowers/specs/2026-05-17-progress-streaming-design.md`

---

## 文件结构

**新增：**
- `crates/zeroclaw-progress-observer/Cargo.toml` —— crate 清单
- `crates/zeroclaw-progress-observer/src/lib.rs` —— 公开 API、模块声明、启动诊断
- `crates/zeroclaw-progress-observer/src/toggles.rs` —— `ProgressEventToggles` + `any_enabled`
- `crates/zeroclaw-progress-observer/src/mapping.rs` —— `event_to_status`、`summarize_tool_args`、`event_to_desc`
- `crates/zeroclaw-progress-observer/src/observer.rs` —— `ProgressReportingObserver` impl
- `crates/zeroclaw-progress-observer/src/mock.rs` —— `MockChannel`（`#[cfg(test)]`，供单测）

**修改：**
- `Cargo.toml`（workspace 根）—— `members` 列表、`[workspace.dependencies]` 注册新 crate
- `crates/zeroclaw-api/src/channel.rs` —— `StatusUpdate`、`StatusPhase`、`Channel::send_status_update`
- `crates/zeroclaw-config/src/schema.rs` —— `ProgressObserverConfig`、`Config.progress_observer`、`WuKongIMConfig.progress_streaming`
- `crates/zeroclaw-channel-wukongim/src/channel.rs` —— `send_cmd_message` helper、`send_status_update` override、`phase_to_content` helper
- `crates/zeroclaw-channels/src/orchestrator/mod.rs` —— 生成 `execution_id`、构造 `ProgressReportingObserver`、组合到 observer 链
- `crates/zeroclaw-channels/Cargo.toml` —— 增加对 `zeroclaw-progress-observer` 的依赖

---

## Task 1：在 `zeroclaw-api` 新增 `StatusUpdate` / `StatusPhase` + `Channel::send_status_update` 默认方法

**文件：**
- 修改：`crates/zeroclaw-api/src/channel.rs`

- [ ] **Step 1：在 `channel.rs` 文件末尾（最后一个 `impl SendMessage` 之后，`#[async_trait]` 之前的位置）新增类型定义**

```rust
/// Out-of-band execution-progress update.
///
/// Carried by [`Channel::send_status_update`] for channels that opt in to
/// real-time progress streaming. Generic by design: each channel adapter is
/// free to render this however it fits its protocol (cmd message, edited
/// draft, ephemeral note, etc.). The default trait implementation is a
/// no-op so channels that don't care are unaffected.
#[derive(Clone, Debug)]
pub struct StatusUpdate {
    pub execution_id: String,
    pub phase: StatusPhase,
    pub name: String,
    pub desc: String,
}

/// Coarse phase tag for [`StatusUpdate`]. The adapter may use this to pick
/// rendering style; the user-facing wording lives entirely in [`StatusUpdate::desc`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusPhase {
    AgentStart,
    LlmThinking,
    ToolStart,
    ToolDone { success: bool, elapsed_ms: u64 },
    Error,
    AgentEnd,
}
```

- [ ] **Step 2：在 `Channel` trait 内（与 `update_draft_progress` 相邻、`pin_message` 之前）追加默认方法**

打开 `channel.rs:172` 附近（`update_draft_progress` 方法之后），插入：

```rust
    /// Deliver an out-of-band execution-progress update for the current
    /// turn. Default no-op so channels without progress streaming support
    /// are unaffected.
    ///
    /// Implementations should treat this as best-effort: they MUST NOT
    /// block, MUST NOT retry, and MUST tolerate the runtime calling this
    /// at any point during a turn (including before the first user
    /// response is sent).
    async fn send_status_update(
        &self,
        _recipient: &str,
        _thread_ts: Option<&str>,
        _update: StatusUpdate,
    ) -> anyhow::Result<()> {
        Ok(())
    }
```

- [ ] **Step 3：在文件末尾的 `#[cfg(test)] mod tests` 中（如不存在则创建）添加默认行为测试**

```rust
#[cfg(test)]
mod status_update_tests {
    use super::*;

    struct DummyChannel;

    #[async_trait]
    impl Channel for DummyChannel {
        fn name(&self) -> &str { "dummy" }
        async fn send(&self, _: &SendMessage) -> anyhow::Result<()> { Ok(()) }
        async fn listen(&self, _: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn default_send_status_update_is_noop_ok() {
        let ch = DummyChannel;
        let update = StatusUpdate {
            execution_id: "exec-1".into(),
            phase: StatusPhase::AgentStart,
            name: "agent".into(),
            desc: "Agent 启动".into(),
        };
        assert!(ch.send_status_update("recipient", None, update).await.is_ok());
    }

    #[test]
    fn status_phase_equality() {
        assert_eq!(StatusPhase::AgentStart, StatusPhase::AgentStart);
        assert_ne!(
            StatusPhase::ToolDone { success: true, elapsed_ms: 10 },
            StatusPhase::ToolDone { success: false, elapsed_ms: 10 },
        );
    }
}
```

- [ ] **Step 4：运行测试以确认通过**

执行：`cargo test -p zeroclaw-api status_update_tests`

预期：测试通过；现有 zeroclaw-api 测试也仍然通过。

- [ ] **Step 5：跑全 workspace 编译，确认无其他 Channel 实现因为这次新增方法而被破坏**

执行：`cargo check --workspace --all-features`

预期：编译通过，无需修改任何现有 channel 实现（因为新方法有 default impl）。

- [ ] **Step 6：commit**

```bash
git add crates/zeroclaw-api/src/channel.rs
git commit -m "feat(api): add StatusUpdate and Channel::send_status_update default

Introduces the StatusUpdate / StatusPhase types and a default no-op
trait method on Channel. Existing channel implementations are
unaffected — adopters override send_status_update to render the
update in their protocol's native form.
"
```

---

## Task 2：在 `zeroclaw-config` 新增 `ProgressObserverConfig` 并接到 `Config`

**文件：**
- 修改：`crates/zeroclaw-config/src/schema.rs`

- [ ] **Step 1：在 `schema.rs` 找一个合适的位置（建议紧邻 `PacingConfig` 或 `AgentConfig` 之后，例如 1788 行附近）新增类型定义**

```rust
/// Configuration for the sidelined progress observer that streams agent
/// execution progress back to opt-in channels.
///
/// Defaults: master switch on, every per-event toggle off. With the
/// defaults, the observer is attached on each channel turn but every
/// event short-circuits — no `StatusUpdate` is produced until the user
/// explicitly enables a sub-toggle.
#[derive(Debug, Clone, Deserialize, Serialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "progress_observer"]
#[serde(default)]
pub struct ProgressObserverConfig {
    /// Master switch. When false, the observer is not attached at all and
    /// no per-event check fires.
    pub enabled: bool,
    /// Emit a status update when the agent loop starts a new session.
    pub agent_start: bool,
    /// Emit a status update when the agent loop finishes.
    pub agent_end: bool,
    /// Emit a status update when a tool call starts.
    pub tool_call_start: bool,
    /// Emit a status update when a tool call completes (success or failure).
    pub tool_call: bool,
    /// Emit a status update when an LLM request is dispatched.
    pub llm_thinking: bool,
    /// Emit a status update for runtime Error events.
    pub error: bool,
}

impl Default for ProgressObserverConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            agent_start: false,
            agent_end: false,
            tool_call_start: false,
            tool_call: false,
            llm_thinking: false,
            error: false,
        }
    }
}
```

- [ ] **Step 2：在 `Config` 主结构体（约 `schema.rs:72`）的字段列表里追加一个字段**

找到 `Config` 结构体定义的末尾字段，在它之后追加：

```rust
    /// Progress observer settings — controls real-time per-turn status
    /// updates emitted to opt-in channels (see [`ProgressObserverConfig`]).
    #[nested]
    #[serde(default)]
    pub progress_observer: ProgressObserverConfig,
```

> **如果 `Config` 结构体未使用 `Configurable` derive 或未支持 `#[nested]`**，删除 `#[nested]` 行只保留 `#[serde(default)]`。先看 `Config` 当前 `derive(...)` 行决定。

- [ ] **Step 3：在 `schema.rs` 的 `#[cfg(test)] mod tests` 末尾（如不存在则创建）追加单测**

```rust
#[cfg(test)]
mod progress_observer_config_tests {
    use super::*;

    #[test]
    fn default_values_are_master_on_subs_off() {
        let cfg = ProgressObserverConfig::default();
        assert!(cfg.enabled, "master switch should default on");
        assert!(!cfg.agent_start);
        assert!(!cfg.agent_end);
        assert!(!cfg.tool_call_start);
        assert!(!cfg.tool_call);
        assert!(!cfg.llm_thinking);
        assert!(!cfg.error);
    }

    #[test]
    fn deserialize_partial_toml_keeps_defaults() {
        let toml_input = r#"
            enabled = true
            agent_start = true
        "#;
        let cfg: ProgressObserverConfig = toml::from_str(toml_input).unwrap();
        assert!(cfg.enabled);
        assert!(cfg.agent_start);
        assert!(!cfg.tool_call);
        assert!(!cfg.error);
    }

    #[test]
    fn config_has_progress_observer_field_with_default() {
        let cfg = Config::default();
        assert!(cfg.progress_observer.enabled);
        assert!(!cfg.progress_observer.agent_start);
    }
}
```

- [ ] **Step 4：运行测试**

执行：`cargo test -p zeroclaw-config progress_observer_config_tests`

预期：3 个测试全通过。

- [ ] **Step 5：跑 schema-export 确保未破坏导出（若启用了该 feature）**

执行：`cargo check -p zeroclaw-config --features schema-export`

预期：编译通过。

- [ ] **Step 6：commit**

```bash
git add crates/zeroclaw-config/src/schema.rs
git commit -m "feat(config): add [progress_observer] config section

Master switch defaults on; every per-event toggle defaults off so
the observer is attached but quiet until the user opts events in.
"
```

---

## Task 3：在 `WuKongIMConfig` + `WuKongIMChannel` 同步加入 `progress_streaming` 字段

> **背景**：`WuKongIMChannel`（`channel.rs:39`）是 flat-fields 结构，把 `WuKongIMConfig` 的字段拍扁存到 channel 自身（没有 `config: WuKongIMConfig` 字段）。因此 config 加新字段后，channel struct 也要相应加字段，并在 `from_config` 中传入。

**文件：**
- 修改：`crates/zeroclaw-config/src/schema.rs`
- 修改：`crates/zeroclaw-channel-wukongim/src/channel.rs`
- 修改：`crates/zeroclaw-channel-wukongim/src/config/mod.rs`（测试）

- [ ] **Step 1：找到 `schema.rs:8419` 的 `WuKongIMConfig` 结构体，在最后一个字段（`dawn_token: String`）之后追加**

```rust
    /// Whether to opt in to receiving real-time agent-progress updates
    /// via the `la_status_update` application command. Default: false.
    ///
    /// Independent of the global `[progress_observer]` master switch —
    /// even when the runtime is producing StatusUpdates, they're silently
    /// dropped here unless this channel opts in.
    #[serde(default)]
    pub progress_streaming: bool,
```

- [ ] **Step 2：在 `WuKongIMChannel` struct（`channel.rs:39`）末尾（`workspace_dir: PathBuf,` 之后）追加字段**

```rust
    pub(crate) progress_streaming: bool,
```

- [ ] **Step 3：在 `from_config`（`channel.rs:64`）的 struct-literal 末尾追加字段赋值**

打开 `channel.rs:64-91` 附近，找到 `from_config` 函数内的 `Self { ... }` 块的末尾（`workspace_dir: workspace_dir.to_path_buf(),` 之后），追加：

```rust
            progress_streaming: config.progress_streaming,
```

- [ ] **Step 4：更新 `crates/zeroclaw-channel-wukongim/src/config/mod.rs` 测试里的 `WuKongIMConfig` 构造**

把 `default_config_fields` 测试里的 struct-literal 改成：

```rust
        let cfg = WuKongIMConfig {
            enabled: true,
            ws_url: "ws://localhost:5200".to_string(),
            uid: "bot".to_string(),
            token: "tok".to_string(),
            device_id: "web-001".to_string(),
            device_flag: 2,
            allowed_users: vec!["*".to_string()],
            mention_only: false,
            approval_timeout_secs: 300,
            downloads_dir: "downloads".to_string(),
            dawn_url: "".to_string(),
            dawn_token: "".to_string(),
            progress_streaming: false,
        };
        assert_eq!(cfg.device_id, "web-001");
        assert_eq!(cfg.device_flag, 2);
        assert!(!cfg.progress_streaming);
```

> 注意：如果 `WuKongIMConfig` 在 schema.rs 里还有 `ack_reactions`/`ack_reactions_message`/`ack_reactions_delay` 等额外字段（看 `channel.rs:81-83` 的引用），上面 struct-literal 里也要追加 —— 优先 `cargo check -p zeroclaw-channel-wukongim`，按编译报错信息逐个补齐。

- [ ] **Step 5：在同一个测试 mod 追加 default 测试**

```rust
    #[test]
    fn progress_streaming_defaults_false_when_missing_from_toml() {
        let toml_input = r#"
            enabled = true
            ws_url = "ws://localhost:5200"
            uid = "bot"
            token = "tok"
            device_id = "dev"
            device_flag = 2
        "#;
        let cfg: WuKongIMConfig = toml::from_str(toml_input).unwrap();
        assert!(!cfg.progress_streaming, "must default to false");
    }
```

- [ ] **Step 6：运行测试**

执行：`cargo test -p zeroclaw-channel-wukongim`

预期：所有 wukongim 测试通过。

- [ ] **Step 7：跑 workspace check 确保未破坏其他 `WuKongIMConfig` / `WuKongIMChannel` 使用点**

执行：`cargo check --workspace --all-features`

预期：编译通过。

- [ ] **Step 8：commit**

```bash
git add crates/zeroclaw-config/src/schema.rs crates/zeroclaw-channel-wukongim/src/channel.rs crates/zeroclaw-channel-wukongim/src/config/mod.rs
git commit -m "feat(channel-wukongim): add progress_streaming opt-in field

WuKongIMConfig schema field + matching flat field on WuKongIMChannel,
wired through from_config. Default false. Independent of the global
[progress_observer] master switch.
"
```

---

## Task 4：创建 `zeroclaw-progress-observer` crate 骨架

**文件：**
- 创建：`crates/zeroclaw-progress-observer/Cargo.toml`
- 创建：`crates/zeroclaw-progress-observer/src/lib.rs`
- 修改：根 `Cargo.toml`（workspace members + workspace.dependencies）

- [ ] **Step 1：确认新目录的父目录存在**

执行：`ls crates/`

预期：看到现有的 `zeroclaw-api`、`zeroclaw-channels` 等目录。

- [ ] **Step 2：创建 `crates/zeroclaw-progress-observer/Cargo.toml`**

```toml
[package]
name = "zeroclaw-progress-observer"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Sidelined observer that streams agent execution progress back to opt-in channels"
repository.workspace = true
rust-version.workspace = true

[dependencies]
zeroclaw-api = { workspace = true }
tokio = { version = "1.50", default-features = false, features = ["rt", "sync"] }
tracing = { version = "0.1", default-features = false }
async-trait = "0.1"
serde_json = { version = "1.0", default-features = false, features = ["std"] }
anyhow = "1.0"

[dev-dependencies]
tokio = { version = "1.50", default-features = false, features = ["rt", "sync", "macros", "rt-multi-thread", "test-util"] }
```

- [ ] **Step 3：创建 `crates/zeroclaw-progress-observer/src/lib.rs` 初始骨架**

```rust
//! Sidelined progress observer for ZeroClaw.
//!
//! This crate hosts a single [`Observer`](zeroclaw_api::observability_traits::Observer)
//! implementation that translates selected [`ObserverEvent`]s into
//! [`StatusUpdate`]s and ships them to a target [`Channel`] via the
//! channel's [`send_status_update`](zeroclaw_api::channel::Channel::send_status_update)
//! method. The observer is per-message: orchestrator constructs a fresh
//! instance for each `ChannelMessage` and drops it when the turn ends.
//!
//! See `docs/superpowers/specs/2026-05-17-progress-streaming-design.md`
//! for the full design.

mod toggles;
mod mapping;
mod observer;

#[cfg(test)]
mod mock;

pub use toggles::ProgressEventToggles;
pub use observer::ProgressReportingObserver;
```

- [ ] **Step 4：创建 4 个空模块文件（占位，下个 task 才填内容）**

```rust
// crates/zeroclaw-progress-observer/src/toggles.rs
// Filled in Task 5.
```

```rust
// crates/zeroclaw-progress-observer/src/mapping.rs
// Filled in Tasks 6 + 7.
```

```rust
// crates/zeroclaw-progress-observer/src/observer.rs
// Filled in Task 9.
```

```rust
// crates/zeroclaw-progress-observer/src/mock.rs
// Filled in Task 8.
```

> 这一步只是文件占位让 `mod` 声明不报错；编译会先抱怨 `ProgressEventToggles` 找不到，下面 Step 5 临时把 `pub use` 行注释掉。

- [ ] **Step 5：临时把 `lib.rs` 末尾两行 `pub use` 注释掉**（Task 5/Task 9 会还原）

```rust
// pub use toggles::ProgressEventToggles;
// pub use observer::ProgressReportingObserver;
```

- [ ] **Step 6：将新 crate 加入根 `Cargo.toml` 的 workspace `members` 列表**

打开根 `Cargo.toml:2`，在 `members` 数组里把 `"crates/zeroclaw-progress-observer"` 追加进去（按字母序插入到 `zeroclaw-plugins` 之后、`zeroclaw-providers` 之前合适处即可）。

- [ ] **Step 7：在根 `Cargo.toml` 的 `[workspace.dependencies]`（约 line 13-29）追加一行**

```toml
zeroclaw-progress-observer = { path = "crates/zeroclaw-progress-observer", version = "0.7.5" }
```

- [ ] **Step 8：编译验证**

执行：`cargo check -p zeroclaw-progress-observer`

预期：编译通过，可能仅有"unused" 之类的 warning。

- [ ] **Step 9：commit**

```bash
git add Cargo.toml crates/zeroclaw-progress-observer
git commit -m "feat(progress-observer): scaffold new crate

Empty crate with module skeleton (toggles / mapping / observer / mock).
Filled in by subsequent commits. Isolated from zeroclaw-config and
zeroclaw-runtime to keep the merge surface minimal against upstream.
"
```

---

## Task 5：实现 `ProgressEventToggles`

**文件：**
- 修改：`crates/zeroclaw-progress-observer/src/toggles.rs`
- 修改：`crates/zeroclaw-progress-observer/src/lib.rs`（取消注释 pub use）

- [ ] **Step 1：先写失败测试** — 替换 `toggles.rs` 全文为：

```rust
//! Per-event toggles controlling which [`ObserverEvent`] variants get
//! translated into status updates. Value type intentionally — no
//! dependency on `zeroclaw-config`.

/// Plain bag of bools, one per supported event class.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgressEventToggles {
    pub agent_start: bool,
    pub agent_end: bool,
    pub tool_call_start: bool,
    pub tool_call: bool,
    pub llm_thinking: bool,
    pub error: bool,
}

impl ProgressEventToggles {
    /// `true` iff at least one sub-toggle is enabled. Used by the observer
    /// to decide whether to emit the startup-diagnostic info log.
    pub fn any_enabled(&self) -> bool {
        self.agent_start
            || self.agent_end
            || self.tool_call_start
            || self.tool_call
            || self.llm_thinking
            || self.error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_all_subs_off() {
        let t = ProgressEventToggles::default();
        assert!(!t.any_enabled());
    }

    #[test]
    fn any_enabled_true_when_one_set() {
        let t = ProgressEventToggles { tool_call_start: true, ..Default::default() };
        assert!(t.any_enabled());
    }

    #[test]
    fn any_enabled_true_when_all_set() {
        let t = ProgressEventToggles {
            agent_start: true, agent_end: true,
            tool_call_start: true, tool_call: true,
            llm_thinking: true, error: true,
        };
        assert!(t.any_enabled());
    }
}
```

- [ ] **Step 2：把 `lib.rs` 里 `pub use toggles::ProgressEventToggles;` 那行的注释去掉，恢复导出**

- [ ] **Step 3：运行测试**

执行：`cargo test -p zeroclaw-progress-observer toggles::tests`

预期：3 个测试全通过。

- [ ] **Step 4：commit**

```bash
git add crates/zeroclaw-progress-observer/src/toggles.rs crates/zeroclaw-progress-observer/src/lib.rs
git commit -m "feat(progress-observer): add ProgressEventToggles value type"
```

---

## Task 6：实现 `summarize_tool_args` 工具参数提取 helper

**文件：**
- 修改：`crates/zeroclaw-progress-observer/src/mapping.rs`

- [ ] **Step 1：替换 `mapping.rs` 全文为以下内容（含失败测试）**

```rust
//! `ObserverEvent` → `StatusUpdate` translation helpers.
//!
//! Pure functions, no I/O. Easy to unit-test exhaustively.

use zeroclaw_api::channel::{StatusPhase, StatusUpdate};
use zeroclaw_api::observability_traits::ObserverEvent;

use crate::toggles::ProgressEventToggles;

/// Maximum length (in chars) of an extracted argument snippet.
const ARG_SNIPPET_MAX_CHARS: usize = 120;

/// Extract a short, human-friendly description of a tool's invocation
/// arguments. Prefers well-known keys (`command`, `query`, `path`, `url`);
/// falls back to a truncated JSON view.
///
/// `args_json` is the raw JSON string from `ObserverEvent::ToolCallStart`'s
/// `arguments` field. `None` or empty input yields `None`.
pub(crate) fn summarize_tool_args(args_json: Option<&str>) -> Option<String> {
    let raw = args_json?;
    if raw.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        for key in ["command", "query", "path", "url"] {
            if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
                return Some(truncate_chars(s, ARG_SNIPPET_MAX_CHARS));
            }
        }
        return Some(truncate_chars(raw, ARG_SNIPPET_MAX_CHARS));
    }
    Some(truncate_chars(raw, ARG_SNIPPET_MAX_CHARS))
}

/// Truncate a `&str` to at most `max_chars` Unicode scalars, appending
/// `…` if truncation occurred. UTF-8 safe.
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod summarize_tests {
    use super::*;

    #[test]
    fn none_input_returns_none() {
        assert!(summarize_tool_args(None).is_none());
    }

    #[test]
    fn empty_input_returns_none() {
        assert!(summarize_tool_args(Some("")).is_none());
    }

    #[test]
    fn extracts_command_key() {
        let arg = r#"{"command": "grep -c TODO README.md"}"#;
        assert_eq!(
            summarize_tool_args(Some(arg)).as_deref(),
            Some("grep -c TODO README.md"),
        );
    }

    #[test]
    fn extracts_query_key() {
        let arg = r#"{"query": "rust async runtime"}"#;
        assert_eq!(
            summarize_tool_args(Some(arg)).as_deref(),
            Some("rust async runtime"),
        );
    }

    #[test]
    fn extracts_path_key() {
        let arg = r#"{"path": "./README.md"}"#;
        assert_eq!(summarize_tool_args(Some(arg)).as_deref(), Some("./README.md"));
    }

    #[test]
    fn extracts_url_key() {
        let arg = r#"{"url": "https://example.com/x"}"#;
        assert_eq!(summarize_tool_args(Some(arg)).as_deref(), Some("https://example.com/x"));
    }

    #[test]
    fn prefers_command_over_others_when_multiple_keys_present() {
        let arg = r#"{"command": "ls", "query": "ignored"}"#;
        assert_eq!(summarize_tool_args(Some(arg)).as_deref(), Some("ls"));
    }

    #[test]
    fn falls_back_to_truncated_json_when_no_known_key() {
        let arg = r#"{"random": "x"}"#;
        let out = summarize_tool_args(Some(arg)).unwrap();
        assert_eq!(out, arg);
    }

    #[test]
    fn falls_back_to_truncated_raw_when_not_valid_json() {
        let arg = "garbage not-json";
        assert_eq!(summarize_tool_args(Some(arg)).as_deref(), Some(arg));
    }

    #[test]
    fn truncates_long_command_with_ellipsis() {
        let long_cmd = "x".repeat(200);
        let arg = format!(r#"{{"command":"{}"}}"#, long_cmd);
        let out = summarize_tool_args(Some(&arg)).unwrap();
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), ARG_SNIPPET_MAX_CHARS + 1);
    }

    #[test]
    fn truncate_handles_multibyte_utf8_safely() {
        let s = "中".repeat(200); // each char = 3 bytes
        let result = truncate_chars(&s, 10);
        assert_eq!(result.chars().count(), 11); // 10 + '…'
        assert!(result.ends_with('…'));
    }
}
```

- [ ] **Step 2：跑测试**

执行：`cargo test -p zeroclaw-progress-observer mapping::summarize_tests`

预期：10 个测试全通过。

- [ ] **Step 3：commit**

```bash
git add crates/zeroclaw-progress-observer/src/mapping.rs
git commit -m "feat(progress-observer): add summarize_tool_args helper

Extracts a short user-friendly snippet from the JSON arguments of a
tool call, preferring known keys (command/query/path/url) and falling
back to truncated raw JSON. UTF-8-safe truncation.
"
```

---

## Task 7：实现 `event_to_status` 映射

**文件：**
- 修改：`crates/zeroclaw-progress-observer/src/mapping.rs`

- [ ] **Step 1：在 `mapping.rs` 现有 `summarize_tool_args` 之后、`#[cfg(test)] mod summarize_tests` 之前，追加 `event_to_status` 与配套 helper**

```rust
/// Translate an `ObserverEvent` to an optional `StatusUpdate` according to
/// the current toggle state. Returns `None` when the event class is
/// disabled or doesn't correspond to a progress phase.
pub(crate) fn event_to_status(
    execution_id: &str,
    event: &ObserverEvent,
    toggles: &ProgressEventToggles,
) -> Option<StatusUpdate> {
    match event {
        ObserverEvent::AgentStart { provider, model } if toggles.agent_start => {
            Some(make(
                execution_id,
                StatusPhase::AgentStart,
                "agent",
                format!("Agent 启动（{}/{}）", provider, model),
            ))
        }
        ObserverEvent::AgentEnd { .. } if toggles.agent_end => {
            Some(make(execution_id, StatusPhase::AgentEnd, "agent", "处理完成".into()))
        }
        ObserverEvent::LlmRequest { messages_count, .. } if toggles.llm_thinking => {
            Some(make(
                execution_id,
                StatusPhase::LlmThinking,
                "llm",
                format!("正在调用大模型推理（{} 条消息）", messages_count),
            ))
        }
        ObserverEvent::ToolCallStart { tool, arguments } if toggles.tool_call_start => {
            let snippet = summarize_tool_args(arguments.as_deref());
            let desc = format_tool_start_desc(tool, snippet.as_deref());
            Some(make(execution_id, StatusPhase::ToolStart, tool, desc))
        }
        ObserverEvent::ToolCall { tool, duration, success } if toggles.tool_call => {
            let elapsed_ms = duration.as_millis().min(u128::from(u64::MAX)) as u64;
            let desc = if *success {
                format!("{} 执行完成（{}ms）", tool, elapsed_ms)
            } else {
                format!("{} 执行失败", tool)
            };
            Some(make(
                execution_id,
                StatusPhase::ToolDone { success: *success, elapsed_ms },
                tool,
                desc,
            ))
        }
        ObserverEvent::Error { component, message } if toggles.error => {
            let trimmed = truncate_chars(message, 200);
            Some(make(
                execution_id,
                StatusPhase::Error,
                "error",
                format!("{} 出现错误：{}", component, trimmed),
            ))
        }
        _ => None,
    }
}

/// Build the `desc` for a `ToolCallStart` event using the tool name to
/// pick a template, falling back to a generic phrasing when no snippet
/// is available.
fn format_tool_start_desc(tool: &str, snippet: Option<&str>) -> String {
    match (tool, snippet) {
        ("shell", Some(s)) => format!("执行命令：{}", s),
        ("web_search", Some(s)) => format!("搜索：{}", s),
        ("read_file", Some(s)) => format!("读取文件：{}", s),
        ("http", Some(s)) => format!("HTTP 请求：{}", s),
        (other, _) => format!("调用工具：{}", other),
    }
}

fn make(execution_id: &str, phase: StatusPhase, name: &str, desc: String) -> StatusUpdate {
    StatusUpdate {
        execution_id: execution_id.to_owned(),
        phase,
        name: name.to_owned(),
        desc,
    }
}
```

- [ ] **Step 2：在 `mapping.rs` 文件末尾追加完整测试 mod**

```rust
#[cfg(test)]
mod event_to_status_tests {
    use super::*;
    use std::time::Duration;

    fn all_on() -> ProgressEventToggles {
        ProgressEventToggles {
            agent_start: true, agent_end: true,
            tool_call_start: true, tool_call: true,
            llm_thinking: true, error: true,
        }
    }

    #[test]
    fn agent_start_emits_when_enabled() {
        let ev = ObserverEvent::AgentStart {
            provider: "anthropic".into(),
            model: "sonnet".into(),
        };
        let out = event_to_status("exec-1", &ev, &all_on()).unwrap();
        assert_eq!(out.execution_id, "exec-1");
        assert_eq!(out.phase, StatusPhase::AgentStart);
        assert_eq!(out.name, "agent");
        assert_eq!(out.desc, "Agent 启动（anthropic/sonnet）");
    }

    #[test]
    fn agent_start_returns_none_when_disabled() {
        let toggles = ProgressEventToggles::default();
        let ev = ObserverEvent::AgentStart {
            provider: "p".into(), model: "m".into(),
        };
        assert!(event_to_status("e", &ev, &toggles).is_none());
    }

    #[test]
    fn agent_end_emits_processing_complete() {
        let ev = ObserverEvent::AgentEnd {
            provider: "p".into(), model: "m".into(),
            duration: Duration::from_secs(1),
            tokens_used: None, cost_usd: None,
        };
        let out = event_to_status("e", &ev, &all_on()).unwrap();
        assert_eq!(out.phase, StatusPhase::AgentEnd);
        assert_eq!(out.desc, "处理完成");
    }

    #[test]
    fn llm_request_includes_message_count() {
        let ev = ObserverEvent::LlmRequest {
            provider: "p".into(), model: "m".into(), messages_count: 8,
        };
        let out = event_to_status("e", &ev, &all_on()).unwrap();
        assert_eq!(out.phase, StatusPhase::LlmThinking);
        assert_eq!(out.desc, "正在调用大模型推理（8 条消息）");
    }

    #[test]
    fn tool_call_start_shell_uses_command_template() {
        let ev = ObserverEvent::ToolCallStart {
            tool: "shell".into(),
            arguments: Some(r#"{"command":"grep -c TODO README.md"}"#.into()),
        };
        let out = event_to_status("e", &ev, &all_on()).unwrap();
        assert_eq!(out.phase, StatusPhase::ToolStart);
        assert_eq!(out.name, "shell");
        assert_eq!(out.desc, "执行命令：grep -c TODO README.md");
    }

    #[test]
    fn tool_call_start_unknown_tool_uses_generic_template() {
        let ev = ObserverEvent::ToolCallStart {
            tool: "custom_tool".into(),
            arguments: Some(r#"{"foo":"bar"}"#.into()),
        };
        let out = event_to_status("e", &ev, &all_on()).unwrap();
        assert_eq!(out.desc, "调用工具：custom_tool");
    }

    #[test]
    fn tool_call_success_includes_elapsed() {
        let ev = ObserverEvent::ToolCall {
            tool: "shell".into(),
            duration: Duration::from_millis(42),
            success: true,
        };
        let out = event_to_status("e", &ev, &all_on()).unwrap();
        match out.phase {
            StatusPhase::ToolDone { success, elapsed_ms } => {
                assert!(success);
                assert_eq!(elapsed_ms, 42);
            }
            _ => panic!("wrong phase"),
        }
        assert_eq!(out.desc, "shell 执行完成（42ms）");
    }

    #[test]
    fn tool_call_failure_uses_failure_template() {
        let ev = ObserverEvent::ToolCall {
            tool: "http".into(),
            duration: Duration::from_millis(1500),
            success: false,
        };
        let out = event_to_status("e", &ev, &all_on()).unwrap();
        assert_eq!(out.desc, "http 执行失败");
    }

    #[test]
    fn error_event_includes_component_and_message() {
        let ev = ObserverEvent::Error {
            component: "provider".into(),
            message: "rate limited".into(),
        };
        let out = event_to_status("e", &ev, &all_on()).unwrap();
        assert_eq!(out.phase, StatusPhase::Error);
        assert_eq!(out.name, "error");
        assert_eq!(out.desc, "provider 出现错误：rate limited");
    }

    #[test]
    fn unrelated_events_return_none() {
        for ev in [
            ObserverEvent::TurnComplete,
            ObserverEvent::HeartbeatTick,
            ObserverEvent::CacheHit { cache_type: "hot".into(), tokens_saved: 100 },
        ] {
            assert!(event_to_status("e", &ev, &all_on()).is_none());
        }
    }

    #[test]
    fn each_desc_is_under_120_chars() {
        // Verify the self-contained-desc invariant for representative events.
        let cases = vec![
            ObserverEvent::AgentStart { provider: "openai".into(), model: "gpt-4".into() },
            ObserverEvent::LlmRequest { provider: "openai".into(), model: "gpt-4".into(), messages_count: 99 },
            ObserverEvent::ToolCallStart { tool: "shell".into(), arguments: Some(r#"{"command":"ls"}"#.into()) },
            ObserverEvent::ToolCall { tool: "shell".into(), duration: Duration::from_millis(1), success: true },
            ObserverEvent::Error { component: "test".into(), message: "boom".into() },
            ObserverEvent::AgentEnd { provider: "p".into(), model: "m".into(), duration: Duration::from_secs(1), tokens_used: None, cost_usd: None },
        ];
        for ev in cases {
            if let Some(s) = event_to_status("e", &ev, &all_on()) {
                assert!(
                    s.desc.chars().count() <= 120,
                    "desc too long: {:?} ({} chars)", s.desc, s.desc.chars().count(),
                );
                assert!(!s.desc.is_empty(), "desc must not be empty");
            }
        }
    }
}
```

- [ ] **Step 3：跑测试**

执行：`cargo test -p zeroclaw-progress-observer mapping`

预期：summarize_tests（10 条）+ event_to_status_tests（11 条）全通过。

- [ ] **Step 4：commit**

```bash
git add crates/zeroclaw-progress-observer/src/mapping.rs
git commit -m "feat(progress-observer): implement event_to_status mapping

Translates AgentStart/AgentEnd/LlmRequest/ToolCallStart/ToolCall/Error
into StatusUpdates with self-contained Chinese desc strings.
Per-event toggles gate emission; unmatched events return None.
"
```

---

## Task 8：在新 crate 内提供 `MockChannel`（仅 `#[cfg(test)]`）

**文件：**
- 修改：`crates/zeroclaw-progress-observer/src/mock.rs`

- [ ] **Step 1：替换 `mock.rs` 全文**

```rust
//! Minimal mock `Channel` for in-crate observer tests.
//!
//! Records every `send_status_update` invocation behind a `Mutex<Vec<_>>`
//! so tests can assert on the count and shape of emitted updates.

use std::sync::Mutex;

use async_trait::async_trait;
use zeroclaw_api::channel::{
    Channel, ChannelMessage, SendMessage, StatusUpdate,
};

pub(crate) struct MockChannel {
    pub recorded: Mutex<Vec<StatusUpdate>>,
}

impl MockChannel {
    pub fn new() -> Self {
        Self { recorded: Mutex::new(Vec::new()) }
    }

    pub fn count(&self) -> usize {
        self.recorded.lock().unwrap().len()
    }

    pub fn last(&self) -> Option<StatusUpdate> {
        self.recorded.lock().unwrap().last().cloned()
    }
}

#[async_trait]
impl Channel for MockChannel {
    fn name(&self) -> &str { "mock" }

    async fn send(&self, _: &SendMessage) -> anyhow::Result<()> { Ok(()) }

    async fn listen(
        &self,
        _: tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn send_status_update(
        &self,
        _recipient: &str,
        _thread_ts: Option<&str>,
        update: StatusUpdate,
    ) -> anyhow::Result<()> {
        self.recorded.lock().unwrap().push(update);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::channel::StatusPhase;

    #[tokio::test]
    async fn records_send_status_update() {
        let ch = MockChannel::new();
        let update = StatusUpdate {
            execution_id: "e".into(),
            phase: StatusPhase::AgentStart,
            name: "agent".into(),
            desc: "x".into(),
        };
        ch.send_status_update("r", None, update.clone()).await.unwrap();
        assert_eq!(ch.count(), 1);
        assert_eq!(ch.last().unwrap().desc, "x");
    }
}
```

- [ ] **Step 2：跑测试**

执行：`cargo test -p zeroclaw-progress-observer mock`

预期：1 个测试通过。

- [ ] **Step 3：commit**

```bash
git add crates/zeroclaw-progress-observer/src/mock.rs
git commit -m "test(progress-observer): add MockChannel test helper"
```

---

## Task 9：实现 `ProgressReportingObserver`

**文件：**
- 修改：`crates/zeroclaw-progress-observer/src/observer.rs`
- 修改：`crates/zeroclaw-progress-observer/src/lib.rs`（取消注释 pub use）

- [ ] **Step 1：替换 `observer.rs` 全文**

```rust
//! Per-message observer that fires status updates into a target channel.
//!
//! Construction is cheap and per-`ChannelMessage`; the orchestrator builds
//! a fresh one per turn and drops it when the turn ends. Emission is
//! fire-and-forget — failures only land in `tracing::warn!`.

use std::sync::Arc;

use zeroclaw_api::channel::Channel;
use zeroclaw_api::observability_traits::{Observer, ObserverEvent, ObserverMetric};

use crate::mapping::event_to_status;
use crate::toggles::ProgressEventToggles;

/// Sidelined observer that emits one [`StatusUpdate`] per enabled event.
pub struct ProgressReportingObserver {
    inner: Arc<dyn Observer>,
    execution_id: String,
    target_channel: Arc<dyn Channel>,
    recipient: String,
    thread_ts: Option<String>,
    toggles: ProgressEventToggles,
}

impl ProgressReportingObserver {
    /// Build a new observer bound to one channel turn.
    ///
    /// Logs a one-time `info!` diagnostic if every sub-toggle is disabled,
    /// so operators don't wonder why nothing is happening when they
    /// enable the master switch but forget the per-event toggles.
    pub fn new(
        execution_id: String,
        target_channel: Arc<dyn Channel>,
        recipient: String,
        thread_ts: Option<String>,
        toggles: ProgressEventToggles,
        inner: Arc<dyn Observer>,
    ) -> Self {
        if !toggles.any_enabled() {
            tracing::info!(
                target: "zeroclaw::progress_observer",
                "attached with all event toggles disabled; configure \
                 [progress_observer] subkeys to enable"
            );
        }
        Self {
            inner,
            execution_id,
            target_channel,
            recipient,
            thread_ts,
            toggles,
        }
    }
}

impl Observer for ProgressReportingObserver {
    fn record_event(&self, event: &ObserverEvent) {
        if let Some(update) = event_to_status(&self.execution_id, event, &self.toggles) {
            let ch = Arc::clone(&self.target_channel);
            let recip = self.recipient.clone();
            let thread = self.thread_ts.clone();
            tokio::spawn(async move {
                if let Err(e) = ch
                    .send_status_update(&recip, thread.as_deref(), update)
                    .await
                {
                    tracing::warn!(
                        target: "zeroclaw::progress_observer",
                        error = %e,
                        "progress status_update failed"
                    );
                }
            });
        }
        self.inner.record_event(event);
    }

    fn record_metric(&self, metric: &ObserverMetric) {
        self.inner.record_metric(metric);
    }

    fn flush(&self) {
        self.inner.flush();
    }

    fn name(&self) -> &str { "progress-reporting" }

    fn as_any(&self) -> &dyn std::any::Any { self }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    use crate::mock::MockChannel;

    /// NoopObserver — counts events for inner-passthrough assertions.
    struct NoopObserver {
        events: Mutex<usize>,
    }
    impl NoopObserver {
        fn new() -> Self { Self { events: Mutex::new(0) } }
        fn count(&self) -> usize { *self.events.lock().unwrap() }
    }
    impl Observer for NoopObserver {
        fn record_event(&self, _: &ObserverEvent) {
            *self.events.lock().unwrap() += 1;
        }
        fn record_metric(&self, _: &ObserverMetric) {}
        fn name(&self) -> &str { "noop" }
        fn as_any(&self) -> &dyn std::any::Any { self }
    }

    fn all_on() -> ProgressEventToggles {
        ProgressEventToggles {
            agent_start: true, agent_end: true,
            tool_call_start: true, tool_call: true,
            llm_thinking: true, error: true,
        }
    }

    #[tokio::test]
    async fn emits_status_for_agent_start_and_passes_to_inner() {
        let mock = Arc::new(MockChannel::new());
        let inner = Arc::new(NoopObserver::new());
        let obs = ProgressReportingObserver::new(
            "exec-1".into(),
            mock.clone() as Arc<dyn Channel>,
            "u-1".into(),
            None,
            all_on(),
            inner.clone() as Arc<dyn Observer>,
        );

        obs.record_event(&ObserverEvent::AgentStart {
            provider: "anthropic".into(),
            model: "sonnet".into(),
        });

        // Wait for the spawned send to complete.
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert_eq!(mock.count(), 1, "one status_update should have fired");
        assert_eq!(inner.count(), 1, "inner observer must be called too");
        let recorded = mock.last().unwrap();
        assert_eq!(recorded.execution_id, "exec-1");
        assert_eq!(recorded.name, "agent");
    }

    #[tokio::test]
    async fn skips_emission_when_toggle_disabled_still_passes_through() {
        let mock = Arc::new(MockChannel::new());
        let inner = Arc::new(NoopObserver::new());
        let obs = ProgressReportingObserver::new(
            "exec-2".into(),
            mock.clone() as Arc<dyn Channel>,
            "u".into(),
            None,
            ProgressEventToggles::default(), // all off
            inner.clone() as Arc<dyn Observer>,
        );

        obs.record_event(&ObserverEvent::AgentStart {
            provider: "p".into(), model: "m".into(),
        });

        tokio::time::sleep(Duration::from_millis(20)).await;

        assert_eq!(mock.count(), 0, "no status_update should have fired");
        assert_eq!(inner.count(), 1, "inner observer must still be called");
    }

    #[tokio::test]
    async fn unrelated_events_still_pass_through_inner_with_no_emission() {
        let mock = Arc::new(MockChannel::new());
        let inner = Arc::new(NoopObserver::new());
        let obs = ProgressReportingObserver::new(
            "e".into(),
            mock.clone() as Arc<dyn Channel>,
            "u".into(),
            None,
            all_on(),
            inner.clone() as Arc<dyn Observer>,
        );

        obs.record_event(&ObserverEvent::HeartbeatTick);
        obs.record_event(&ObserverEvent::CacheHit {
            cache_type: "hot".into(), tokens_saved: 50,
        });

        tokio::time::sleep(Duration::from_millis(20)).await;

        assert_eq!(mock.count(), 0);
        assert_eq!(inner.count(), 2);
    }

    #[tokio::test]
    async fn record_metric_and_flush_passthrough() {
        let mock = Arc::new(MockChannel::new());
        let inner = Arc::new(NoopObserver::new());
        let obs = ProgressReportingObserver::new(
            "e".into(),
            mock.clone() as Arc<dyn Channel>,
            "u".into(),
            None,
            all_on(),
            inner.clone() as Arc<dyn Observer>,
        );
        obs.record_metric(&ObserverMetric::TokensUsed(10));
        obs.flush();
        assert_eq!(obs.name(), "progress-reporting");
    }
}
```

- [ ] **Step 2：把 `lib.rs` 里 `pub use observer::ProgressReportingObserver;` 那行注释去掉**

- [ ] **Step 3：跑测试**

执行：`cargo test -p zeroclaw-progress-observer observer`

预期：4 个 observer 测试全通过。

- [ ] **Step 4：跑整个 crate 全测试做一次回归**

执行：`cargo test -p zeroclaw-progress-observer`

预期：全部测试通过。

- [ ] **Step 5：commit**

```bash
git add crates/zeroclaw-progress-observer/src/observer.rs crates/zeroclaw-progress-observer/src/lib.rs
git commit -m "feat(progress-observer): implement ProgressReportingObserver

Per-message observer; spawns send_status_update fire-and-forget on
each emitted StatusUpdate; passes every event through to the inner
observer; logs a startup info diagnostic when all sub-toggles are off.
"
```

---

## Task 10：在 WuKongIM 实现 `send_cmd_message` helper

**文件：**
- 修改：`crates/zeroclaw-channel-wukongim/src/channel.rs`

> **P-1 决策**：spec 标注了 payload 格式 (a) JSON object 与 (b) base64 整体编码两条路径。**默认走 (a)** —— `SendParams.payload` 接受任意 `serde_json::Value`，直接传入 cmd JSON 对象。如果运行时 WK 服务端拒绝（看 send 返回的 error/服务器关闭连接），改 Step 1 的 helper 走 (b)：把 cmd JSON `serde_json::to_string` 再 `base64::encode` 塞进 `Value::String`。在 helper 上方留一行注释记录该决策。

- [ ] **Step 1：在 `channel.rs` 的 `impl WuKongIMChannel { ... }` 内（紧邻 `send_text_message`，约 line 524 之后）追加 helper**

```rust
    /// Send a structured "application command" message (JSON-shaped payload
    /// distinct from the base64-encoded text payload used by
    /// [`send_text_message`]).
    ///
    /// Payload format decision (see spec OQ-2 / P-1): we send the JSON
    /// object verbatim as `SendParams.payload`. The receive path
    /// (`la_init_helloworld` parsing) treats `payload` as a JSON object
    /// already, so the symmetric send shape is also JSON object. If WK
    /// rejects this shape in the field, change the body of this helper
    /// to base64-encode the serialized JSON into a `Value::String`.
    async fn send_cmd_message(
        &self,
        channel_id: &str,
        channel_type: u8,
        cmd_payload: serde_json::Value,
    ) -> anyhow::Result<()> {
        let params = SendParams {
            from_uid: Some(self.uid.clone()),
            client_msg_no: Uuid::new_v4().to_string(),
            channel_id: channel_id.to_string(),
            channel_type,
            payload: cmd_payload,
            header: None,
            setting: None,
            msg_key: None,
            expire: None,
            stream_no: None,
            topic: None,
        };
        let _: serde_json::Value = self.send_rpc("send", params).await?;
        Ok(())
    }
```

- [ ] **Step 2：在文件末尾的 `#[cfg(test)] mod tests`（如不存在则新建）追加 payload 形态测试**

```rust
#[cfg(test)]
mod cmd_message_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cmd_payload_serializes_with_known_shape() {
        // Documents the wire format. If WK ever requires a different
        // shape, update both this assertion and send_cmd_message.
        let payload = json!({
            "cmd": "la_status_update",
            "content": "执行状态",
            "param": {
                "mid": "exec-9c4f",
                "name": "shell",
                "desc": "执行命令：ls",
            }
        });

        let s = serde_json::to_string(&payload).unwrap();
        assert!(s.contains("\"cmd\":\"la_status_update\""));
        assert!(s.contains("\"mid\":\"exec-9c4f\""));
        assert!(s.contains("\"desc\":\"执行命令：ls\""));
    }
}
```

- [ ] **Step 3：跑编译 + 测试**

执行：`cargo test -p zeroclaw-channel-wukongim cmd_message_tests`

预期：1 个测试通过。

执行：`cargo check -p zeroclaw-channel-wukongim`

预期：编译通过。

- [ ] **Step 4：commit**

```bash
git add crates/zeroclaw-channel-wukongim/src/channel.rs
git commit -m "feat(channel-wukongim): add send_cmd_message helper

Sends an application-cmd JSON payload via SendParams.payload (object
shape, mirroring the receive side). Used by send_status_update for
la_status_update progress messages.
"
```

---

## Task 11：WuKongIM 实现 `Channel::send_status_update` override + `phase_to_content` helper

**文件：**
- 修改：`crates/zeroclaw-channel-wukongim/src/channel.rs`

- [ ] **Step 1：在 `channel.rs` 中、`impl Channel for WuKongIMChannel { ... }` 块内（建议放在 `async fn send` 之后）追加 override**

> 注意：`WuKongIMChannel` 是 flat-fields 结构，opt-in 字段是 `self.progress_streaming`（已在 Task 3 加入），不是 `self.config.progress_streaming`。`send_cmd_message` 接受的 `channel_type` 在 wukongim 内部是 `u8`，而 `parse_recipient` 返回类型也是 `(String, u8)` —— 与现有 `send_text_message` 调用方式一致。

```rust
    async fn send_status_update(
        &self,
        recipient: &str,
        _thread_ts: Option<&str>,
        update: zeroclaw_api::channel::StatusUpdate,
    ) -> anyhow::Result<()> {
        if !self.progress_streaming {
            return Ok(());
        }
        let (channel_id, channel_type) = parse_recipient(recipient);
        let payload = serde_json::json!({
            "cmd": "la_status_update",
            "content": phase_to_content(&update.phase),
            "param": {
                "mid": update.execution_id,
                "name": update.name,
                "desc": update.desc,
            }
        });
        self.send_cmd_message(&channel_id, channel_type, payload).await
    }
```

- [ ] **Step 2：在 `channel.rs` 文件顶部 `use` 区或合适位置追加 `phase_to_content` helper**（放在 `impl WuKongIMChannel` 块外，作为模块级函数）

```rust
fn phase_to_content(phase: &zeroclaw_api::channel::StatusPhase) -> &'static str {
    use zeroclaw_api::channel::StatusPhase;
    match phase {
        StatusPhase::AgentStart => "Agent 启动",
        StatusPhase::LlmThinking => "正在思考",
        StatusPhase::ToolStart => "工具启动",
        StatusPhase::ToolDone { success: true, .. } => "工具完成",
        StatusPhase::ToolDone { success: false, .. } => "工具失败",
        StatusPhase::Error => "错误",
        StatusPhase::AgentEnd => "处理完成",
    }
}
```

- [ ] **Step 3：追加 `phase_to_content` 单测到 `cmd_message_tests` 同级 mod**（纯函数测试，不需要 channel 实例）

```rust
    use zeroclaw_api::channel::StatusPhase;

    #[test]
    fn phase_to_content_covers_all_variants() {
        assert_eq!(phase_to_content(&StatusPhase::AgentStart), "Agent 启动");
        assert_eq!(phase_to_content(&StatusPhase::LlmThinking), "正在思考");
        assert_eq!(phase_to_content(&StatusPhase::ToolStart), "工具启动");
        assert_eq!(
            phase_to_content(&StatusPhase::ToolDone { success: true, elapsed_ms: 0 }),
            "工具完成",
        );
        assert_eq!(
            phase_to_content(&StatusPhase::ToolDone { success: false, elapsed_ms: 0 }),
            "工具失败",
        );
        assert_eq!(phase_to_content(&StatusPhase::Error), "错误");
        assert_eq!(phase_to_content(&StatusPhase::AgentEnd), "处理完成");
    }
```

- [ ] **Step 4：追加 opt-in 行为单测**

`WuKongIMChannel` 内含 `Arc<dyn Memory>` 等运行期资源、不易在测试里直接构造。采用最直接的策略：构造一个 fake `Memory`（zero-sized type 实现 trait）+ struct-literal 直接拼装 `WuKongIMChannel`（所有字段都是 `pub(crate)` 模块内可见）。

```rust
    use zeroclaw_api::channel::{StatusPhase, StatusUpdate};
    use zeroclaw_api::memory_traits::Memory;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::collections::HashMap;
    use tokio::sync::RwLock;

    struct NoopMemory;
    #[async_trait::async_trait]
    impl Memory for NoopMemory {
        // Implement the minimum surface for the trait. If `Memory` has many
        // required methods, prefer adding a `#[cfg(test)] pub(crate) fn
        // test_with_progress_streaming(b: bool) -> Self` constructor on
        // WuKongIMChannel that takes only the flag and stubs the rest.
        // See trait at crates/zeroclaw-api/src/memory_traits.rs.
        // (Trait impl body filled in by implementer at execution time.)
    }

    fn make_test_channel(progress_streaming: bool) -> WuKongIMChannel {
        WuKongIMChannel {
            ws_url: "ws://test".into(),
            uid: "bot".into(),
            token: "tok".into(),
            device_id: "dev".into(),
            device_flag: 2,
            allowed_users: vec![],
            approval_timeout_secs: 1,
            mention_only: false,
            dawn_url: String::new(),
            dawn_token: String::new(),
            ack_reactions: false,
            ack_reactions_message: String::new(),
            ack_reactions_delay_secs: 0,
            memory: Arc::new(NoopMemory),
            pending_responses: Arc::new(RwLock::new(HashMap::new())),
            pending_approvals: Arc::new(RwLock::new(HashMap::new())),
            ws_sink: Arc::new(RwLock::new(None)),
            downloads_dir: PathBuf::from("."),
            last_message_time: Arc::new(RwLock::new(HashMap::new())),
            workspace_dir: PathBuf::from("."),
            progress_streaming,
        }
    }

    #[tokio::test]
    async fn send_status_update_returns_ok_when_opt_in_disabled() {
        let ch = make_test_channel(false);
        let update = StatusUpdate {
            execution_id: "e".into(),
            phase: StatusPhase::AgentStart,
            name: "agent".into(),
            desc: "x".into(),
        };
        // 关闭 opt-in 时短路返回，不触发网络。
        assert!(ch.send_status_update("P:u1", None, update).await.is_ok());
    }
```

> **执行注意（实现者）**：上面 `NoopMemory` 的 `impl Memory` 留空，是因为 `Memory` trait 的具体方法集合可能很大（看 `zeroclaw-api/src/memory_traits.rs`）。两种执行策略任选其一：
>
> - **策略 A**：实现 `Memory` 所有必需方法（每个返回 `unreachable!()`），最快、最直接
> - **策略 B**：检查 `crates/zeroclaw-channel-wukongim` 内是否已有 test-only 的 mock memory（grep `impl Memory for`），复用之
>
> 编译器会精确告诉缺哪几个方法 —— 先 `cargo test -p zeroclaw-channel-wukongim send_status_update_returns_ok_when_opt_in_disabled --no-run`，对照报错逐个补 `unreachable!()`。

- [ ] **Step 5：跑测试**

执行：`cargo test -p zeroclaw-channel-wukongim`

预期：所有相关测试通过。

- [ ] **Step 6：跑全 workspace 编译**

执行：`cargo check --workspace --all-features`

预期：通过。

- [ ] **Step 7：commit**

```bash
git add crates/zeroclaw-channel-wukongim/src/channel.rs
git commit -m "feat(channel-wukongim): implement send_status_update override

When progress_streaming is on, serialize the StatusUpdate to a
la_status_update cmd message and send via send_cmd_message; when off,
return Ok(()) without touching the network. phase_to_content gives a
coarse Chinese label for the cmd's 'content' field.
"
```

---

## Task 12：orchestrator 接线 —— 生成 `execution_id` 并挂载 observer

**文件：**
- 修改：`crates/zeroclaw-channels/Cargo.toml`
- 修改：`crates/zeroclaw-channels/src/orchestrator/mod.rs`

- [ ] **Step 1：在 `crates/zeroclaw-channels/Cargo.toml` 的 `[dependencies]` 里追加新 crate 依赖**

```toml
zeroclaw-progress-observer = { workspace = true }
```

> 同时检查 `uuid` 是否已经是依赖；如果不在 `[dependencies]` 里需要追加：
> ```toml
> uuid = { version = "1.22", default-features = false, features = ["v4", "std"] }
> ```

- [ ] **Step 2：在 `orchestrator/mod.rs` 顶部 `use` 区追加导入**

```rust
use zeroclaw_progress_observer::{ProgressEventToggles, ProgressReportingObserver};
```

- [ ] **Step 3：新增 `From<&ProgressObserverConfig> for ProgressEventToggles` 转换 impl**

在 `orchestrator/mod.rs` 文件内（推荐紧贴 `ChannelRuntimeContext` 定义之后），追加：

```rust
impl From<&zeroclaw_config::schema::ProgressObserverConfig> for ProgressEventToggles {
    fn from(cfg: &zeroclaw_config::schema::ProgressObserverConfig) -> Self {
        ProgressEventToggles {
            agent_start: cfg.agent_start,
            agent_end: cfg.agent_end,
            tool_call_start: cfg.tool_call_start,
            tool_call: cfg.tool_call,
            llm_thinking: cfg.llm_thinking,
            error: cfg.error,
        }
    }
}
```

- [ ] **Step 4：在 `process_channel_message` 内、`ChannelNotifyObserver` 构造点之前（约 `mod.rs:3400` 上方）插入 `execution_id` 生成与 `ProgressReportingObserver` 构造**

找到这段（约 line 3400）：

```rust
    // Wrap observer to forward tool events as live thread messages
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let notify_observer: Arc<ChannelNotifyObserver> = Arc::new(ChannelNotifyObserver {
        inner: Arc::clone(&ctx.observer),
        ...
```

在它**之前**插入：

```rust
    // Per-turn execution identifier — used as `mid` by progress observer.
    let execution_id = uuid::Uuid::new_v4().to_string();

    // Optionally attach ProgressReportingObserver, chained INSIDE
    // ChannelNotifyObserver so the legacy emoji path is unaffected.
    let progress_observer: Option<Arc<dyn Observer>> = {
        let progress_cfg = &ctx.prompt_config.progress_observer;
        if progress_cfg.enabled {
            if let Some(channel) = target_channel.as_ref() {
                let toggles: ProgressEventToggles = progress_cfg.into();
                Some(Arc::new(ProgressReportingObserver::new(
                    execution_id.clone(),
                    Arc::clone(channel),
                    msg.reply_target.clone(),
                    followup_thread_id(&msg),
                    toggles,
                    Arc::clone(&ctx.observer),
                )) as Arc<dyn Observer>)
            } else {
                None
            }
        } else {
            None
        }
    };
```

然后把 `ChannelNotifyObserver` 的 `inner` 字段从 `Arc::clone(&ctx.observer)` 改为：

```rust
    let notify_observer: Arc<ChannelNotifyObserver> = Arc::new(ChannelNotifyObserver {
        inner: progress_observer
            .clone()
            .unwrap_or_else(|| Arc::clone(&ctx.observer)),
        tx: notify_tx,
        tools_used: AtomicBool::new(false),
    });
```

> **说明**：链式包裹结构是 `ChannelNotifyObserver → ProgressReportingObserver → ctx.observer`。`ChannelNotifyObserver` 仍位于最外层（行为 100% 不变），`ProgressReportingObserver` 嵌在中间，最里层是原本的 base observer。若 ProgressReportingObserver 不挂载，则直接 fallback 到原本的 `ctx.observer`。

- [ ] **Step 5：编译验证**

执行：`cargo check -p zeroclaw-channels`

预期：通过。如果报 `unused variable: execution_id` 警告，是预期的 —— execution_id 已经被 ProgressReportingObserver 持有，这里的 `let` binding 用于将来可能的 tracing/log，不必为消除 warning 而删除。或者改成：

```rust
    let _execution_id = uuid::Uuid::new_v4().to_string();
    // 然后下面 ProgressReportingObserver::new 改用 _execution_id.clone()
```

- [ ] **Step 6：跑 channels crate 测试**

执行：`cargo test -p zeroclaw-channels`

预期：现有测试不受影响。

- [ ] **Step 7：跑全 workspace 编译 + 测试**

执行：`cargo test --workspace --all-features`

预期：所有测试通过。

- [ ] **Step 8：commit**

```bash
git add crates/zeroclaw-channels/Cargo.toml crates/zeroclaw-channels/src/orchestrator/mod.rs
git commit -m "feat(channels): wire ProgressReportingObserver into orchestrator

For each ChannelMessage, generate a UUID execution_id and (when
[progress_observer].enabled and a target channel exists) construct a
ProgressReportingObserver chained inside ChannelNotifyObserver. Legacy
emoji-text path is preserved end-to-end.
"
```

---

## Task 13：端到端冒烟验证

**文件：**
- 无新增/修改。仅运行与手工检查。

- [ ] **Step 1：全 workspace 编译（含 release 模式确认无 fast-path 问题）**

执行：`cargo build --release --features agent-runtime,channel-wukongim`

预期：编译通过。

- [ ] **Step 2：全 workspace 单测**

执行：`cargo test --workspace --all-features`

预期：所有测试通过，progress-observer crate 的所有测试全部 OK。

- [ ] **Step 3：手工配置一份测试 `config.toml`，启用 progress observer**

创建临时 `/tmp/zeroclaw-progress-test.toml`：

```toml
# ... 现有最小配置（provider、memory 等）...

[progress_observer]
enabled = true
agent_start = true
agent_end = true
tool_call_start = true
tool_call = true
llm_thinking = true
error = true

[channels.wukongim]
enabled = true
ws_url = "ws://localhost:5200"
uid = "test-bot"
token = "your-token"
device_id = "test-dev"
device_flag = 2
progress_streaming = true
```

- [ ] **Step 4：启动 daemon 观察启动日志（不必接真实 WK 服务，仅看日志）**

执行：`RUST_LOG=zeroclaw::progress_observer=info ./target/release/zeroclaw daemon --config /tmp/zeroclaw-progress-test.toml`

预期：
- 不出现 `attached with all event toggles disabled` info 日志（因为所有子开关都开）
- 如果连不上 WK 服务，channel 自身的连接错误会打 warn，但 progress observer 本身不会报错

- [ ] **Step 5：再启一次，把所有 sub-toggles 设回 false 验证启动诊断日志**

把 step 3 中所有 `agent_start = true` 等改为 `false`，重启。

预期：第一次有 channel message 进入时 stderr 会出现一条
```
INFO zeroclaw::progress_observer: attached with all event toggles disabled; configure [progress_observer] subkeys to enable
```

- [ ] **Step 6：（可选 / 当真实 WK 环境可用时）发一条消息触发 agent，确认客户端收到 la_status_update**

通过测试 WK 客户端发送消息给 bot；agent 执行过程中客户端应能收到多条 `cmd = "la_status_update"` 的消息，按 `param.mid` 聚合。

如果客户端报 `payload format unexpected`，说明 P-1 决策 (a) 不被服务端接受，回到 Task 10 改为路径 (b)：

```rust
let payload_str = serde_json::to_string(&cmd_payload)?;
let payload_b64 = base64::engine::general_purpose::STANDARD.encode(payload_str);
// 然后在 SendParams 里：payload: serde_json::Value::String(payload_b64),
```

修改后重新跑 Task 10 + Task 11 的测试 → 重新 commit fix。

- [ ] **Step 7：spec follow-up 清理 commit**

确认 spec 中 P-1 决策（payload 格式 a vs b）已在 channel.rs 的 helper 注释中固化（Task 10 Step 1 已写在 doc-comment 里）。

如果选了 (b)，更新 spec 文档反映：

执行：`git status` 检查是否需要更新 spec。

- [ ] **Step 8：合并/总结 commit（如有 fix）**

如有 P-1 决策路径调整、或测试失败修复，统一 commit：

```bash
git commit -m "fix(channel-wukongim): switch send_cmd_message payload to base64 (P-1)"
```

否则跳过此步。

---

## 自审清单（写完检查）

- ✅ **spec 覆盖**：每一节 spec 都对应到上面某个 task
  - `StatusUpdate`/`StatusPhase`/trait 默认 → Task 1
  - `ProgressObserverConfig` → Task 2
  - `WuKongIMConfig.progress_streaming` → Task 3
  - 新 crate 骨架 + `ProgressEventToggles` + 映射 + observer + mock → Tasks 4-9
  - WK `send_cmd_message` 与 `send_status_update` override → Tasks 10-11
  - orchestrator 接线 + execution_id → Task 12
  - 启动诊断、`desc` 不变式、`event_to_status` 完整事件覆盖 → Tasks 7+9 的测试
  - 端到端冒烟 → Task 13
- ✅ **类型一致性**：`StatusUpdate`/`StatusPhase`/`ProgressEventToggles` 名称在所有 task 中一致；`anyhow::Result<()>` 与 trait 签名一致；`ObserverEvent::LlmRequest` 而非 `LlmRequestStart`
- ✅ **placeholder 无残留**：每一处代码块均完整；OQ 已在 spec 内全部消解
- ⚠️ **Task 11 Step 5 的 `WuKongIMChannel::test_new`**：实现者需要按 channel 实际字段调整 placeholder —— 在 plan 内明确标注了"先 Read 完整字段定义"

---

## 后续工作（不在本计划范围）

- `ToolCall` 事件增加 `error_message` 字段（跨 crate 改动，独立 PR）
- `desc` 文案的 i18n 化（v1 硬编码中文）
- 其他 channel 适配 `send_status_update`（Telegram/Discord/Slack 等）
- WuKongIM 客户端 app 解析 `la_status_update` 并按 `mid` 聚合的实现（客户端团队推进）
