# One2X Custom Feature Registry

每个自定义功能的完整档案：实现位置、上游观察条件、等价性判断标准、删除步骤。

**使用方式**：在每次 upstream sync 时，配合 `./dev/check-parity.sh` 一起使用。
脚本发出 ⚠️ REVIEW 信号后，对照本文档的「等价性标准」手动判断是否可以删除。

---

## F-01: Session Hygiene

**状态**: 保留中  
**我们的文件**: `src/one2x/session_hygiene.rs`  
**上游文件改动**: `channels/mod.rs` (+3 hooks), `channels/session_store.rs` (+pub fn)

### 功能说明
防止 session JSONL 无限膨胀导致重启后上下文爆炸 → 504 级联：
1. `trim_tool_result_for_session` — 持久化前截断 >2KB 工具结果
2. `truncate_session_file` — 压缩成功后将 JSONL 截断至内存历史长度
3. `repair_session_messages` — 启动加载时清除孤立 tool_result 和空消息

### 上游观察关键词
```
trim_tool_result | truncate_session | repair_session | session.*bloat | prune.*session
```

### 等价性标准（必须全部满足才可删除）
- [ ] 上游有工具结果持久化前的截断（不仅是内存中的 trim）
- [ ] 截断阈值可配置或与我们的 2KB 接近
- [ ] 上游有压缩后同步 JSONL 文件的逻辑（而不仅仅是内存）
- [ ] 上游有 session 加载时的 repair/self-healing

### 删除步骤
```bash
rm src/one2x/session_hygiene.rs
# channels/mod.rs: 删除 3 个 cfg(one2x) 块（行号见 Upstream Integration Points）
# channels/session_store.rs: session_path 改回 fn（如果上游恢复了私有性）
# src/one2x/mod.rs: 删除 pub mod session_hygiene;
# dev/UPSTREAM-SYNC-SOP.md: 更新 Architecture 表格
```

---

## F-02: Multi-Stage Compaction

**状态**: 保留中  
**我们的文件**: `src/one2x/compaction.rs`  
**上游文件改动**: `agent/context_compressor.rs` (+1 cfg hook)

### 功能说明
替换上游单次 `compress_once` 为分块多阶段压缩：
- 将中间段按 20 条消息分块，每块独立摘要
- Merge 阶段将所有块摘要合并为一个最终摘要
- Quality Check 阶段验证最新用户请求在摘要中是否可寻址

### 上游观察关键词
```
multi.stage.*compress | chunk.*compress | compress.*chunk | chunked.*compaction
merge.*summary | quality.*check.*compress
```

### 等价性标准（必须全部满足才可删除）
- [ ] 上游对长对话使用分块摘要而不是单次全量压缩
- [ ] 上游有质量验证步骤（压缩后检查最新问题是否可回答）
- [ ] 上游的分块压缩覆盖 channel 路径（不仅是 agent 路径）

### 删除步骤
```bash
rm src/one2x/compaction.rs
# agent/context_compressor.rs: 删除 cfg(one2x) 的 try_multi_stage_compress 调用块
# src/one2x/mod.rs: 删除 pub mod compaction;
```

---

## F-03: Planning Detection + Fast Approval

**状态**: 保留中（已实现，已接线至 loop_.rs 和 channels/mod.rs）  
**我们的文件**: `src/one2x/agent_hooks.rs`  
**上游文件改动**: `agent/loop_.rs` (+planning hook), `channels/mod.rs` (+fast approval hook)

### 功能说明
两个独立的 agent 行为钩子：
1. `check_planning_without_execution` — 检测 LLM 只描述计划不执行的响应，注入「立即执行」nudge
2. `detect_fast_approval` — 识别用户的简短确认（"ok","好的","执行"），替换为优化指令减少一次往返

### 上游观察关键词
```
planning.*without.*execution | execution.*nudge | plan.*detect
fast.*approval | approval.*detect | short.*confirm
```

