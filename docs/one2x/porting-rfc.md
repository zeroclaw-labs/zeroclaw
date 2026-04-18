# ZeroClaw LCM/Memory 移植 RFC

> 作者：刘小奎（主 agent 手写，基于 ZeroClaw v7 源码 + OpenClaw LCM 插件 + Codex reality-check）
> 日期：2026-04-18
> 分支：`one2x/custom-v7` (HEAD 8eaafc5e)

## 0. TL;DR

**关键修正——ZeroClaw 的记忆系统比之前文档描述的成熟得多。**

ZeroClaw 已经有独立 crate `crates/zeroclaw-memory/`（20+ 模块），包含：
- 5 个 backend：`SqliteMemory`（含 FTS5+嵌入）、`MarkdownMemory`、`QdrantMemory`、`LucidMemory`、`NoneMemory`
- `AuditedMemory` 审计包装、`NamespacedMemory` 命名空间
- `RetrievalPipeline`（混合检索）、`consolidation`（回合合并）、`decay`（衰减）、`conflict`（冲突检测）
- `knowledge_graph` + `knowledge_extraction`、`injection_guard`、`hygiene`

**这意味着 §2 comparison 文档里"把 ZeroClaw 降为 Markdown + 向量索引"的建议多余了——已经实装过了。**

真正缺的是 **DAG 可展开层**：现在压缩后 summary 是**不可展开**地塞回 history（assistant message `[CONTEXT SUMMARY ...]`）+ `Memory::store()` 走记忆层，但没有 `summary_id → raw_messages` 的反向映射表，也没有 `lcm_expand` 等价工具。

## 1. 真实现状（基于源码）

### 1.1 Memory crate 结构

`crates/zeroclaw-memory/src/lib.rs`（行 1-27）：

```rust
pub mod audit;             // AuditedMemory 审计
pub mod backend;           // MemoryBackendKind 枚举 + 分类
pub mod chunker;           // 文本切块
pub mod conflict;          // 冲突检测
pub mod consolidation;     // 回合合并 consolidate_turn
pub mod decay;             // 记忆衰减
pub mod embeddings;        // 嵌入抽象
pub mod hygiene;
pub mod importance;
pub mod injection_guard;
pub mod knowledge_extraction;
pub mod knowledge_graph;
pub mod lucid;             // LucidMemory
pub mod markdown;          // MarkdownMemory
pub mod namespaced;        // NamespacedMemory（多租户）
pub mod none;
pub mod policy;
pub mod qdrant;            // QdrantMemory（向量 DB）
pub mod response_cache;
pub mod retrieval;         // RetrievalPipeline
pub mod snapshot;
pub mod sqlite;            // SqliteMemory（主力 backend）
pub mod traits;            // Memory trait（re-export from zeroclaw-api）
pub mod vector;
```

### 1.2 SqliteMemory schema

`crates/zeroclaw-memory/src/sqlite.rs` L163-210：

```sql
-- 主表
CREATE TABLE memories (
    id         TEXT PRIMARY KEY,
    key        TEXT NOT NULL UNIQUE,
    content    TEXT NOT NULL,
    category   TEXT NOT NULL DEFAULT 'core',
    embedding  BLOB,                    -- 嵌入向量（直接存表里，非外部 DB）
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    session_id TEXT,                    -- migration 添加
    namespace  TEXT                     -- migration 添加
);

-- FTS5 全文索引（BM25 scoring，CJK 无特殊处理但能工作）
CREATE VIRTUAL TABLE memories_fts USING fts5(
    key, content, content=memories, content_rowid=rowid
);

-- 嵌入缓存（LRU）
CREATE TABLE embedding_cache (
    content_hash TEXT PRIMARY KEY,
    embedding    BLOB NOT NULL,
    created_at   TEXT NOT NULL,
    accessed_at  TEXT NOT NULL
);
```

**关键发现：** `rusqlite 0.37` 带 FTS5 没问题，`embedding BLOB` 直接存在 `memories` 表里，不需要独立向量 DB。

### 1.3 压缩触发链路

`crates/zeroclaw-runtime/src/agent/loop_.rs`：

- **L280**：`const NON_SYSTEM_MSG_THRESHOLD` — 自动压缩阈值
- **L995**：`micro_compact_old_tool_results(history)` — 轻量级 tool 结果压缩（不是完整压缩）
- **L2594-2638**：**Preemptive compaction（主路径）**
  ```
  estimate_tokens()
    → 如果进入 Hard zone
    → try_multi_stage_compress()  // 来自 one2x/compaction.rs
    → 失败则 warn，继续运行
  ```
