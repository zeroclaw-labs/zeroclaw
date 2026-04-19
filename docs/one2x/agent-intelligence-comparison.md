# Hermes vs ZeroClaw vs OpenClaw：Agent Intelligence 全面对比分析

> 最后更新：2026-04-18  
> 作者：研究所 Researcher  
> 源码阅读量：~4000 行（Hermes Python + ZeroClaw Rust + OpenClaw TS）

---

## 速查表：三者一句话定位

| 维度 | Hermes | ZeroClaw | OpenClaw |
|------|--------|----------|----------|
| **定位** | RL 训练 + 生产 Agent，Python，OpenAI-spec | 高性能 Rust Agent Runtime，One2X 定制版 | 多渠道个人助理平台，TypeScript，插件生态 |
| **Agent Loop** | async + ThreadPoolExecutor(128)，标准 tool_calls | Tokio async，7-dim context，CancellationToken | 事件驱动，插件 hook 点丰富 |
| **Memory** | SQLite 短期 + MemoryProvider 插件 | 多后端记忆（Sqlite/Markdown/Qdrant/Lucid/None）+ 可选 embedding | LanceDB + active-memory 插件，autoCapture |
| **Compaction** | 4-phase 结构化摘要，anti-thrashing，focus_topic | Multi-stage chunked + quality check + key-facts flush | 预防式 compaction，compaction hooks，post-compaction AGENTS.md 注入 |
| **错误处理** | FailoverReason 枚举（14 种），jittered backoff，credential rotation | MAX_OVERFLOW_RECOVERY_ATTEMPTS=3，emergency_history_trim | 插件级 retry，compaction safety timeout |
| **Skills** | `~/.hermes/skills/`，SKILL.md 必需，agentskills.io 标准 | SkillForge 自动发现（GitHub/ClawHub/HuggingFace），sandbox verify | 100+ 内置 skills，SKILL.md 体系，clawhub 集成 |
| **SOUL/身份** | SOUL.md 详尽（grill-me 模式、TDD、autoresearch loop）| `workspace/SOUL.md` 运行时注入 | SOUL.md + AGENTS.md + IDENTITY.md 三层身份 |
| **多渠道** | 无（单 CLI/API 模式） | zeroclaw-channels crate，channel-based approval | 104 个扩展（feishu/telegram/discord/matrix/whatsapp 等）|
| **安全** | ACP permissions，_approval_lock threading.Lock | AutonomyLevel 三级，Sandbox（Docker/Firejail），LeakDetector | DM security，approveChannelId，pairing guard |
| **可观测性** | InsightsEngine（SQLite cost/token 分析），tracing.log | OpenTelemetry + Prometheus，HeartbeatMetrics EMA | HeartbeatRunner，DiagnosticHeartbeatEvent，LCM 压缩历史 |

---

## 第一节：架构总览

### Hermes：Python ML Training First

**结论：Hermes 是为强化学习训练场景设计的 Agent 框架，生产部署是次要考量。**

架构层次（源码：`~/.hermes/hermes-agent/`）：
- **入口**：`cli.py`（交互式）/ `batch_runner.py`（RL 训练批处理）/ `acp_adapter/server.py`（ACP 协议适配）
- **核心**：`environments/agent_loop.py`（HermesAgentLoop，534 行）
- **工具层**：`model_tools.py` 的 `handle_function_call()`，同步工具在 ThreadPoolExecutor 中运行
- **记忆层**：`agent/memory_manager.py` + `agent/memory_provider.py`（插件式）
- **部署模式**：本地 CLI / Docker / Modal / SSH / Daytona / Singularity（`terminal_tool.py`）

关键设计决策：工具线程池 `_tool_executor = ThreadPoolExecutor(max_workers=128)`（`agent_loop.py:30`），这个 128 是为了 RL 批处理场景——89 个 TB2 任务并发时不想 starvation。普通用户根本用不着 128 个线程。这是"先优化训练，后优化日用"的典型体现。

**弱点**：没有多渠道支持，不是一个"bot 平台"；部署复杂度高（多 backend）；没有内置 observability metrics。

### ZeroClaw：Rust One2X 定制版

**结论：ZeroClaw 是一个工程品质最高的 Agent Runtime，34 commits 的定制都集中在"让 Agent 更可靠地完成任务"。**

Crate 结构（`~/projects/tools/zeroclaw/`，branch `one2x/custom-v7`）：
```
zeroclaw-runtime/  ← 核心 (loop_.rs = 7593 行)
zeroclaw-memory/   ← 多后端记忆（Sqlite/Markdown/Qdrant/Lucid/None）
zeroclaw-providers/ ← LLM provider 抽象
zeroclaw-channels/ ← 多渠道支持
zeroclaw-config/   ← 配置 schema
zeroclaw-tools/    ← 工具注册
zeroclaw-gateway/  ← HTTP gateway
zeroclaw-skillforge/ ← 自动 Skill 发现
zeroclaw-tui/      ← 终端 UI
zeroclaw-security/ ← 安全子系统（Sandbox/PairingGuard/LeakDetector）
zeroclaw-observability/ ← OTel/Prometheus
```

One2X 定制不是只集中在一个目录，而是按 crate 边界分布：
- `crates/zeroclaw-runtime/src/one2x/compaction.rs`（400 行）：多阶段压缩
- `crates/zeroclaw-runtime/src/one2x/mod.rs`：pre-LLM hygiene + planning detection
- `crates/zeroclaw-channels/src/one2x.rs`：session hygiene + channel-side tool pairing + fast approval
- `src/one2x/`：Web channel / Agent SSE / root-crate wiring

**弱点**：Rust 编译门槛高，修改核心逻辑需要重新编译；没有 Python/JS 生态的直接集成；Skill 系统仍在成熟中。

### OpenClaw：TypeScript 插件生态

**结论：OpenClaw 是三者中功能覆盖最广、部署最简单的，但源码分析受限于打包产物（dist/）。**

架构（`~/.npm-global/lib/node_modules/openclaw/dist/`）：
- **核心**：事件驱动，AgentEventStream 处理多种事件类型（`lifecycle|tool|assistant|error|item|plan|approval|command_output|patch|compaction|thinking`）
- **插件系统**：104 个扩展（channel plugins + provider plugins + memory plugins）
- **运行时 workspace**：`~/.openclaw/workspace/`，包含 SOUL.md/AGENTS.md/MEMORY.md/HEARTBEAT.md
- **LCM（Lossless Context Management）**：分层 DAG 压缩摘要，sum_xxx 节点可展开

