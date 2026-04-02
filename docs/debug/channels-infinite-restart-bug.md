# Channels 无限重启 Bug 分析报告

**日期**: 2026-04-02
**分支**: feat/config-hot-reload
**涉及 commits**: `09164e40`, `7029b7a8`
**状态**: 已修复（待验证）

> 存在两个独立的无限重启 Bug，由不同操作触发，但最终都表现为 `Daemon component 'channels' exited unexpectedly` 持续滚动。

---

## 一、Bug 概述

| | Bug 1 | Bug 2 |
|---|---|---|
| **触发条件** | PUT /api/config 更新渠道配置（渠道仍存在） | PUT /api/config **删除**所有渠道配置 |
| **日志特征** | `channels closure invoked: token is_cancelled=true` | `No channels configured. Run zeroclaw onboard` |
| **循环间隔** | `channel_initial_backoff_secs`（约 2 秒） | 同上 |
| **根因** | 旧 supervisor closure 持有已取消的 token | 新 supervisor 无限重试立即返回 `Ok(())` 的 `start_channels` |
| **修复方式** | `restart_channels` abort 旧 supervisor | `restart_channels` 无渠道时不 spawn supervisor |

---

## 二、整体架构背景

```mermaid
graph TD
    A[daemon::run] -->|spawn| GW[gateway supervisor]
    A -->|spawn| CH[channels supervisor<br/>JoinHandle → local handles vec]
    A -->|Arc| DR[DaemonReloader<br/>channel_cancel: ArcSwap&lt;T&gt;<br/>channels_abort: Mutex&lt;AbortHandle&gt;]

    GW --> API[PUT /api/config<br/>handle_api_config_put]
    API -->|channels_config 有变化| RC[restart_channels]

    RC -->|abort| CH
    RC -->|cancel| CT[old CancellationToken]
    RC -->|spawn| CH2[新 channels supervisor]

    subgraph start_channels
        SC1{有渠道?}
        SC1 -->|No| RET["return Ok(()) 立即"]
        SC1 -->|Yes| SEL[tokio::select!<br/>dispatch_loop / cancel.cancelled]
    end

    CH2 --> SC1
```

---

## 三、Bug 1 — 旧 supervisor 被 token 毒化后死循环

### 触发场景
PUT /api/config 更新渠道配置（如修改 feishu 的参数），但保留渠道。

### 调用链

```mermaid
sequenceDiagram
    participant RC as restart_channels
    participant S0 as 初始 Supervisor<br/>(closure 持有 Arc→T1)
    participant SC as start_channels

    Note over S0,SC: 正常运行中，start_channels 阻塞在 select!

    RC->>RC: 创建 T2，swap ArcSwap
    RC->>T1: old_cancel.cancel() ← T1 被永久取消
    RC->>S1: spawn 新 supervisor（持有 T2）
    Note over RC: ❌ 旧 S0 的 JoinHandle 不在 reloader 里，无法 abort

    T1-->>SC: cancel.cancelled() 触发
    SC-->>S0: return Ok(())

    loop 每 ~2 秒
        S0->>S0: Ok(()) → "exited unexpectedly" → 重置 backoff → 休眠
        S0->>SC: 重新调用 start_channels(config, T1.clone(), ...)
        Note over SC: T1.is_cancelled() == true
        SC-->>SC: tokio::select! 立即触发 cancel 分支
        SC-->>S0: return Ok(())
    end
```

### 证据（日志）
```
[DIAG] restart_channels called — old_is_cancelled_before=false   ← 一次
[DIAG] channels closure invoked: token is_cancelled=true          ← 无限重复
[DIAG] entering tokio::select — cancel.is_cancelled=true          ← 无限重复
Daemon component 'channels' exited unexpectedly                   ← 无限重复
```

### 根因代码（修复前）

`daemon/mod.rs` `run()` 第 160–181 行：
```rust
// 初始 supervisor 的 JoinHandle 只存到局部 handles Vec
// DaemonReloader 没有引用，restart_channels 无法 abort 它
let cancel = reloader.channel_cancel.load().clone();  // 捕获 Arc→T1
handles.push(spawn_component_supervisor("channels", ..., move || {
    let c = (*cancel).clone();  // 每次重试都 clone T1（已取消状态）
    ...
}));
```