### 等价性标准（必须全部满足才可删除）
- [ ] 上游有检测 planning-only 响应并注入 nudge 的逻辑
- [ ] 上游覆盖 channel 路径（不仅是 CLI/TUI 路径）
- [ ] 上游的快速确认优化包含中文常用词（"好","可以","执行"等）

### 删除步骤
```bash
rm src/one2x/agent_hooks.rs
# agent/loop_.rs: 删除 planning_nudge_used 变量 + cfg(one2x) 检查块
# channels/mod.rs: 删除 fast_approval cfg(one2x) 块
# src/one2x/mod.rs: 删除 pub mod agent_hooks;
```

---

## F-04: Web Channel (WebSocket)

**状态**: 保留中（One2X 专属功能，不期望上游采纳）  
**我们的文件**: `src/one2x/web_channel.rs`, `src/one2x/gateway_ext.rs`, `src/one2x/config.rs`  
**上游文件改动**: `config/schema.rs` (+cfg web field), `gateway/mod.rs` (+extend_router), `channels/mod.rs` (+extend_channels), `cron/scheduler.rs` (+cfg arm)

### 功能说明
为 videoclaw 前端提供 WebSocket 实时 agent 通道。
上游使用 Telegram/Slack/Matrix 等标准 channel；我们需要 WebSocket 直连。

### 上游观察关键词
```
websocket.*channel | ws.*channel | web.*channel | browser.*channel
```

### 等价性标准
- [ ] 上游提供原生 WebSocket channel，支持前端直连，API 接口与我们相同

_注：这是业务专属功能，通常不会被上游采纳。_

### 删除步骤
```bash
rm src/one2x/web_channel.rs src/one2x/gateway_ext.rs src/one2x/config.rs
# config/schema.rs: 删除 cfg-gated web field
# gateway/mod.rs: 删除 extend_router() 调用
# channels/mod.rs: 删除 extend_channels() 调用
# cron/scheduler.rs: 删除 cfg-gated "web" arm
```

---

## F-05: Agent SSE Endpoint

**状态**: 保留中（One2X 专属功能）  
**我们的文件**: `src/one2x/agent_sse.rs`  
**上游文件改动**: `gateway/mod.rs` (+extend_router 中注册)

### 功能说明
HTTP SSE（Server-Sent Events）Agent 端点，供 videoclaw 服务端以 HTTP 流的形式驱动 agent 执行。
与 Web Channel 配合使用：WebSocket 用于实时交互，SSE 用于后台任务推送。

### 上游观察关键词
```
sse.*agent | agent.*sse | server.sent.*events.*agent | http.*stream.*agent
```

### 等价性标准
- [ ] 上游提供原生 SSE 端点，支持同等的 session 管理和 memory 注入能力

_注：这是业务专属功能。_

### 删除步骤
```bash
rm src/one2x/agent_sse.rs
# gateway/mod.rs: 删除 extend_router() 中的 agent_sse 路由注册
```

---

## F-06: Memory `list_by_prefix`

**状态**: 保留中（可考虑贡献回上游）  
**上游文件改动**: `memory/traits.rs` (+default method), `memory/sqlite.rs` (+impl), `gateway/api.rs` (+两个端点)

### 功能说明
Memory trait 扩展方法，按 key 前缀批量列举记忆条目。
上游 Memory API 只有 get/store/search，缺少按前缀枚举（用于 agent 构建上下文摘要）。

### 上游观察关键词
```
list_by_prefix | memory.*list | prefix.*memory | enumerate.*memor
```

### 等价性标准
- [ ] 上游 Memory trait 有按前缀或标签批量列举的方法
- [ ] SQLite 实现包含该方法

### 删除步骤
```bash
# memory/traits.rs: 删除 list_by_prefix default method
# memory/sqlite.rs: 删除 list_by_prefix SQLite impl
# gateway/api.rs: 删除两个相关端点
```

---

## F-07: Shell `SESSION_ID` Environment Variable

**状态**: 保留中  
**上游文件改动**: `tools/shell.rs` (~9 行)