关键文件（dist 层）：
- `active-memory/index.js`：`DEFAULT_TIMEOUT_MS=15s`，`DEFAULT_QUERY_MODE="recent"`
- `preemptive-compaction.types.js`：`shouldPreemptivelyCompactBeforePrompt()`
- `post-compaction-context.d.ts`：压缩后注入 AGENTS.md 内容

**弱点**：dist 打包产物调试困难，难以精确追踪逻辑；TypeScript 运行时性能低于 Rust；plugin 生态碎片化，依赖版本管理复杂。

---

## 第二节：Agent Loop 设计

### Hermes：简洁的异步 while 循环

**核心**：`environments/agent_loop.py` 的 `HermesAgentLoop.run()`，标准 OpenAI tool-calling 循环。

```python
# agent_loop.py:30 - 工具线程池
_tool_executor = concurrent.futures.ThreadPoolExecutor(max_workers=128)

# AgentResult dataclass - 循环出口
@dataclass
class AgentResult:
    messages: List[Dict[str, Any]]       # 完整对话历史
    managed_state: Optional[Dict]        # Phase 2 服务器状态
    turns_used: int                      # LLM 调用次数
    finished_naturally: bool             # 是否自然结束（vs 超限）
    reasoning_per_turn: List[Optional[str]]  # 每轮 CoT 内容
    tool_errors: List[ToolError]         # 工具错误记录
```

**Phase 1 vs Phase 2**：
- Phase 1：标准 OpenAI spec，检查 `response.choices[0].message.tool_calls`
- Phase 2：ManagedServer + 客户端 tool call 解析，fallback 处理 raw `<tool_call>` 标签

支持多种 reasoning 格式（`reasoning_content` / `reasoning` / `reasoning_details[].text`），可见兼容了 DeepSeek/Claude/OpenAI 等不同格式。

**并发**：工具调用在 ThreadPoolExecutor 中运行（为了避免 asyncio 死锁），Max workers=128 可运行时调整（`resize_tool_pool()`）。

**弱点**：没有 streaming 支持（每次等完整响应）；没有 CancellationToken；tool 并发度取决于线程池而非 async task。

### ZeroClaw：工程最成熟的 Agent Loop

**核心**：`crates/zeroclaw-runtime/src/agent/loop_.rs`（7593 行，最大的单文件）

关键常量（`loop_.rs`）：
```rust
const STREAM_CHUNK_MIN_CHARS: usize = 80;
const STREAM_TOOL_MARKER_WINDOW_CHARS: usize = 512;
const DEFAULT_MAX_TOOL_ITERATIONS: usize = 10;
const AUTOSAVE_MIN_MESSAGE_CHARS: usize = 20;
```

**One2X 定制 hook 点**（`one2x/mod.rs`，每次 LLM 调用前执行）：
```rust
// 每次 LLM call 前的三步清理
repair_full_tool_pairing(history);           // 修复孤儿 tool_call/result
micro_compact_old_tool_results(history);     // 清除 3 个 user turn 之前的 tool results
limit_tool_result_sizes_with_budget(history, context_window);  // 动态 cap tool result 大小
check_planning_without_execution(messages);  // 检测只规划不执行，注入执行 nudge
```

**check_planning_without_execution** 是最有趣的 hook：检查最后一条 assistant 消息是否包含 18 个"规划短语"（"i will", "step 1:", "here's my plan" 等）但不包含任何执行指示符（"```", "tool_use", "done." 等）。如果是，注入 `"Do not describe what you will do — execute it now."` —— 直接解决了"Agent 说话不做事"问题。

**模型运行时切换**（`loop_.rs`）：
```rust
static MODEL_SWITCH_REQUEST: LazyLock<Arc<Mutex<Option<(String, String)>>>> = ...
```
通过 `model_switch` tool 可以在 agent loop 运行中途切换 provider/model，这是 Hermes 和 OpenClaw 都没有的功能。

**MCP tool 过滤**（`filter_tool_specs_for_turn()`）：内置工具永远传递，MCP 工具按 "always"（无条件）或 "dynamic"（关键词匹配用户消息）分组过滤，防止 tool 列表无限膨胀。

**视觉/多模态**：独立的 `vision_provider_box`，`prepare_messages_for_provider()` 处理多模态消息转换。

**CancellationToken**（tokio_util）：完整的取消机制，优雅中断长任务。

**三级 AutonomyLevel**：
- `ReadOnly`：只读观察
- `Supervised`（默认）：高风险操作需审批
- `Full`：在 policy 范围内全自主

### OpenClaw：事件驱动 + 插件钩子

**核心**：事件流 (`AgentEventStream`) + approval flow。ApprovalEventKind：`exec|plugin|unknown`。

**Preemptive Compaction**（`preemptive-compaction.types.js`）：
```typescript
shouldPreemptivelyCompactBeforePrompt()
estimatePrePromptTokens()
```
在 prompt 之前评估是否需要压缩，是三者中最"主动"的压缩策略。

**HeartbeatReasonKind**：`retry|interval|manual|exec-event|wake|cron|hook|other`，Heartbeat 触发原因追踪。

| | Hermes | ZeroClaw | OpenClaw |
|---|---|---|---|
| **流式输出** | ❌ | ✅ (STREAM_CHUNK_MIN_CHARS=80) | ✅ |
| **取消机制** | ❌ | ✅ CancellationToken | ✅ AbortController |
| **并发工具** | ✅ ThreadPool | 单线程顺序 | ✅ Promise.all |
| **模型运行时切换** | ❌ | ✅ model_switch tool | ❌ |
| **执行强制 nudge** | ❌ | ✅ check_planning_without_execution | ❌ |
| **approval flow** | ✅ threading.Lock | ✅ ApprovalManager | ✅ channel-based |

**最佳**：ZeroClaw——流式输出 + 取消机制 + 多项 One2X 定制 hook 是生产级工程的体现。

---

## 第三节：Memory 系统

### Hermes：轻量插件式

**短期记忆**：对话历史 `messages` 列表（Python dict），直接在内存中。