- **L2747-2751**：Channel loop 里的 compaction recovery

**压缩产物有两个出口：**
1. 进程内 history 插入一条 `[CONTEXT SUMMARY ...]` assistant message（**丢失原始消息引用**）
2. `Memory::store()` 把关键事实存入 `SqliteMemory`（可通过 `RetrievalPipeline` 召回，但和 summary 无关联）

### 1.4 当前压缩算法（`one2x/compaction.rs` 398 行）

- 多阶段 chunked 压缩：`CHUNK_SIZE=20`, `CHUNK_MAX_CHARS=15_000`
- 4 个 LLM 角色：`KEY_FACTS_EXTRACTOR` / `STAGE_SUMMARIZER` / `MERGE_SUMMARIZER` / `QUALITY_CHECK`
- **PASS/FAIL 质检门**：QUALITY_CHECK 裁决是否接受摘要
- 失败处理：直接 warn 继续，没有多级降级

## 2. OpenClaw LCM vs ZeroClaw 对照表

| 能力 | OpenClaw (lossless-claw) | ZeroClaw 当前 | Gap |
|------|-------------------------|--------------|------|
| 压缩触发 | `contextThreshold=0.75 * budget` | `loop_.rs L2594` 预估 token，hard zone 触发 | ✅ 对等 |
| 多级 summarize 降级 | normal → aggressive → fallback → capped | PASS/FAIL 质检门，失败 warn 继续 | ⚠️ ZeroClaw 可以加 aggressive retry |
| Summary 存储 | SQLite `summaries` 表，可展开到 `summary_messages` | history 里 `[CONTEXT SUMMARY]` 文本 + `Memory::store` | ❌ 无展开映射 |
| 压缩可逆性 | 可展开引用（raw message 永不删） | 不可逆（history 被替换） | ❌ 核心缺失 |
| DAG 上卷 | condensed summary 折叠 leaf | 无 | ❌ 缺失 |
| Dynamic system prompt | assembler 根据深度注入不同强度"expand first"规则 | 静态 system prompt | ❌ 零成本可加 |
| Expand 工具 | `lcm_grep` / `lcm_describe` / `lcm_expand` / `lcm_expand_query` | `memory.recall` 但不带 summary 维度 | ❌ 缺失 |
| Distilled sub-agent recall | `lcm_expand_query` spawn 子 agent | 主 agent 直接读 memory | ❌ 缺失 |
| 向量索引 | plugin-sdk + memory-lancedb（独立扩展） | `SqliteMemory.embedding BLOB` 内嵌 + Qdrant 可选 | ✅ 已有（架构更简单） |
| 知识图谱 | `memory-wiki` 扩展（Claim 结构） | `knowledge_graph` + `knowledge_extraction` 内建 | ✅ 对等 |
| FTS5 | 有（summary_fts + messages_fts） | 有（memories_fts，BM25） | ✅ 对等 |
| 审计/冲突 | 无内建 | `AuditedMemory`、`conflict.rs`、`decay.rs` | ✅ ZeroClaw 更强 |

## 3. 落地优先级（按 ROI 重新排序）

### 🥇 P1：Dynamic System Prompt Addition（1-2 天）

**收益**：压缩后的 history 里有 `[CONTEXT SUMMARY ...]` 时，LLM 更倾向于调 `memory.recall` 验证，而不是直接猜。

**实施位置**：
- 文件：`crates/zeroclaw-runtime/src/agent/loop_.rs` 系统 prompt 构建处
- 新增函数：`fn build_recall_addendum(history: &[ChatMessage]) -> Option<String>`
  - 扫描 history，count `[CONTEXT SUMMARY]` 数量和最老出现位置
  - 数量 ≥ 1 时追加：
    ```
    ## Context Recall Guidance
    Some older messages have been compacted into [CONTEXT SUMMARY] markers.
    Before answering with specific details (commands, file paths, exact values),
    use `memory.recall` or the context recovery tools to verify.
    Do not fabricate details from condensed summaries.
    ```
  - 数量 ≥ 3 时追加更强的 uncertainty checklist

**风险**：零。纯 prompt 增量，不改数据结构。

**对照 OpenClaw**：`src/assembler.ts` L60-135。

---

### 🥈 P2：Summary ↔ Raw Messages 映射表 + expand tool（1-2 周）

**收益**：把 ZeroClaw 从"压缩即丢失"升级为"压缩可展开"。单层 DAG（leaf summary → 原 messages），不做 condensed 上卷（那是 P3）。