### 修复
在 `DaemonReloader` 增加 `channels_abort: Mutex<Option<AbortHandle>>`，`restart_channels` 调用前先 abort 旧 supervisor。

---

## 四、Bug 2 — 热删除渠道后新 supervisor 死循环

### 触发场景
PUT /api/config **删除**飞书渠道配置（`channels_config` 中无任何真实渠道）。

### 调用链

```mermaid
sequenceDiagram
    participant WB as workbot
    participant API as handle_api_config_put
    participant RC as restart_channels
    participant S1 as 新 Supervisor
    participant SC as start_channels

    WB->>API: PUT /api/config (无渠道配置)
    API->>API: channels_config 有变化
    API->>RC: restart_channels(new_config，无渠道)

    Note over RC: ❌ 未检查 new_config 是否有渠道，直接 spawn
    RC->>S1: spawn_component_supervisor("channels", ...)

    loop 每 ~2 秒
        S1->>SC: start_channels(无渠道 config, T2, ...)
        SC->>SC: channels.is_empty() == true
        SC-->>SC: println!("No channels configured")
        SC-->>S1: return Ok(())  ← 不是错误，是正常返回
        S1->>S1: Ok(()) → "exited unexpectedly"<br/>backoff 重置为最小值 → 休眠 → 重试
    end
```

### 证据（日志）
```
No sandbox backend available, using application-layer security
No channels configured. Run `zeroclaw onboard` to set up channels.
WARN zeroclaw::daemon: Daemon component 'channels' exited unexpectedly
（以上无限重复，无 token is_cancelled 字样）
```

### 根因代码（修复前）

`daemon/mod.rs` `restart_channels`：
```rust
// ❌ 无论 config 是否有渠道，都无条件 spawn supervisor
let handle = spawn_component_supervisor("channels", initial, max, move || {
    // start_channels 立即 Ok(()) → supervisor 无限重试
    ...
});
```

`daemon/mod.rs` `spawn_component_supervisor`：
```rust
// Ok(()) 被视为"意外退出"并以最小 backoff 无限重试
// 没有"组件表示自己不该运行"的退出机制
Ok(()) => {
    tracing::warn!("Daemon component '{name}' exited unexpectedly");
    backoff = initial_backoff_secs.max(1);  // 重置 backoff！
}
```

### 修复
`restart_channels` 中检查 `has_supervised_channels(&config)`，若无渠道则只 abort 旧 supervisor，不 spawn 新的：

```rust
if !has_supervised_channels(&config) {
    tracing::info!("[DIAG] restart_channels: no supervised channels in new config, skipping supervisor spawn");
    crate::health::mark_component_ok("channels");
    return;
}
```

---

## 五、验证方法

### Bug 1 验证（热更新渠道参数，保留渠道）

修复后预期：
```
[DIAG] restart_channels called — old_is_cancelled_before=false
channel supervisor cancelled, shutting down channels        ← 一次
Daemon component 'channels' exited unexpectedly             ← 一次
[DIAG] channels closure invoked: token is_cancelled=false   ← 新 supervisor 正常
[DIAG] entering tokio::select — cancel.is_cancelled=false
Listening for messages...
```

**不应再出现**：`channels closure invoked: token is_cancelled=true` 持续滚动

### Bug 2 验证（热删除所有渠道配置）

修复后预期：
```
[DIAG] restart_channels called — old_is_cancelled_before=false
channel supervisor cancelled, shutting down channels        ← 一次
[DIAG] restart_channels: no supervised channels in new config, skipping supervisor spawn
Daemon component 'channels' exited unexpectedly             ← 一次（旧 supervisor 退出）
（之后静默，不再出现 channels 相关日志）
```

**不应再出现**：`No channels configured` 持续滚动