**长期记忆**：`agent/memory_manager.py`
- 内置 provider 始终第一
- 最多一个外部插件 provider（第二个被拒绝并警告）
- `sanitize_context()`：清除 provider 输出中的 fence tags 和注入尝试
- `build_memory_context_block()`：包裹在 `<memory-context>` 标签中

**RL 环境限制**：per-loop TodoStore（临时），`memory/session_search` 在 RL 环境中被禁用（防止 agent 通过记忆系统"作弊"）。

**InsightsEngine**（`agent/insights.py`）：从 SQLite 分析 token/cost/tool 使用，用 `usage_pricing.py` 估算成本——这是三者中唯一有内置成本分析的。

### ZeroClaw：最丰富的记忆架构

`crates/zeroclaw-memory/` 模块列表：
```
audit, backend, chunker, conflict, decay, embeddings, hygiene, 
importance, knowledge_graph, lucid, markdown, namespaced, none, 
policy, qdrant, response_cache, retrieval, snapshot, sqlite, traits, vector
```

**多后端**（`backend/`）：
- `Sqlite`：本地快速
- `Lucid`：sqlite + 本地，带特殊索引
- `Qdrant`：生产向量存储，fallback to MarkdownMemory
- `Markdown`：纯文件，调试用
- `None`：禁用

**时间衰减**（`memory/decay.rs`）：
```rust
// core memories 永不衰减（evergreen）
// 普通记忆：指数衰减 score * 2^(-age_days / half_life)
const DEFAULT_HALF_LIFE_DAYS: f64 = 7.0;
```
这个设计参考了人类记忆曲线——最近的记忆权重高，7天后权重减半。Core memories（用户明确设置的重要记忆）不受衰减影响。

**Compaction 前 key-facts flush**（`crates/zeroclaw-runtime/src/agent/context_compressor.rs`）：
```rust
// 压缩前提取关键事实存到当前 memory backend
// KEY_FACTS_EXTRACTOR_SYSTEM 提取 UUIDs/tokens/决策/配置值
// 存到 MemoryCategory::Daily，key = "key_facts_YYYY-MM-DD"
```
这保证了即使对话被压缩，关键标识符也持久化在记忆系统中。

### OpenClaw：LanceDB + active-memory 插件

**active-memory**（`extensions/active-memory/index.js`）：
```javascript
const DEFAULT_TIMEOUT_MS = 15000;      // 15秒 recall 超时
const DEFAULT_QUERY_MODE = "recent";   // 默认近期查询模式
// NO_RECALL_VALUES 黑名单
// RECALLED_CONTEXT_LINE_PATTERNS 格式检测
// 每 session 异步锁
```

**LCM（Lossless Context Management）**：这是 OpenClaw 独有的设计——DAG 摘要树，每个 sum_xxx 节点可展开（lcm_expand/lcm_expand_query）。这不是"有损压缩"，而是"可查询的分层摘要"，信息理论上不丢失（只是需要展开查询）。

**memory-lancedb**：OpenAI embeddings（`text-embedding-3-small`），自动 capture/recall（`autoCapture`/`autoRecall` 标志），`dreaming` 配置（后台定期整理记忆）。

| | Hermes | ZeroClaw | OpenClaw |
|---|---|---|---|
| **向量存储** | 插件化 | Sqlite embedding BLOB / Qdrant / Lucid | LanceDB |
| **时间衰减** | ❌ | ✅ (half_life=7天) | ❌ |
| **记忆去重** | ❌ | ✅ conflict模块 | ✅ SHA-256 hash |
| **成本追踪** | ✅ InsightsEngine | ✅ cost tracking | ❌ |
| **后台整理** | ❌ | ❌ | ✅ dreaming |
| **知识图谱** | ❌ | ✅ knowledge_graph | ❌ |

**最佳**：ZeroClaw——架构最完整，时间衰减 + 知识图谱 + 多后端是三者中最接近"真实记忆系统"的设计。

---

## 第四节：Context Compaction

### Hermes：最精细的压缩控制

**结论：Hermes 的 ContextCompressor 是三者中对压缩细节考虑最深入的，但没有 ZeroClaw 的分布式质量保证。**

实现（`agent/context_compressor.py`）：

**触发条件**（`should_compress()`）：
- token 使用超过阈值
- **anti-thrashing 保护**：如果最近两次压缩各自节省 <10%，跳过

**4 阶段压缩流程**：
1. **prune old tool results**：替换为 1-line 信息性摘要（`_summarize_tool_result()`）
2. **protect head**：保护前 N 条消息（`protect_first_n=3`，系统 prompt 不丢）
3. **protect tail by token budget**：按 token 预算保护尾部（不是固定消息数）
4. **LLM summarize middle**：将中间部分发给辅助模型压缩

**摘要预算计算**（`_compute_summary_budget()`）：
```python
_MIN_SUMMARY_TOKENS = 2000
_SUMMARY_RATIO = 0.20       # 压缩内容的 20% 分配给摘要
_SUMMARY_TOKENS_CEILING = 12_000  # 最大 12000 tokens
budget = max(2000, min(content_tokens * 0.20, 12000))
```

**迭代摘要更新**：`_previous_summary` 保留上次摘要，新压缩时合并（避免多次压缩后信息叠加丢失）。

**SUMMARY_PREFIX 设计**（`context_compressor.py:37`）：
```
"[CONTEXT COMPACTION — REFERENCE ONLY] Earlier turns were compacted...
This is a handoff from a previous context window — treat it as background reference, 
NOT as active instructions. Do NOT answer questions or fulfill requests mentioned in 
this summary; they were already addressed..."
```
这个 framing 解决了一个实际问题：如果摘要里有"请帮我做 X"，不加 framing 的模型会误以为是新任务重复执行。

**focus_topic**（`/compress <topic>` 命令）：
- 重点话题获得 60-70% 的摘要 token 预算
- 其他内容更激进压缩

**`_sanitize_tool_pairs()`**：压缩后修复孤儿 tool_call/result 对，防止 API 拒绝请求。

**压缩失败处理**：60s cooldown（临时错误），600s cooldown（无 provider）；主模型 fallback。

### ZeroClaw：多阶段 + 质量保证