**实施位置**：
- 新模块：`crates/zeroclaw-memory/src/context_summary.rs`
- 数据结构（Rust struct）：
  ```rust
  pub struct ContextSummaryStore {
      conn: Arc<Mutex<Connection>>,
  }

  pub struct SummaryRecord {
      pub summary_id: String,       // "sum_" + sha256(content+ts)[..16]
      pub session_id: String,
      pub seq: i64,                 // 该 session 内的序号
      pub content: String,          // summary 文本
      pub token_count: u32,
      pub created_at: DateTime<Utc>,
      pub model: String,            // 生成用的模型
      pub earliest_msg_ts: DateTime<Utc>,
      pub latest_msg_ts: DateTime<Utc>,
      pub message_count: u32,
  }
  ```

- SQLite schema（加到 SqliteMemory.init_schema 或独立 DB）：
  ```sql
  CREATE TABLE context_summaries (
      summary_id TEXT PRIMARY KEY,
      session_id TEXT NOT NULL,
      seq INTEGER NOT NULL,
      content TEXT NOT NULL,
      token_count INTEGER,
      created_at TEXT NOT NULL,
      model TEXT,
      earliest_msg_ts TEXT,
      latest_msg_ts TEXT,
      message_count INTEGER,
      UNIQUE(session_id, seq)
  );
  CREATE INDEX idx_csum_session ON context_summaries(session_id);

  CREATE TABLE context_summary_messages (
      summary_id TEXT NOT NULL,
      ordinal INTEGER NOT NULL,
      role TEXT NOT NULL,
      content TEXT NOT NULL,              -- raw message JSON
      created_at TEXT NOT NULL,
      PRIMARY KEY (summary_id, ordinal),
      FOREIGN KEY (summary_id) REFERENCES context_summaries(summary_id) ON DELETE CASCADE
  );

  CREATE VIRTUAL TABLE context_summaries_fts USING fts5(
      content, content=context_summaries, content_rowid=rowid
  );
  ```

- 修改 `one2x/compaction.rs::try_multi_stage_compress()`：
  - 压缩产出 summary 时，同时写 `context_summaries` + `context_summary_messages`（传入被压缩的原始 history slice）
  - history 里插入的 `[CONTEXT SUMMARY]` 消息带上 `summary_id`，格式：
    ```
    [CONTEXT SUMMARY sum_abc123] <content>
    ```

- 新增 tool：`context_expand`
  - 实现位置：`crates/zeroclaw-tools/src/builtin/` 新增 `context_expand.rs`
  - 输入：`{ summary_id: String, include_messages?: bool }`
  - 输出：summary metadata + 可选的 raw messages
  - 对应 OpenClaw 的 `lcm_describe` + `lcm_expand`（简化版：单层）

**对照 OpenClaw**：
- `src/compaction.ts` L398-520（leafPass）
- `src/store/summary-store.ts`（1133 行，可大幅精简）

**风险**：
- ⚠️ history 里 `[CONTEXT SUMMARY sum_xxx]` 格式变更，需要同步更新 `injection_guard` 和 parser
- ⚠️ 跨 session 的 summary 清理策略需要定义（建议挂到 `decay.rs`）

**工作量**：~10 人天（新 crate 模块 + compaction 改造 + tool + 测试）

---

### 🥉 P3：Distilled Sub-Agent Recall + 多级降级（2-3 周）

**收益**：主 agent 不污染上下文；压缩鲁棒性提升。

**实施位置**：
- 多级降级：`one2x/compaction.rs` 加 `SummarizerStrategy` enum：
  ```rust
  enum SummarizerStrategy {
      Normal,      // 当前 PASS/FAIL
      Aggressive,  // 更激进的 system prompt，目标更小
      Fallback,    // 确定性截断（尾部 N 条 + 首 M 条）
      Capped,      // 硬截断到 max_chars
  }
  ```
  失败时按顺序降级，最坏情况给出截断摘要。

- Distilled recall tool：
  - 新 tool：`context_deep_recall`
  - 实现逻辑：起子 agent（用 orchestrator），传 summary_ids 列表
  - 子 agent 调 `context_expand` 读展开内容，返回针对 prompt 的聚焦答案
  - 主 agent 只看到答案，不看到原始展开内容

**对照 OpenClaw**：
- `src/tools/lcm-expand-query-tool.ts`（793 行）
- `src/expansion.ts::distillForSubagent()`

**风险**：
- ⚠️ 防递归：子 agent 不能再调 `context_deep_recall`（用 tool allowlist 控制）
- ⚠️ token 预算传递：主 agent 要把展开预算通过 tool input 传给子 agent