INFO  zeroclaw::channels: [DIAG] entering tokio::select — cancel.is_cancelled=true
INFO  zeroclaw::channels: channel supervisor cancelled, shutting down channels
WARN  zeroclaw::daemon: Daemon component 'channels' exited unexpectedly
```

---

## 二、完整调用路径

```mermaid
sequenceDiagram
    participant WB as workbot (外部 Go 服务)
    participant GW as gateway/api.rs<br/>handle_api_config_put
    participant DR as DaemonReloader<br/>restart_channels
    participant S0 as 初始 Supervisor<br/>(closure 持有 T1)
    participant SC as start_channels<br/>(channels/mod.rs)

    Note over S0,SC: 正常阶段：daemon 启动，T1 创建，channels 运行
    S0->>SC: 调用 start_channels(config, T1)
    SC-->>SC: tokio::select! 等待消息或 T1.cancelled()

    Note over WB,DR: 触发阶段：外部更新 channels 配置
    WB->>GW: PUT /api/config (新 channels_config)
    GW->>GW: 对比 channels_config 发现差异
    GW->>DR: restart_channels(new_config)

    Note over DR: Bug 触发点
    DR->>DR: 创建 T2，swap 进 ArcSwap
    DR->>T1: old_cancel.cancel() ← T1 被永久取消
    DR->>S0_new: spawn_component_supervisor(new_config, T2)
    Note over DR: ❌ 旧 supervisor S0 未被 abort

    T1-->>SC: cancel.cancelled() 被触发
    SC->>SC: 打印 "channel supervisor cancelled"
    SC-->>S0: 返回 Ok(())

    Note over S0: ❌ 无限循环开始
    loop 每隔 ~2 秒
        S0->>S0: supervisor 收到 Ok() → 认为"意外退出"→ 重试
        S0->>SC: 再次调用 start_channels(config, ×T1.clone())
        Note over SC: T1 已被取消，clone 出来的 token 也是取消状态
        SC->>SC: 进入 tokio::select!<br/>cancel.is_cancelled() = true 立即触发
        SC-->>S0: 立即返回 Ok(())
    end
```

---

## 三、根因定位

### 3.1 问题所在代码（修复前）

**`src/daemon/mod.rs` — 初始 supervisor 闭包（第 160–181 行）**

```rust
// ❌ 问题：cancel (Arc<CancellationToken>) 在 daemon 启动时一次性捕获
let cancel = reloader.channel_cancel.load().clone();  // 捕获的是 T1

handles.push(spawn_component_supervisor("channels", ..., move || {
    let c = (*cancel).clone();  // 每次重试都 clone T1，共享同一个取消状态
    async move { start_channels(cfg, c, s).await }
}));
// ❌ 问题：JoinHandle 只存到局部 handles，DaemonReloader 无法访问它
```

**`src/daemon/mod.rs` — `restart_channels`（第 74–104 行）**

```rust
fn restart_channels(&self, config: Config) {
    let new_cancel = Arc::new(CancellationToken::new());
    let old_cancel = self.channel_cancel.swap(new_cancel);
    old_cancel.cancel();  // T1 被取消

    // ❌ 问题：新的 supervisor 正确创建了
    let handle = spawn_component_supervisor("channels", ..., closure_with_T2);
    self.handles.lock().push(handle);

    // ❌ 问题：旧的初始 supervisor 完全不受影响
    //    它的 closure 持有的 T1 已经被毒化
    //    它的 JoinHandle 不在 self.handles 里（在 run() 的局部变量里）
    //    它会永远重试，每次用 T1.clone() 立即触发 cancelled()
}
```

### 3.2 Bug 在整体架构中的位置

```mermaid
graph TD
    A[daemon::run] -->|spawn| B[gateway supervisor]
    A -->|spawn| C[channels supervisor<br/>初始，JoinHandle 存到局部 handles]
    A -->|spawn| D[heartbeat/scheduler ...]
    A -->|Arc clone| E[DaemonReloader<br/>channel_cancel: ArcSwap&lt;T1&gt;]

    B --> F[gateway::run_gateway]
    F --> G[PUT /api/config handler]
    G -->|channels_config 有变化| H[restart_channels]

    H -->|swap & cancel| I["T1.cancel() ❌ 毒化<br/>C 的 closure 永久失效"]
    H -->|spawn| J[新 channels supervisor<br/>JoinHandle 存到 reloader.handles]

    C -->|重试 loop| K["closure 再次调用<br/>T1.clone() is_cancelled=true"]
    K -->|立即返回 Ok| C

    style I fill:#ff4444,color:#fff
    style K fill:#ff4444,color:#fff
    style C fill:#ff8800,color:#fff