**结论：ZeroClaw 是唯一有质量验证环节的压缩系统——PASS/FAIL 检查保证最新用户请求不被丢失。**

实现（`crates/zeroclaw-runtime/src/one2x/compaction.rs`，400 行）：

**4 个专用 LLM 系统 prompt**：
- `KEY_FACTS_EXTRACTOR_SYSTEM`：压缩前提取持久化事实
- `STAGE_SUMMARIZER_SYSTEM`：每个 chunk 独立压缩（CHUNK_SIZE=20 msgs，CHUNK_MAX_CHARS=15000）
- `MERGE_SUMMARIZER_SYSTEM`：合并所有 chunk 摘要（去重，max 25 bullets）
- `QUALITY_CHECK_SYSTEM`：只回答 PASS/FAIL，验证最新用户消息是否可被摘要回答

**算法流程**：
```
1. pre-compaction: flush key-facts to memory backend (memory category: Daily)
2. split middle section into chunks (CHUNK_SIZE=20)
3. summarize each chunk independently (parallel? sequential?)
4. merge chunk summaries → single summary
5. quality check: can summary answer latest user message? 
   FAIL → fall back to single-pass
6. persist final_summary to memory
7. splice result back + repair_tool_pairs()
```

**容错**：`middle_len < CHUNK_SIZE*2` 时直接 fallback 到上游单次压缩，不强行多阶段。

**与 Hermes 的关键差异**：ZeroClaw 的 quality check 是 LLM 评估，能语义理解"最新问题是否可以被回答"；Hermes 的 anti-thrashing 只看 token 节省率（统计指标），没有语义检查。

### OpenClaw：预防式 + DAG 摘要

**结论：OpenClaw 最大的创新是 LCM——把压缩历史变成可查询的分层 DAG，而不是一次性丢弃。**

关键组件：

**Preemptive Compaction**（`preemptive-compaction.types.js`）：
```typescript
shouldPreemptivelyCompactBeforePrompt()  // 在发送 prompt 前检查
estimatePrePromptTokens()                // 预估 token 使用
PREEMPTIVE_OVERFLOW_ERROR_TEXT           // 溢出文案
```

**Post-Compaction Context Injection**（`post-compaction-context.d.ts`）：
```typescript
readPostCompactionContext()  // 压缩后重新注入 AGENTS.md 内容（带日期替换）
```
这保证了压缩后 AGENTS.md 里的"今天是 XX"类信息不会过期。

**Compaction Hooks**（`compaction-hooks`）：
```typescript
CompactionHookRunner.runBeforeCompaction()   // 压缩前 hook
CompactionHookRunner.runAfterCompaction()    // 压缩后 hook
// metrics: messageCount, tokenCount, sessionFile
```

**LCM**：每个 sum_xxx 节点是一个摘要叶子，可以通过 `lcm_expand_query` 展开，支持跨 session 检索（allConversations=true）。

| | Hermes | ZeroClaw | OpenClaw |
|---|---|---|---|
| **触发方式** | token 阈值 | token 阈值 | preemptive（提前预估）|
| **压缩前保护** | key-facts（摘要结构模板）| key-facts flush to memory backend | AGENTS.md 注入 |
| **质量验证** | anti-thrashing（统计）| PASS/FAIL LLM 检查 | 无 |
| **压缩后可查** | ❌（一次性） | ❌（除非 key-facts 入库）| ✅ LCM DAG |
| **focus topic** | ✅ /compress topic | ❌ | ❌ |
| **迭代摘要** | ✅ _previous_summary | ❌ | ❌ |

**最佳**：并列前两。ZeroClaw 的质量保证最严格；OpenClaw 的 LCM DAG 信息保留最完整；Hermes 的细粒度控制（focus_topic、迭代摘要）最灵活。

---

## 第五节：错误处理 & 自愈

### Hermes：最系统化的错误分类

**结论：Hermes 的 error_classifier.py 是三者中对 LLM API 错误分类最细致的，14 种 FailoverReason 涵盖了生产环境的主要故障模式。**

**FailoverReason 枚举**（`agent/error_classifier.py`）：
```python
auth, auth_permanent, billing, rate_limit, overloaded, 
server_error, timeout, context_overflow, payload_too_large, 
model_not_found, format_error, thinking_signature, 
long_context_tier, unknown
```

**ClassifiedError dataclass**：
```python
reason: FailoverReason
status_code: int
provider: str
model: str
message: str
error_context: dict
# 三个 flag
retryable: bool        # 可以 retry
should_compress: bool   # 触发压缩
should_rotate_credential: bool  # 轮换凭证
should_fallback: bool   # 降级到 fallback model
```

**jittered_backoff**（`agent/retry_utils.py`）：
```python
# 计算公式：min(base * 2^(attempt-1), max_delay) + jitter
# jitter seed: time_ns ^ (tick * 0x9E3779B9)  ← 黄金比例哈希
# monotonic _jitter_counter + threading.Lock
```
用黄金比例哈希的 jitter——这比简单的 random.uniform 更均匀，避免 thundering herd。

**credential rotation**（`agent/credential_pool.py`）：多个 API key 池，触发轮换时换下一个 key。

**rate_limit_tracker + nous_rate_guard**：分别处理 API 速率限制和 Nous 特定的速率保护。

### ZeroClaw：Rust 类型系统保证 + 多层自愈

**context overflow recovery**（`loop_.rs`）：
```rust
const MAX_OVERFLOW_RECOVERY_ATTEMPTS: usize = 3;
// is_context_window_exceeded() 检查点在 3 处
// → emergency_history_trim() 紧急截断
// → trim_history() 正常截断
// → fast_trim_tool_results() 快速清理
```

**repair_full_tool_pairing**（`one2x/mod.rs:12`）：
```rust
// 移除中间历史里的孤儿 tool 消息
// 为缺失的 tool_call ID 插入合成 "[one2x] missing tool result" 
// 这防止了 Anthropic/OpenAI API 因消息格式不合法而拒绝请求
```

**limit_tool_result_sizes_with_budget**（`one2x/mod.rs:131`）：
```rust
const TAIL_CHARS: usize = 2_000;
// dynamic_cap = min(context_window * 0.30 * 4, 20_000)
// head + "[N chars omitted]" + tail 格式
```
动态 cap 而非固定大小——大 context 模型（200K）允许更大的 tool result，小模型（8K）自动压缩。