**工作量**：~15 人天

---

### ⏸ P4（可选，1-2 月后）：DAG 上卷

Condensed summary 层——多个 leaf summary 折叠成更高层。**现阶段不做**，因为：
- ZeroClaw session 生命周期通常 < 24h，DAG 上卷主要解决超长 session（多天到多周）
- DAG integrity 校验的工程开销大（对应 OpenClaw 的 `integrity.ts` 600 行）
- P2 单层展开已经解决 90% 场景

## 4. Reality Check：哪些 OpenClaw 方案在 Rust 里反而更容易

| OpenClaw TS 痛点 | Rust 里的情况 |
|-----------------|--------------|
| `Promise` 错误传播复杂 | Rust `Result<T, E>` + `?` 更清晰 |
| SQLite 并发需要手搓连接池 | `rusqlite` + `Arc<Mutex<Connection>>` 已在 `SqliteMemory` 里搞定 |
| FTS5 CJK 分词 | 和 TS 一样，BM25 对 CJK 不完美，但能用。ZeroClaw `memories_fts` 已经跑起来了 |
| DAG 完整性校验 | Rust 类型系统保证 FK 约束，少写很多运行时检查 |
| `expansion-auth.ts` grantId 机制 | ZeroClaw 子 agent 通过 orchestrator 调度，权限边界更清晰，不需要 grantId |

## 5. 不建议抄的部分

| OpenClaw 功能 | 为什么不抄 |
|-------------|----------|
| `bootstrap_state` 从外部 session 文件冷启动 | ZeroClaw session state 自管，不需要兼容 |
| `message_parts` 表的 12 种 part_type | ZeroClaw `ChatMessage` 已经是 tagged enum，不需要平坦化存储 |
| `large_files` 外部化表 | ZeroClaw 已有 skills snapshot + artifacts 机制 |
| DAG 多 parent 边（`summary_parents` 表） | P3 之前用不上，P3 之后再考虑 |

## 6. 三阶段路线图

### Phase 1（第 1 周）：零风险增量
- [ ] P1 Dynamic system prompt（1-2 天）
- [ ] `one2x/compaction.rs` 加 Aggressive retry 降级（1-2 天）
- [ ] 监控指标：压缩成功率、平均摘要 token 数、QUALITY_CHECK PASS 率

### Phase 2（第 2-4 周）：核心能力
- [ ] P2 `context_summary.rs` 模块 + SQLite schema
- [ ] 修改 `try_multi_stage_compress` 写 summary → raw 映射
- [ ] 新 tool `context_expand`
- [ ] `RetrievalPipeline` 扩展：recall 结果里同时返回 summary_id
- [ ] injection_guard 适配 `[CONTEXT SUMMARY sum_xxx]` 格式

### Phase 3（第 5-8 周）：深度能力
- [ ] P3 多级降级（4 种 SummarizerStrategy）
- [ ] `context_deep_recall` tool + 子 agent 路由
- [ ] tool allowlist 防递归
- [ ] 压力测试：多轮压缩场景、跨 session expand、子 agent 预算控制

## 7. 开放问题（需要刘奎决策）

1. **新 SQLite DB 文件位置**：
   - 选项 A：复用 `SqliteMemory` 的主 DB（memories + context_summaries 混在一起）
   - 选项 B：独立 `~/.zeroclaw/sessions/<sid>/summaries.db`
   - **倾向 B**：session 级隔离，和 Memory 长期数据解耦

2. **`[CONTEXT SUMMARY]` 格式变更**：
   - 老格式 `[CONTEXT SUMMARY ...]` 可能已有消费者（UI 显示、日志解析）
   - 新格式 `[CONTEXT SUMMARY sum_abc123] ...`
   - 需要 grep 一遍 `zeroclaw-channels` 看有没有 consumer

3. **子 agent 模型选择**：
   - `context_deep_recall` 的子 agent 用什么模型？
   - OpenClaw 默认 Sonnet，ZeroClaw 可以用 `config.agent.fast_model`

## 8. 参考

- OpenClaw lossless-claw 源码：`/home/ec2-user/.openclaw/extensions/lossless-claw/src/`
- ZeroClaw v7：`/home/ec2-user/projects/tools/zeroclaw`，分支 `one2x/custom-v7`
- 前置文档：
  - `research/agent-intelligence-comparison.md`（三家对比）
  - `research/openclaw-deepdive.md`（1425 行，Claude Code ACP 生成，全系统覆盖）
  - `research/openclaw-lcm-deepdive.md`（593 行，主 agent 手写，带 LCM 源码行号）