### 功能说明
在 shell 工具执行时注入 `ZEROCLAW_SESSION_ID` 环境变量，使 shell 脚本能感知当前 session。
用于 videoclaw skill 脚本中的 session 隔离和日志关联。

### 上游观察关键词
```
SESSION_ID.*env | ZEROCLAW_SESSION | session.*id.*shell | shell.*session.*env
```

### 等价性标准
- [ ] 上游在 shell 工具中注入 session 标识符环境变量

### 删除步骤
```bash
# tools/shell.rs: 删除 cfg(one2x) 的 SESSION_ID 注入块（约 9 行）
```

---

## F-08: Heartbeat Lark/Feishu Validation

**状态**: 保留中  
**上游文件改动**: `daemon/mod.rs` (~22 行)

### 功能说明
Heartbeat 系统在检测到 Lark/Feishu channel 配置时的扩展验证。
上游已采纳 Lark delivery（`feat: add Lark/Feishu delivery`），但 heartbeat 的 Lark 验证逻辑未被采纳。

### 上游观察关键词
```
heartbeat.*lark | heartbeat.*feishu | lark.*heartbeat | feishu.*heartbeat | daemon.*lark.*valid
```

### 等价性标准
- [ ] 上游 heartbeat 系统对 Lark/Feishu channel 有配置验证

### 删除步骤
```bash
# daemon/mod.rs: 删除 cfg(one2x) 的 lark/feishu heartbeat 验证块（约 22 行）
```

---

## F-09: Stream Idle Timeout (N1)

**状态**: 保留中（改动在上游文件，无 feature flag — 属于全局改进）  
**上游文件改动**: `providers/reliable.rs` — `stream_chat`、`stream_chat_with_system`、`stream_chat_with_history` 三个函数的 tokio::spawn 内，将 `stream.next().await` 替换为带 60s 超时的 loop  
**对应上游实现**:
- openclaw: `src/agents/pi-embedded-runner/run/llm-idle-timeout.ts` — `DEFAULT_LLM_IDLE_TIMEOUT_MS = 60_000`，全局生效，无 flag
- claude-code: `src/services/api/claude.ts:1868` — `STREAM_IDLE_TIMEOUT_MS = 90_000`，需要 `CLAUDE_ENABLE_STREAM_WATCHDOG=true`

### 功能说明
当 LLM 流式响应卡住（每个 token 超过 60s 未到达），主动关闭连接并发送错误。
错误触发 `loop_.rs` 里已有的 streaming fallback（`Err(stream_err)` → 降级到非流式 chat）。
**注意**：已覆盖全部三条 `stream_chat` 路径（`stream_chat`、`stream_chat_with_system`、`stream_chat_with_history`）。

### 上游观察关键词
```
idle.*timeout | stream.*timeout | per.*token.*timeout | STREAM_IDLE_TIMEOUT
```

### 等价性标准（满足即可删除）
- [ ] 上游 `reliable.rs` 有 per-token 超时保护（任意形式）
- [ ] 超时值 ≤ 60s 或可配置
- [ ] 覆盖 `stream_chat` 主路径

### 删除步骤
```bash
# providers/reliable.rs: 删除 STREAM_IDLE_TIMEOUT_SECS 常量
# providers/reliable.rs: 将三个 spawn 内的 loop { timeout(...) } 还原为 while let Some(x) = stream.next().await
```

---

## F-10: Compaction Context Window Floor (N2)

**状态**: 保留中（改动在上游文件，无 feature flag — 属于全局防御性改进）  
**上游文件改动**: `agent/context_compressor.rs` — `ContextCompressor::new()` 应用 `context_window.max(MIN_CONTEXT_WINDOW_FLOOR)`  
**对应上游实现**:
- openclaw: `src/agents/pi-settings.ts:4` — `DEFAULT_PI_COMPACTION_RESERVE_TOKENS_FLOOR = 20_000`，应用在 `reserveTokens` 参数（语义略不同：保护 tail token 数，而非总窗口下限）
- claude-code: `src/services/compact/autoCompact.ts:30` — `MAX_OUTPUT_TOKENS_FOR_SUMMARY = 20_000`，应用在输出空间预留（语义再次不同）