**STEP_TIMEOUT_MAX_RETRIES = 2**（`one2x/mod.rs:175`）：步骤超时后最多重试 2 次。

**AutonomyLevel + ApprovalManager**：
```rust
// ApprovalManager::from_config(&config.autonomy)
// auto-deny if channel doesn't support approval
// 失败安全：不支持 approval 的 headless channel 自动拒绝危险操作
```

### OpenClaw：插件级隔离 + compaction safety

**compaction safety timeout**（`plugin-sdk`）：
- `compaction-retry-aggregate-timeout`：重试超时上限
- `compaction-safety-timeout`：单次压缩超时
- 超时后使用上次成功的压缩结果（graceful degradation）

**error event stream**：`AgentEventStream` 的 `error` 类型，插件可以订阅并处理。

**approval flow**（`channel-config-helpers.js`）：
```javascript
resolveChatChannelSecurity()  // DM security with approveChannelId/channelKey
```

| | Hermes | ZeroClaw | OpenClaw |
|---|---|---|---|
| **错误分类** | ✅ 14种 FailoverReason | Rust Result<T,E> 类型系统 | 插件级 |
| **jitter backoff** | ✅ 黄金比例 seed | Rust tokio + 指数退避 | Node.js setTimeout |
| **credential rotation** | ✅ credential_pool | ❌ | ❌ |
| **tool pair 修复** | ✅ _sanitize_tool_pairs | ✅ repair_full_tool_pairing | ❌ |
| **context overflow 恢复** | 单次 compress | ✅ 3次 + emergency trim | preemptive 预防 |
| **circuit breaker** | ❌ | ❌ | ❌ |

**最佳**：Hermes（错误分类和 credential rotation 最完整）+ ZeroClaw（Rust 类型安全 + 多层恢复）并列。三者都没有完整的 circuit breaker 实现。

---

## 第六节：自进化 / Skills

### Hermes：人工编写 + auto-improve loop

**Skills 目录**（`~/.hermes/skills/`）：
- `SKILL.md` 必需（兼容 agentskills.io spec）
- YAML frontmatter 字段：`name`（≤64 chars）、`description`（≤1024 chars）、`license`、`compatibility`、`metadata`（任意 KV）、`related_skills`
- **Progressive disclosure** 三层（`tools/skills_tool.py`，1420 行）：
  - tier 1：`skills_list()` 返回 metadata 只（避免 context 爆炸）
  - tier 2：`skill_view("name")` 加载完整内容
  - tier 3：`skill_view("name", "references/file.md")` 加载链接引用

**自动激活**（`SOUL.md`）：不需要用户输入 skill 名，通过意图识别自动触发（23 个触发规则）。

**Autoresearch Loop**（SOUL.md）：发现错误 → 写 `.learnings/LEARNINGS.md`（LRN-YYYYMMDD-XXX 格式）→ 定义 3-6 条评分清单 → 下次对照评分 → 出现 3+ 次则升级到 SOUL.md/AGENTS.md 或独立 Skill。这是最完整的"自我改进"文档化机制。

**弱点**：Skill 完全由人工维护，没有自动发现新 Skill 的能力。

### ZeroClaw：自动 Skill 发现 + sandbox 验证

**SkillForge**（`crates/zeroclaw-runtime/src/skillforge/mod.rs`）：
```rust
SkillForgeConfig {
    sources: [GitHub, ClawHub],  // 来源
    min_score: 0.7,              // 质量门槛
    scan_interval_hours: 24,     // 每24小时扫描
    sandbox_verify: true,        // 沙箱运行 TEST.sh
    sandbox_timeout_secs: 30,    // 超时30秒
    auto_integrate: true         // 自动集成通过验证的
}
```

**Scout 流程**（`scout.rs`）：
```rust
ScoutSource: GitHub | ClawHub | HuggingFace
// ScoutResult: name, url, stars, language, updated_at, has_license
```

**Evaluator 评分**（`evaluator.rs`）：
```rust
// weights: compatibility 0.3, quality 0.35, security 0.35
// Auto: >= min_score (0.7)
// Manual: 0.4 - 0.7
// Skip: < 0.4
```

**安全门**：sandbox_verify=true 时运行 TEST.sh，失败则回滚。这防止了自动集成恶意/破坏性 Skill。

**弱点**：自动发现的 Skill 质量依赖于 GitHub/ClawHub 生态的规范性；没有 Hermes 的意图识别自动激活。

### OpenClaw：最大生态 + 标准化最好

**规模**：100+ Skills（`~/.openclaw/workspace/.agents/skills/`）+ 内置 Skills（`~/.npm-global/lib/node_modules/openclaw/skills/`）。

**SKILL.md 标准**：每个 Skill 必须有 SKILL.md，`<available_skills>` 列表在 system prompt 中自动注入，agent 按 description 选择最匹配的 Skill 并 read 加载。

**ClawHub 集成**：通过 `clawhub` skill（clawhub.com）在线搜索/安装/更新/发布 Skills。

**Skill 加载机制**：lazy loading——system prompt 只注入 description，agent 决定调用哪个 skill 后才 `read` 加载 SKILL.md，节省 context。

| | Hermes | ZeroClaw | OpenClaw |
|---|---|---|---|
| **Skill 数量** | ~50（估计）| 少（系统刚建立）| 100+ |
| **自动发现** | ❌ | ✅ SkillForge | ✅ clawhub |
| **意图自动激活** | ✅ 23条规则 | ❌ | ✅ description匹配 |
| **sandbox 验证** | ❌ | ✅ TEST.sh | ❌ |
| **自我改进** | ✅ autoresearch loop | ❌ | ❌ |
| **标准规范** | agentskills.io | 内部规范 | SKILL.md + clawhub |

**最佳**：各有侧重。OpenClaw 生态最大；ZeroClaw 自动发现最可靠；Hermes 自我改进机制最成熟。

---

## 第七节：Prompt / SOUL / 身份管理

### Hermes：最细致的 SOUL.md

**结论：Hermes 的 SOUL.md 是三者中最具操作性的，从意图识别到技术节奏到自改进协议，每条都是可执行的规则，不是空话。**