```

---

## 四、证据链（来自诊断日志）

| 时间 | 日志 | 含义 |
|------|------|------|
| 09:43:34.793 | `[DIAG] restart_channels called — old_is_cancelled_before=false` | workbot 触发 PUT /api/config，T1 此时正常 |
| 09:43:34.793 | `channel supervisor cancelled` | T1 被取消，当前 start_channels 退出 ✓ |
| 09:43:34.794 | `config hot-reload applied actions=["channels-restarted"]` | 新 supervisor 启动 |
| 09:43:34.864 | `No channels configured` | 新 supervisor 用新 config 启动，无 feishu |
| 09:43:36.795 | `[DIAG] channels closure invoked: token is_cancelled=true` | ❌ 旧 supervisor 重试，T1 已死 |
| 09:43:36.879 | `[DIAG] entering tokio::select — cancel.is_cancelled=true` | ❌ 进 select 前就触发 |
| 09:43:36.879 | `Daemon component 'channels' exited unexpectedly` | ❌ 无限循环第 N 次 |

关键证据：**`restart_channels called` 只出现一次，但 `channels closure invoked: is_cancelled=true` 无限重复**，证明是旧 supervisor 的 closure 被毒化后永续循环，而非反复调用 `restart_channels`。

---

## 五、修复方案

### 核心思路

在 `DaemonReloader` 中用 `channels_abort: Mutex<Option<AbortHandle>>` 持有当前 channels supervisor 的中止句柄。`restart_channels` 调用时先 abort 旧 supervisor（终止其重试循环），再启动新的。

### 关键改动

**`src/daemon/mod.rs`**

```rust
pub(crate) struct DaemonReloader {
    // ... 原有字段 ...
    pub channels_abort: std::sync::Mutex<Option<tokio::task::AbortHandle>>,  // ← 新增
}

fn restart_channels(&self, config: Config) {
    // ✅ 先 abort 旧 supervisor，终止其重试循环
    if let Ok(mut h) = self.channels_abort.lock() {
        if let Some(abort) = h.take() {
            abort.abort();
        }
    }

    // 取消旧 token（让仍在运行的 start_channels 优雅退出）
    let new_cancel = Arc::new(CancellationToken::new());
    let old_cancel = self.channel_cancel.swap(new_cancel.clone());
    old_cancel.cancel();

    // 启动新 supervisor，持久化其 AbortHandle
    let handle = spawn_component_supervisor("channels", ..., new_closure_with_T2);
    if let Ok(mut h) = self.channels_abort.lock() {
        *h = Some(handle.abort_handle());  // ✅ 下次 restart_channels 可以 abort 它
    }
    // ...
}

// run() 中初始 supervisor 也要注册 AbortHandle
let ch_handle = spawn_component_supervisor("channels", ...);
if let Ok(mut h) = reloader.channels_abort.lock() {
    *h = Some(ch_handle.abort_handle());  // ✅ 初始 supervisor 也可被 abort
}
handles.push(ch_handle);
```

### 修复后的生命周期

```mermaid
sequenceDiagram
    participant DR as DaemonReloader
    participant S0 as 初始 Supervisor
    participant S1 as 新 Supervisor

    Note over S0: 正常运行，channels_abort = S0.abort_handle()
    DR->>S0: restart_channels 调用<br/>abort(S0.abort_handle()) ✅ 终止重试循环
    DR->>S0: old_cancel.cancel() ✅ 优雅退出当前 start_channels
    DR->>S1: spawn 新 supervisor
    DR->>DR: channels_abort = S1.abort_handle()
    Note over S1: 正常运行，下次 restart_channels 可 abort S1
```

---

## 六、验证方法

修复后，当 `PUT /api/config` 触发 `restart_channels` 时，应观察到：

**预期正常日志模式**：

```
[DIAG] restart_channels called — old_is_cancelled_before=false
channel supervisor cancelled, shutting down channels        ← 旧 supervisor 优雅退出
Daemon component 'channels' exited unexpectedly             ← 仅出现一次
[DIAG] channels closure invoked: token is_cancelled=false   ← 新 supervisor 启动，token 正常
[DIAG] entering tokio::select — cancel.is_cancelled=false   ← 正常进入等待
Listening for messages...                                   ← channels 恢复正常
```

**不应再出现**：
- `channels closure invoked: token is_cancelled=true` 持续滚动
- `Daemon component 'channels' exited unexpectedly` 持续滚动