### 功能说明
防止 `context_window` 被错误配置为极小值（如 0 或 1000），导致 compaction threshold 接近 0、每轮都触发压缩。
`MIN_CONTEXT_WINDOW_FLOOR = 20_000` 与 openclaw 的值对齐，但应用点不同（总窗口 vs. 尾部 reserve）。

### 上游观察关键词
```
context_window.*floor | context_window.*min | MIN_CONTEXT | compaction.*floor | reserve.*tokens.*floor
```

### 等价性标准（满足即可删除）
- [ ] 上游 `ContextCompressor::new()` 或等效位置对 `context_window` 有最小值保护

### 删除步骤
```bash
# agent/context_compressor.rs: 删除 MIN_CONTEXT_WINDOW_FLOOR 常量
# agent/context_compressor.rs: ContextCompressor::new() 中 context_window 恢复直接赋值
```

---

## F-11: Case-Insensitive Tool Name Lookup (N3)

**状态**: 保留中（改动在上游文件，无 feature flag）  
**上游文件改动**: `agent/tool_execution.rs` — `find_tool()` 新增大小写不敏感 fallback  
**对应上游实现**:
- openclaw: `src/agents/pi-embedded-runner/run/attempt.tool-call-normalization.ts` — 多级标准化 pipeline，在**流层面**提前处理（先 exact → lowercase+alias → case-insensitive scan → 结构化 → ID 推断）
- claude-code: `src/Tool.ts:355` — **没有大小写不敏感匹配**，只有 exact + `aliases[]`

### 功能说明
兼容部分 OpenAI-compatible 提供商会把 tool name 大小写改变后返回的情况。
注意：我们的实现在 **dispatch 层**（`find_tool` 调用时），openclaw 在**流处理层**（stream wrapper 里）。
效果相同，但 openclaw 的方案更早捕获问题（还能处理 alias、命名空间等更复杂情形）。

### 上游观察关键词
```
case.*insensitive.*tool | toLowerCase.*tool | tool.*normalize | tool.*alias | find_tool.*lower
```

### 等价性标准（满足即可删除）
- [ ] 上游 `find_tool` 或流处理层有大小写不敏感的 tool name 匹配
- [ ] 覆盖 `dispatch` 路径（不仅是 stream 层的标准化）

### 删除步骤
```bash
# agent/tool_execution.rs: 删除 find_tool 里的 case-insensitive fallback 段（约 5 行）
# agent/tool_execution.rs: 删除文档注释中的 openclaw 引用
```

---

## F-12: Full Mid-History Tool Pairing Repair

**状态**: 保留中（`#[cfg(feature = "one2x")]` — 改动在 one2x 文件 + loop_.rs hook）  
**我们的文件**: `src/one2x/session_hygiene.rs` — `repair_full_tool_pairing()`  
**上游文件改动**: `agent/loop_.rs` — 在 `prepare_messages_for_provider` 前添加 cfg-gated hook

### 功能说明
扩展 `ensure_tool_result_pairing`（仅修复开头/结尾/summary 后的孤立 tool 消息）：
1. 扫描**全部历史**，删除任何中间位置的孤立 `tool` 消息（前一条不是 assistant/tool）
2. 对 native-mode assistant JSON 消息，检查每个 `tool_calls[].id` 是否有对应 `tool` 消息中的 `tool_call_id`，缺失则插入合成 error result

### 上游观察条件
```
repair.*tool.*pairing.*full | full.*tool.*pairing | mid.*history.*orphan | synthetic.*tool.*result
```

### 等价性标准（满足即可删除）
- [ ] 上游扫描全部历史（不仅开头/结尾/summary 边界）修复孤立 tool 消息
- [ ] 上游为缺失 tool_call 结果插入合成 error 条目