`~/.hermes/SOUL.md` 关键内容：
- **grill-me 模式**：新需求来了先追问，不急执行
- **TDD 节奏**：垂直切片 + 红绿重构，不写"所有测试再一起实现"
- **Autoresearch Loop**：`.learnings/LEARNINGS.md`（LRN-YYYYMMDD-XXX）→ 评分清单 → 迭代 → 升级到 SOUL.md
- **Dan Koe 学习模式 + Karpathy 知识管理**：`knowledge/raw/` → `knowledge/*.md` → `research/*.md`
- **23 条意图识别规则**：把 Skill 变成"肌肉记忆"，不是用户的命令行
- **克制原则**：没搞清楚需求前不写代码，没确认前不 PR/部署/删数据

**PromptBuilder**（`agent/prompt_builder.py`）：构建完整系统 prompt，整合 SOUL.md + USER.md + MEMORY.md + 当前上下文。

### ZeroClaw：运行时注入，最轻量

**SOUL.md**（`workspace/SOUL.md`）：在运行时作为文件读入 system prompt。没有 Hermes 那样详细，但可以随时修改不需要重新编译。

**身份注入**：`ensure_heartbeat_file()` 创建 `HEARTBEAT.md`，把 agent 状态文件化。

### OpenClaw：三层身份系统

**SOUL.md + AGENTS.md + IDENTITY.md** 分别承担不同职责：
- `SOUL.md`：人格、风格、核心原则
- `AGENTS.md`：多 agent 团队通讯录，谁擅长什么
- `IDENTITY.md`：当前 agent 的具体角色定义

**Post-Compaction 注入**（`post-compaction-context.d.ts`）：压缩后重新注入 AGENTS.md 相关章节（带日期替换），防止 agent 压缩后"忘了自己是谁"。

| | Hermes | ZeroClaw | OpenClaw |
|---|---|---|---|
| **身份稳定性** | ✅ SOUL.md + autoresearch | 运行时注入 | ✅ post-compaction 注入 |
| **行为规则粒度** | ✅ 高（23条意图规则）| 中 | 中 |
| **多 agent 协调** | ✅ AGENTS.md | ❌ | ✅ AGENTS.md 团队目录 |

**最佳**：Hermes——SOUL.md 最具操作性，能真正改变 agent 行为；OpenClaw 的三层分离架构最清晰。

---

## 第八节：多渠道 / 多会话

### Hermes：单渠道

Hermes 本质是一个 CLI + API 框架，不是 bot 平台。多"渠道"体现在 terminal backend 多样性（local/docker/modal/ssh/daytona/singularity），而不是 IM 渠道。

**ACP Adapter**（`acp_adapter/`）：提供 ACP 协议支持，可以被 OpenClaw 等调用。

### ZeroClaw：channels crate

`zeroclaw-channels` crate 提供 channel 抽象，但具体 channel 实现细节未深入分析。Channel-based approval flow 存在（channel 不支持 approval 时自动拒绝危险操作）。

### OpenClaw：104 个扩展，最完整

**Channel 插件列表**（`dist/extensions/`，104 个扩展）：
- IM：feishu/telegram/discord/signal/slack/matrix/whatsapp/synology-chat/zalo/twitch/bluebubbles
- AI provider：anthropic/openai/amazon-bedrock/deepseek/volcengine/byteplus/chutes 等
- 工具：brave/tavily/browser/comfy/codex/runway/speech-core 等

**Session 隔离**：每个 channel + chat_id 组合独立 session，不同 channel 的会话互不干扰。

**跨 session 记忆**（LCM）：`allConversations=true` 可以跨所有 session 搜索历史摘要，这是真正的"跨 session 记忆"。

**Channel 配置**（`channel-config-helpers.js`）：
```javascript
resolveSdkChatChannelMeta()
buildChannelOutboundSessionRoute()
resolveChatChannelSecurity()  // DM security
resolveChatChannelPairing()   // device pairing
```

| | Hermes | ZeroClaw | OpenClaw |
|---|---|---|---|
| **IM 渠道数** | 0 | 少量 | 104 |
| **跨 session 记忆** | ❌ | 需配置 | ✅ LCM allConversations |
| **Session 隔离** | 进程级 | ✅ channel-based | ✅ channel+chatId |
| **Pairing / 设备授权** | ❌ | ✅ PairingGuard | ✅ device-pair |

**最佳**：OpenClaw——多渠道支持遥遥领先。

---

## 第九节：安全 / 权限

### Hermes：Python threading.Lock 级别

**ACP permissions**（`acp_adapter/permissions.py`）：映射 ACP PermissionOptionKind 到 approval 字符串。

**Approval 机制**（`cli.py`）：
```python
_approval_state    # 当前审批状态
_approval_deadline # 审批超时时间
_approval_lock     # threading.Lock 互斥
```
单 terminal session 的 approval，没有复杂的权限级别。

### ZeroClaw：最完整的安全子系统

`crates/zeroclaw-runtime/src/security/mod.rs` 包含：
```rust
// 沙箱后端
Sandbox trait: Docker | Firejail | Bubblewrap | Landlock
// 认证
PairingGuard    // 设备配对 + channel 认证
SecretStore     // 加密凭证存储
OtpValidator    // OTP 二次验证
NevisAuthProvider + IamPolicy  // IAM 集成
webauthn feature flag          // WebAuthn 支持
// 防护
AuditLogger     // 审计日志
LeakDetector    // 凭证泄漏检测
PromptGuard     // prompt injection 防御（MIT license）
WorkspaceBoundary  // 工作空间边界
DomainMatcher   // 域名白名单
EstopManager    // 紧急停止（多级别）
```

**CommandRiskLevel**（`security/policy.rs`）：
```rust
CommandRiskLevel: Low | Medium | High
// ActionTracker: 1小时滑动窗口
// PerSenderTracker: 每 chat-ID 独立的操作计数桶
```

### OpenClaw：channel 级别安全

```javascript
resolveChatChannelSecurity()  // DM security: approveChannelId + channelKey
resolveChatChannelPairing()   // device pairing
```

| | Hermes | ZeroClaw | OpenClaw |
|---|---|---|---|
| **沙箱执行** | ❌ | ✅ Docker/Firejail/Bubblewrap | ❌ |
| **Prompt injection 防御** | ❌ | ✅ PromptGuard | ❌ |
| **凭证泄漏检测** | ❌ | ✅ LeakDetector | ❌ |
| **紧急停止** | ❌ | ✅ EstopManager | ❌ |
| **Autonomy 分级** | ACP PermissionOptionKind | ✅ ReadOnly/Supervised/Full | DM security |
| **审计日志** | ❌ | ✅ AuditLogger | ❌ |

**最佳**：ZeroClaw——安全子系统是三者中最完整的，Hermes 和 OpenClaw 在安全方面都有明显缺口。

---

## 第十节：可观测性

### Hermes：InsightsEngine（成本分析最强）

**InsightsEngine**（`agent/insights.py`）：
- 从 SQLite 分析 token/cost/tool 使用历史
- `usage_pricing.py` 的 `CanonicalUsage + estimate_usage_cost`
- 可以按模型/会话/时间范围分析成本

这是三者中唯一有内置成本分析 dashboard 的。

### ZeroClaw：OTel + Prometheus + HeartbeatMetrics

**observability crate**（backends）：
```rust
log | verbose | prometheus | otel | noop
OtelObserver { endpoint, headers }    // OpenTelemetry endpoint
PrometheusObserver                    // Prometheus metrics
```
feature flags: `observability-otel` / `observability-prometheus`

**HeartbeatMetrics**（`heartbeat/`）：
```rust
HeartbeatMetrics {
    uptime_secs: u64,
    consecutive_successes: u32,
    consecutive_failures: u32,
    last_tick_at: DateTime<Utc>,
    avg_tick_duration_ms: f64,  // EMA
    total_ticks: u64,
}
// record_success(duration_ms) / record_failure(duration_ms)
// Shared via Arc<ParkingMutex<HeartbeatMetrics>>
```
avg_tick_duration_ms 用 EMA（指数移动平均），比简单平均更敏感于近期异常。

### OpenClaw：HeartbeatRunner + DiagnosticEvents

**HeartbeatRunner**（dist/）：
```typescript
HeartbeatReasonKind: "retry"|"interval"|"manual"|"exec-event"|"wake"|"cron"|"hook"|"other"
isWithinActiveHours()           // 活跃时间段门控
startHeartbeatRunner(stableSchedulerSeed)  // 稳定调度种子
DiagnosticHeartbeatEvent        // 诊断事件类型
```

| | Hermes | ZeroClaw | OpenClaw |
|---|---|---|---|
| **分布式 tracing** | ❌ | ✅ OTel | ❌ |
| **Metrics 暴露** | ❌ | ✅ Prometheus | ❌ |
| **成本分析** | ✅ InsightsEngine | ✅ cost tracking | ❌ |
| **Heartbeat** | cron job | ✅ HeartbeatMetrics EMA | ✅ HeartbeatRunner |
| **压缩历史可查** | ❌ | ❌ | ✅ LCM DAG |

**最佳**：ZeroClaw（OTel/Prometheus 生产级）；Hermes（成本分析最好）。

---

## 第十一节：互相学习建议（最重要）

### Hermes 应该向 ZeroClaw + OpenClaw 学习

**1. 引入 check_planning_without_execution**（抄 ZeroClaw `one2x/mod.rs:220`）

Hermes 的 agent 经常会出现"说了半天要做什么但不动手"的问题。ZeroClaw 的解决方案直接有效：检测 18 个规划短语 + 无执行指示符时，注入"立刻执行"nudge。

具体做法：在 `environments/agent_loop.py` 的 `_run_turn()` 后添加 `_check_planning_without_execution()` 检查，触发时在 messages 末尾注入 user 消息。

**2. 实现 repair_tool_pairing**（抄 ZeroClaw `one2x/mod.rs:12`）

Hermes 有 `_sanitize_tool_pairs()` 但只在压缩后运行。ZeroClaw 每次 LLM call 前都检查并修复孤儿 tool_call/result 对——这能防止在 streaming 中断或工具失败时 API 拒绝请求。

**3. 引入 focus_topic 压缩**（Hermes 已有，但应暴露给 ZeroClaw）

ZeroClaw 的多阶段压缩没有 focus_topic 参数。Hermes 的 `/compress <topic>` 让用户指定压缩重点，60-70% budget 给主题内容。这对长任务特别有用（e.g., "压缩历史但保留所有关于 database schema 的内容"）。

**4. 增加跨 session LCM**（抄 OpenClaw LCM 思路）

Hermes 的 session 结束就结束，没有跨 session 历史。OpenClaw 的 LCM DAG 可以跨 session 检索历史摘要。Hermes 可以在 session 结束时把关键摘要持久化到 `~/.hermes/history/` 目录，供后续 session 检索。

**5. 引入 AutonomyLevel**（抄 ZeroClaw `security/mod.rs`）

Hermes 的权限控制太简单（threading.Lock + ACP PermissionOptionKind）。ZeroClaw 的三级 AutonomyLevel（ReadOnly/Supervised/Full）+ per-sender 操作计数桶是更细粒度的控制模型，适合 Hermes 的 RL 训练场景（训练中完全自主，生产用 Supervised）。

---

### ZeroClaw 应该向 Hermes + OpenClaw 学习

**6. 引入 focus_topic 压缩参数**（抄 Hermes `context_compressor.py:676`）

ZeroClaw 的多阶段压缩没有话题引导。在 `compaction.rs` 的 `try_multi_stage_compress()` 中增加可选的 `focus_topic: Option<String>` 参数，当用户明确指定压缩重点时，让 STAGE_SUMMARIZER_SYSTEM 系统 prompt 包含话题权重指令。

**7. 引入 anti-thrashing 保护**（抄 Hermes `context_compressor.py:313`）

ZeroClaw 的压缩触发目前只看 token 阈值，可能频繁无效压缩。Hermes 的 anti-thrashing：连续 2 次压缩各节省 <10% → 跳过。在 ZeroClaw 的 `context_compressor.rs` 里维护 `last_two_savings: [f64; 2]`，在压缩前检查。

**8. 引入 InsightsEngine**（抄 Hermes `agent/insights.py`）

ZeroClaw 有 cost tracking（`TOOL_LOOP_COST_TRACKING_CONTEXT`）但没有历史分析。Hermes 的 SQLite 记录 + `estimate_usage_cost()` 让用户可以看到"上周花了多少钱"。ZeroClaw 可以把 cost tracking 数据写入 SQLite，暴露 `/costs summary` 命令。