### 删除步骤
```bash
# src/one2x/session_hygiene.rs: 删除 repair_full_tool_pairing() 函数
# agent/loop_.rs: 删除 cfg(one2x) 的 repair_full_tool_pairing + limit_tool_result_sizes 块
# src/one2x/mod.rs: 更新 Upstream Integration Points 表格
```

---

## F-13: Pre-LLM Tool Result Size Guard

**状态**: 保留中（`#[cfg(feature = "one2x")]` — 与 F-12 共用同一个 loop_.rs hook 块）  
**我们的文件**: `src/one2x/session_hygiene.rs` — `limit_tool_result_sizes()`  
**上游文件改动**: `agent/loop_.rs` — 与 F-12 同一个 cfg-gated block

### 功能说明
在每次 LLM 调用**之前**（无条件）将超大 tool 结果截断至 20,000 字符。
不同于上游的 `fast_trim_tool_results`（仅在 budget breach 时触发）。
防止单条 tool 结果占用超过 50% context window。

### 上游观察条件
```
limit.*tool.*result.*size | pre.*llm.*tool.*trim | unconditional.*tool.*trim | tool.*result.*guard
```

### 等价性标准（满足即可删除）
- [ ] 上游在每次 LLM 调用前无条件截断过大 tool 结果（不仅在 budget breach 时）

### 删除步骤
```bash
# src/one2x/session_hygiene.rs: 删除 limit_tool_result_sizes() 函数
# agent/loop_.rs: 同 F-12 删除块（共用）
```

---

## F-14: Pre-Compaction Key-Facts Memory Flush

**状态**: 保留中（改动在 one2x 文件，`#[cfg(feature = "one2x")]` 内）  
**我们的文件**: `src/one2x/compaction.rs` — `try_multi_stage_compress()` 内新增 pre-compaction 阶段  
**对应上游实现**:
- openclaw: `src/agents/pi-embedded-runner/run/memory/memory-flush.ts` — 压缩前单独 LLM turn 提取 key facts，写入 `memory/YYYY-MM-DD.md`

### 功能说明
压缩开始**之前**，对即将被丢弃的中间历史段运行专项 LLM 提取，将 key facts 持久化到
`memory/key_facts_{YYYY-MM-DD}` 条目（`MemoryCategory::Daily`）。
即使后续压缩失败，已提取的 facts 也已安全持久化，可在未来 session 通过记忆检索获取。

### 上游观察条件
```
pre.*compact.*memory | memory.*flush.*compress | key.*facts.*extract | before.*compaction.*store
```

### 等价性标准（满足即可删除）
- [ ] 上游在压缩前运行专项 LLM 提取 key facts 并持久化到带日期标识的记忆条目

### 删除步骤
```bash
# src/one2x/compaction.rs: 删除 KEY_FACTS_EXTRACTOR_SYSTEM 常量
# src/one2x/compaction.rs: 删除 try_multi_stage_compress 中 "Pre-compaction key-facts flush" 块（约 45 行）
```

---

## F-15: Retry Jitter in ReliableProvider Backoff

**状态**: 保留中（改动在上游文件，无 feature flag — 防御性改进）  
**上游文件改动**: `providers/reliable.rs` — `compute_backoff()` 新增 ±10% jitter  
**对应上游实现**:
- openclaw: `src/agents/pi-embedded-runner/run/llm-retry.ts` — `jitter: 0.1` 系数

### 功能说明
在指数退避基础上增加 ±10% 随机 jitter，避免多个客户端/API keys 同时触发速率限制后
同步重试形成 retry storm。
覆盖所有四条 `chat*` 路径（`compute_backoff` 是统一入口）。

### 上游观察条件
```
jitter.*backoff | backoff.*jitter | retry.*jitter | random.*backoff
```

### 等价性标准（满足即可删除）
- [ ] 上游 `compute_backoff` 或等效位置有随机 jitter（任意系数）

### 删除步骤
```bash
# providers/reliable.rs: 删除 compute_backoff 中的 jitter 计算（3 行）
# providers/reliable.rs: 删除顶部 use rand::RngExt as _; import
```