**9. 引入 Skill 意图识别**（抄 Hermes SOUL.md 的 23 条触发规则 + OpenClaw SKILL.md description 匹配）

ZeroClaw 的 SkillForge 只做自动发现，没有自动激活。学 OpenClaw 的 description 匹配：把 Skill 的 description 注入 system prompt，让 LLM 决定要不要加载哪个 Skill；再参考 Hermes 的关键词意图识别，在 `agent_hooks` 里增加 Skill 触发逻辑。

**10. 改进 SOUL.md 到可执行规则**（抄 Hermes SOUL.md 风格）

ZeroClaw 的 `workspace/SOUL.md` 只有基础身份描述。参考 Hermes 的 SOUL.md：每条规则都是 IF-THEN 形式（"如果看到 X 场景，自动做 Y"），不是泛泛描述风格。

---

### OpenClaw 应该向 Hermes + ZeroClaw 学习

**11. 引入 check_planning_without_execution nudge**（抄 ZeroClaw `one2x/mod.rs:220`）

OpenClaw 没有这个机制。在 agent loop 的 tool dispatch 前增加检查：如果最后一条 assistant 消息包含规划语言但没有 tool call，注入执行 nudge。这在调用 Codex/Claude Code 等 ACP harness 时特别有用。

**12. 引入 Hermes 的 jittered backoff**（抄 `agent/retry_utils.py`）

OpenClaw 的 retry 用简单的 setTimeout，没有基于黄金比例哈希的 jitter。高并发场景（多个 channel 同时 retry）会产生 thundering herd。`time_ns ^ (tick * 0x9E3779B9)` 的 seed 方案成本极低，防效果好。

**13. 引入 repair_tool_pairing**（抄 ZeroClaw `one2x/mod.rs:12`）

OpenClaw 目前没有 tool pair 修复机制。当 ACP harness session 中断时（Codex crash 等），历史消息里会出现孤儿 tool_call。在 agent loop 中每次 LLM call 前调用 repair 函数，能显著减少 API 422/400 错误。

**14. 引入时间衰减记忆**（抄 ZeroClaw `memory/decay.rs`）

OpenClaw 的 active-memory 没有时间衰减。记忆累积越来越多，召回质量下降。ZeroClaw 的指数衰减（half_life=7天）+ Core memories evergreen 模式非常适合 OpenClaw 的个人助理场景（最近的记忆最重要，但用户明确标记的重要事项永不过期）。

**15. 暴露 cost analytics**（抄 Hermes InsightsEngine + ZeroClaw cost tracking）

OpenClaw 没有成本分析工具。用户不知道自己每天花了多少钱。在 heartbeat 或 `/status` 命令中显示本月 token 消耗和估算成本，数据存 SQLite（参考 Hermes `agent/insights.py:InsightsEngine`）。

---

## 结语：选哪个？

**Hermes**：适合 RL 训练场景、需要精细成本控制、团队规模小且以 Python 为主的场景。**不适合**：多用户 bot 平台、需要实时多渠道支持、需要生产级安全隔离。

**ZeroClaw**：适合高可靠性生产部署、需要安全沙箱、团队有 Rust 能力、希望极致性能。**不适合**：快速原型、需要大量第三方 IM 渠道集成、Python/JS 优先的团队。

**OpenClaw**：适合个人助理场景、需要接多个 IM 渠道、想用现成 100+ Skills 生态。**不适合**：需要精细安全控制的企业部署、需要成本分析的场景、需要修改核心 agent loop 的场景（打包产物难以定制）。

三者都没有完整的 circuit breaker 实现，这是共同的技术债。

---

## 源码引用索引

1. `~/.hermes/hermes-agent/environments/agent_loop.py:30` — ThreadPoolExecutor(128)
2. `~/.hermes/hermes-agent/environments/agent_loop.py:50-80` — AgentResult dataclass
3. `~/.hermes/hermes-agent/agent/context_compressor.py:37` — SUMMARY_PREFIX
4. `~/.hermes/hermes-agent/agent/context_compressor.py:52-63` — compaction 常量
5. `~/.hermes/hermes-agent/agent/context_compressor.py:66` — _summarize_tool_result()
6. `~/.hermes/hermes-agent/agent/context_compressor.py:313` — should_compress() + anti-thrashing
7. `~/.hermes/hermes-agent/agent/context_compressor.py:676` — focus_topic 权重
8. `~/projects/tools/zeroclaw/crates/zeroclaw-runtime/src/agent/loop_.rs:1-100` — loop 常量 + MODEL_SWITCH_REQUEST
9. `~/projects/tools/zeroclaw/crates/zeroclaw-runtime/src/one2x/mod.rs:5` — session_hygiene 模块
10. `~/projects/tools/zeroclaw/crates/zeroclaw-runtime/src/one2x/mod.rs:100` — micro_compact_old_tool_results, KEEP_RECENT_TURNS=3
11. `~/projects/tools/zeroclaw/crates/zeroclaw-runtime/src/one2x/mod.rs:131` — limit_tool_result_sizes_with_budget, TAIL_CHARS=2000
12. `~/projects/tools/zeroclaw/crates/zeroclaw-runtime/src/one2x/mod.rs:172-245` — agent_hooks: PLANNING_PHRASES, EXECUTION_INDICATORS, check_planning_without_execution()
13. `~/projects/tools/zeroclaw/crates/zeroclaw-runtime/src/one2x/compaction.rs:1-120` — multi-stage compaction: KEY_FACTS_EXTRACTOR, CHUNK_SIZE=20
14. `~/projects/tools/zeroclaw/crates/zeroclaw-runtime/src/one2x/compaction.rs` — QUALITY_CHECK_SYSTEM, PASS/FAIL
15. `~/.npm-global/lib/node_modules/openclaw/dist/extensions/active-memory/index.js:13-79` — DEFAULT_TIMEOUT_MS, RECALLED_CONTEXT_LINE_PATTERNS
16. `~/.npm-global/lib/node_modules/openclaw/dist/extensions/` — 104 个扩展（`ls | wc -l`）
17. `~/.hermes/SOUL.md` — Autoresearch Loop, grill-me, TDD, 意图识别规则
