# OpenClaw LCM & Memory 深度分析（基于实装源码）

> 作者：刘小奎 🦞  写于 2026-04-18
> 基于阅读 `@martian-engineering/lossless-claw@0.5.3` 源码（16,577 行 TypeScript）+ OpenClaw 官方文档
> 修正了之前对比文档里的多处过度渲染

---

## 0. 纠正之前的误解

上次那份 `agent-intelligence-comparison.md` 里我写的"OpenClaw LCM 是核心功能、DAG 可展开"**不完全对**：

- **LCM 实际是一个独立的 OpenClaw 插件**：`@martian-engineering/lossless-claw`，Martian Engineering 出品，MIT 许可
- 装在 `/home/ec2-user/.openclaw/extensions/lossless-claw/`（v0.5.3）
- 通过 `plugins.slots.contextEngine = "lossless-claw"` 选用
- 默认 OpenClaw 用的是 `legacy` 引擎（没 DAG，退化到 pi-coding-agent 的平坦 CompactionEntry）
- **DAG 是真 DAG**（summary 可有多 parent，不只是树）

下面内容全部来自实际读代码，有具体文件和行号。

---

## 1. 架构总览

```
┌─────────────────────────────────────────────────────────────┐
│  OpenClaw Core (TS/Node)                                    │
│  ┌──────────────────┐       ┌───────────────────────┐       │
│  │ contextEngine    │◄──────│ plugins.slots         │       │
│  │ slot selector    │       │ .contextEngine =      │       │
│  │                  │       │  "lossless-claw"      │       │
│  └────────┬─────────┘       └───────────────────────┘       │
│           │                                                  │
│           ▼ 4 lifecycle hooks (ingest/assemble/compact/afterTurn)
└───────────┼──────────────────────────────────────────────────┘
            │
┌───────────▼──────────────────────────────────────────────────┐
│  lossless-claw plugin (TS, ESM)                              │
│                                                              │
│   LcmContextEngine (engine.ts, 2838 行)                      │
│     ├─ assembler.ts   (1088 行) — 组装 context 喂给 LLM     │
│     ├─ compaction.ts  (1663 行) — 多级压缩（leaf→condensed）│
│     ├─ expansion.ts   (383 行)  — 展开摘要为子树            │
│     ├─ summarize.ts   (1391 行) — LLM 摘要生成 + 重试       │
│     ├─ retrieval.ts   (361 行)  — FTS5/regex 检索           │
│     └─ integrity.ts   (600 行)  — DAG 完整性校验            │
│                                                              │
│   Storage (SQLite via node:sqlite)                           │
│     ├─ conversation-store.ts (892 行) — messages/message_parts│
│     └─ summary-store.ts      (1133 行) — summaries + DAG edges│
│                                                              │
│   Agent Tools (暴露给 LLM)                                   │
│     ├─ lcm_grep          — 搜索 summary/message             │
│     ├─ lcm_describe      — 查 summary 元数据和子树 manifest  │
│     ├─ lcm_expand        — 按 ID 或 query 展开 DAG         │
│     └─ lcm_expand_query  — 展开+子 agent 蒸馏成聚焦答案      │
└──────────────────────────────────────────────────────────────┘
```

**核心数据流：**
1. 每条 user/assistant/tool 消息 → `ingest` hook → 写入 `messages` + `message_parts` + 作为 `context_items` 末尾的 `message` 项
2. 达到 `contextThreshold`（默认 0.75 of budget）→ `compact` hook 触发
3. 压缩时先把旧消息折叠成 **leaf summary**（depth=0），更老的 leaf 再被折叠成 **condensed summary**（depth≥1），形成 DAG
4. `context_items` 被重写：折叠区间的 message 被替换成 summary 节点；近期 `freshTailCount`（默认 32）条保持 message 形态
5. `assemble` 时按 `context_items` 顺序拼出最终 LLM context（summary 节点渲染为 `<summary id="sum_xxx">…</summary>` XML）
6. LLM 看 summary 如果想看详情，调用 `lcm_expand(summaryIds=["sum_xxx"])` → 查 `summary_parents` 拿子 summary/leaf → 渲染成 prompt 回填

---

## 2. DAG 存储 Schema（实际 DDL）

文件：`src/db/migration.ts` L430-570

```sql
-- 会话级元数据
CREATE TABLE conversations (
  conversation_id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL,
  session_key TEXT,
  title TEXT,
  bootstrapped_at TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- 原始消息（压缩后也不删）
CREATE TABLE messages (
  message_id INTEGER PRIMARY KEY AUTOINCREMENT,
  conversation_id INTEGER NOT NULL REFERENCES conversations,
  seq INTEGER NOT NULL,
  role TEXT CHECK (role IN ('system','user','assistant','tool')),
  content TEXT NOT NULL,
  token_count INTEGER NOT NULL,
  created_at TEXT,
  UNIQUE (conversation_id, seq)
);

-- 消息细节（tool_call、reasoning、patch、file 等分开存）
CREATE TABLE message_parts (
  part_id TEXT PRIMARY KEY,
  message_id INTEGER NOT NULL REFERENCES messages,
  session_id TEXT NOT NULL,
  part_type TEXT CHECK (part_type IN (
    'text','reasoning','tool','patch','file',
    'subtask','compaction','step_start','step_finish',
    'snapshot','agent','retry'
  )),
  ordinal INTEGER NOT NULL,
  text_content TEXT,
  tool_call_id TEXT, tool_name TEXT, tool_input TEXT, tool_output TEXT,
  patch_hash TEXT, patch_files TEXT,
  file_mime TEXT, file_name TEXT, file_url TEXT,
  subtask_prompt TEXT, subtask_agent TEXT,
  step_cost REAL, step_tokens_in INTEGER, step_tokens_out INTEGER,
  metadata TEXT,
  UNIQUE (message_id, ordinal)
);

-- 摘要节点（核心 DAG 的顶点）
CREATE TABLE summaries (
  summary_id TEXT PRIMARY KEY,            -- "sum_<hex>"
  conversation_id INTEGER NOT NULL,
  kind TEXT CHECK (kind IN ('leaf','condensed')),
  depth INTEGER NOT NULL DEFAULT 0,       -- 0 = leaf (直接盖住 messages)
  content TEXT NOT NULL,
  token_count INTEGER NOT NULL,
  earliest_at TEXT, latest_at TEXT,       -- 覆盖的消息时间范围
  descendant_count INTEGER,               -- 子树大小（统计用）
  descendant_token_count INTEGER,
  source_message_token_count INTEGER,
  created_at TEXT,
  file_ids TEXT NOT NULL DEFAULT '[]'     -- 关联的 large_files
);

-- Leaf summary 到原始 message 的映射（压缩可逆的根基）
CREATE TABLE summary_messages (
  summary_id TEXT NOT NULL REFERENCES summaries,
  message_id INTEGER NOT NULL REFERENCES messages,
  ordinal INTEGER NOT NULL,
  PRIMARY KEY (summary_id, message_id)
);

-- DAG 父子边（condensed summary 可有多 parent）
CREATE TABLE summary_parents (
  summary_id TEXT NOT NULL REFERENCES summaries,          -- 被折叠者（子）
  parent_summary_id TEXT NOT NULL REFERENCES summaries,   -- 折叠之后的父
  ordinal INTEGER NOT NULL,
  PRIMARY KEY (summary_id, parent_summary_id)
);

-- 当前喂给 LLM 的顺序列表（message 和 summary 混编）
CREATE TABLE context_items (
  conversation_id INTEGER NOT NULL,
  ordinal INTEGER NOT NULL,
  item_type TEXT CHECK (item_type IN ('message','summary')),
  message_id INTEGER REFERENCES messages,
  summary_id TEXT REFERENCES summaries,
  created_at TEXT,
  PRIMARY KEY (conversation_id, ordinal),
  CHECK (
    (item_type='message' AND message_id IS NOT NULL AND summary_id IS NULL) OR
    (item_type='summary' AND summary_id IS NOT NULL AND message_id IS NULL)
  )
);

-- 大文件（附件）单独存，用 file_id 引用
CREATE TABLE large_files (
  file_id TEXT PRIMARY KEY,
  conversation_id INTEGER NOT NULL,
  file_name TEXT, mime_type TEXT, byte_size INTEGER,
  storage_uri TEXT NOT NULL,
  exploration_summary TEXT,
  created_at TEXT
);

-- 跨重启 bootstrap（从现有 session 文件复原 DAG）
CREATE TABLE conversation_bootstrap_state (
  conversation_id INTEGER PRIMARY KEY,
  session_file_path TEXT NOT NULL,
  last_seen_size INTEGER, last_seen_mtime_ms INTEGER,
  last_processed_offset INTEGER,
  last_processed_entry_hash TEXT,
  updated_at TEXT
);

-- 索引
CREATE INDEX messages_conv_seq_idx ON messages (conversation_id, seq);
CREATE INDEX summaries_conv_created_idx ON summaries (conversation_id, created_at);
CREATE INDEX context_items_conv_idx ON context_items (conversation_id, ordinal);

-- FTS5 全文搜索（可选，编译不支持时退化到 LIKE）
CREATE VIRTUAL TABLE messages_fts USING fts5(content, ...);
CREATE VIRTUAL TABLE summaries_fts USING fts5(content, ...);
```

**关键观察：**
- `context_items` 是一张"快照表"，每次 compaction 后重写，不是 append-only
- `messages` 和 `summaries` 永不删除——压缩是"把 context_items 指针从 message 挪到 summary"
- `summary_parents` 的多对多设计使得条件合并（多个 leaf 合并成一个 condensed）和多重引用都可能
- SQLite 放在 `~/.openclaw/lcm.db`（具体路径由 `LcmConfig.dbPath` 决定，在 `db/config.ts`）


---

## 3. Compaction 多级策略

文件：`src/compaction.ts`（1663 行）

### 3.1 分层：Leaf + Condensed

LCM 用两层摘要架构：

- **Leaf summary**（`depth=0`）：直接覆盖一批 raw message。一次 leaf 压缩处理 `leafChunkTokens`（默认 20000 tokens）的原始消息，产出 `leafTargetTokens`（默认 600）token 的摘要。
- **Condensed summary**（`depth≥1`）：折叠若干已存在的 summary（可能是 leaf，也可能是更低 depth 的 condensed），形成 DAG 的上层节点。目标 `condensedTargetTokens`（默认 900）。

两层的 fanout 约束：
- `leafMinFanout`：达到此数量的 leaf 才启动 condense
- `condensedMinFanout`：condensed 层的最小输入个数
- `condensedMinFanoutHard`：硬触发时的放宽值（强制压缩场景）

### 3.2 触发条件

```typescript
const threshold = Math.floor(contextThreshold * tokenBudget);  // 默认 0.75 * budget
const leafTrigger = await evaluateLeafTrigger(conversationId);

if (!force && tokensBefore <= threshold && !leafTrigger.shouldCompact) {
  return { actionTaken: false, ... };
}
```

两个口子：
1. **软触发**（threshold）：`context_items` 总 token 数 > 0.75 × budget
2. **硬触发**（leafTrigger）：特定模式被触发（例如连续长 tool 输出填满），可以独立于总 token 数

### 3.3 执行流程（`compactLeaf`）

```
1. 选择最老的可压缩 leaf chunk：selectOldestLeafChunk()
   - 从 context_items 头部往后找，跳过 freshTailCount 尾部（默认 32）
   - 累积到 leafChunkTokens（20k）或自然边界

2. 解析 prior leaf summary（连续性）：
   - resolvePriorLeafSummaryContext — 取上一次压缩产出的 summary 内容
   - 作为 previousSummary 传给 summarize()，让 LLM 知道"之前压缩过什么"
   - 保证时间线连续，不会重复摘要同一段

3. leafPass():
   - 拼 prompt: [系统摘要指令, previousSummary（如有）, chunk 的原始消息]
   - 调 LLM: complete({ model: summaryModel, messages, maxTokens: leafTargetTokens * 3 })
   - 多级失败回退：normal → aggressive → fallback → capped
     * normal: 标准 prompt
     * aggressive: 更激进的删减指令
     * fallback: 用确定性截断（LLM 失败时）—— src/compaction.ts 的 FALLBACK_MAX_CHARS = 512*4
     * capped: capSummaryText() 硬截断到 maxTokens

4. 写 summaries + summary_messages + 更新 context_items:
   - 新 summary_id = "sum_" + sha256(content + Date.now()).slice(0,16)
   - summary_messages 记录这个 leaf 覆盖了哪些 message_id
   - context_items 删除被覆盖的 message 行，插入 summary 行
   - DELETE 和 INSERT 在 transaction 内，保证原子性

5. 可选 incremental condense:
   - incrementalMaxDepth 默认 1（刘奎的配置里是 -1 = 不做）
   - 每次 leaf 压缩后，顺带跑一轮 condensed pass
```

### 3.4 Condensed Pass（上卷）

```typescript
for (let targetDepth = 0; targetDepth < incrementalMaxDepth; targetDepth++) {
  const fanout = resolveFanoutForDepth(targetDepth, false);
  const chunk = await selectOldestChunkAtDepth(conversationId, targetDepth);

  if (chunk.items.length < fanout ||
      chunk.summaryTokens < condensedMinChunkTokens) {
    break;  // 不够料，停
  }

  const condenseResult = await condensedPass(
    conversationId, chunk.items, targetDepth, summarize, summaryModel);
}
```

condensed summary 的 parent 关系：**新 condensed 的 parent = 参与折叠的所有 summary**，写入 `summary_parents` 表。所以 summary 是多 parent 可能的 —— 真 DAG，不只是树。

### 3.5 关键边界处理

| 问题 | 处理方式 | 文件位置 |
|------|---------|----------|
| Tool use/result 配对不全 | `transcript-repair.ts` sanitize | `src/transcript-repair.ts` |
| 大文件附件 | 抽出到 `large_files` 表，content 里只留 `file_id` 引用 | `src/large-files.ts` |
| 媒体消息（MEDIA:/path、data:URL） | 专门的 regex 过滤（`MEDIA_PATH_RE`, `EMBEDDED_DATA_URL_RE`）不塞进 LLM | `src/compaction.ts` L~180 |
| 压缩 LLM 失败 | 4 级降级：normal→aggressive→fallback→capped；`fallback` 用确定性截断兜底 | `src/compaction.ts` |
| 摘要超过目标 token | `capSummaryText()` 硬截断并标注 `[Capped from X to Y]` | `src/compaction.ts` L90 |
| 重复同一段 | `previousSummary` 链条保证连续性 | `src/compaction.ts` resolvePriorLeafSummaryContext |
| DAG 完整性校验 | `integrity.ts`（600 行）独立模块 | `src/integrity.ts` |

### 3.6 Bootstrap（从既有 session 文件冷启动）

`conversation_bootstrap_state` 表追踪文件偏移和 mtime。插件第一次见到一个 session 文件时，会从头 ingest，但每次只处理新增的部分（`last_processed_offset`）。重启后可增量继续，不会重复压缩。

---

## 4. Assembler：组装最终 LLM Context

文件：`src/assembler.ts`（1088 行）

### 4.1 流程

```
1. 读 context_items ORDER BY ordinal
2. 对每个 item:
   - type=message → 从 messages + message_parts 还原成 AgentMessage（带 tool_call/result/reasoning）
   - type=summary → 渲染成 XML:
     <summary id="sum_xxx" kind="condensed" depth="3"
              descendant_count="576" earliest_at="..." latest_at="...">
       <parents>
         <summary_ref id="sum_parentA" />
         ...
       </parents>
       <content>
         ... summary 正文 ...
       </content>
     </summary>
3. 调 sanitizeToolUseResultPairing() 修复 tool_use/tool_result 配对
4. 构造 systemPromptAddition:
   - 包含 "## LCM Recall" 章节
   - 深压缩时（depth≥2 或 condensed≥2）额外注入 "Uncertainty checklist"
   - 列出 lcm_grep / lcm_describe / lcm_expand_query 三件套
5. 返回 { messages, systemPromptAddition }
```

### 4.2 System Prompt 注入（实装文字）

浅压缩：
```
## LCM Recall
Summaries above are compressed context — maps to details, not the details themselves.

**Recall priority:** Use LCM tools first for compacted conversation history...

**Tool escalation:**
1. lcm_grep — search by regex or full-text across messages and summaries
2. lcm_describe — inspect a specific summary (cheap, no sub-agent)
3. lcm_expand_query — deep recall: spawns bounded sub-agent, expands DAG,
                     returns answer with cited summary IDs (~120s, don't ration it)
...

**For precision/evidence questions** (exact commands, SHAs, paths, timestamps,
config values, root-cause chains): expand before answering.
Do not guess from condensed summaries — expand first or state uncertainty.
```

深压缩额外追加：
```
⚠ Deeply compacted context — expand before asserting specifics.

**Uncertainty checklist (run before answering):**
- Am I relying on an older summary even though newer evidence disagrees?
- Am I making exact factual claims from a condensed summary?
- Could compaction have omitted a crucial detail?
- Would this answer fail if the user asks for proof?

If yes to any → expand first.

**Do not guess** exact commands, SHAs, file paths, timestamps, config values,
or causal claims from condensed summaries.
```

**这是个关键工程设计**：prompt 内容随上下文压缩状态动态变化，不是静态的。ZeroClaw 目前没有这层——所有 session system prompt 一视同仁。

---

## 5. Expansion：四个 LCM 工具的实装

### 5.1 `lcm_grep`

文件：`src/tools/lcm-grep-tool.ts`（214 行）

```
输入: { pattern, mode: regex|full_text, scope: messages|summaries|both,
        conversationId?, allConversations?, since?, before?, limit? }

实装:
- 模式 regex: 通过 RetrievalEngine，messages/summaries 表上做正则
- 模式 full_text:
  * 如果 FTS5 可用: MATCH sanitizeFts5Query(pattern)
  * 否则退化到 LIKE + buildLikeSearchPlan（full-text-fallback.ts）
  * CJK 支持: containsCjk() 检测，走不同分词策略

返回: 匹配片段 + snippet + summaryId/messageId
```

### 5.2 `lcm_describe`

文件：`src/tools/lcm-describe-tool.ts`（240 行）

```
输入: { id: "sum_xxx" | "file_xxx", tokenCap? }

实装:
- summary: 查 summaries + summary_parents + summary_messages 拿子树 manifest
  返回: kind, depth, descendant_count, earliest_at, latest_at, token_count,
       path lineage, 子节点预览（budget-fit 标注: 哪些能塞进 tokenCap）
- file: 查 large_files，返回 storage_uri, exploration_summary, byte_size

是 lcm_expand_query 的"预估工具"：决定要不要真展开。
```

### 5.3 `lcm_expand`

文件：`src/tools/lcm-expand-tool.ts`（448 行）+ `expansion.ts`（383 行）

```
输入: {
  summaryIds?: string[],    // 或
  query?: string,           // grep-first 模式
  maxDepth?: number,        // 默认 3
  tokenCap?: number,        // 硬预算，跨所有展开共享
  includeMessages?: boolean // 是否包含原始 message
}

实装（ExpansionOrchestrator.expand）:
1. 对每个 summaryId:
   - retrieval.expand({ summaryId, depth: maxDepth, includeMessages, tokenCap: remainingBudget })
   - 递归：从当前 summary 查 summary_parents reverse edge 找子节点，继续往下
   - 深度达到 maxDepth 或 token 用尽时停
2. 收集所有 citedIds
3. 返回 ExpansionResult { expansions[], citedIds[], totalTokens, truncated }
```

**关键：token 预算是全局共享的**，不是每个 summary 各自有预算。所以展开越多 summary，单个展开被切得越浅——避免一次展开吞掉整个 context window。

### 5.4 `lcm_expand_query`

文件：`src/tools/lcm-expand-query-tool.ts`（793 行）—— 最复杂

```
输入: { prompt, summaryIds? | query?, maxTokens?, ... }

实装:
1. 如果 query 给了：先 grep → 拿到 top summaryIds
2. ExpansionOrchestrator.describeAndExpand() 拉出 DAG 子树
3. distillForSubagent() 把展开结果压缩成 plain text payload
4. 决策路由（decideLcmExpansionRouting）:
   - 如果 payload 够小 + prompt 简单 → 内联返回
   - 否则 → spawn 专门子 agent（Claude Sonnet 常用），
     传 distilled payload + prompt，让子 agent 回答
5. 权限（expansion-auth.ts）: 签发一次性 grantId，
   限制子 agent 只能展开授权范围的 summary，防止绕过 conversationId 边界
6. 防递归（lcm-expansion-recursion-guard.ts）: 子 agent 自己不能再调 lcm_expand_query
   —— 否则一个 query 可能炸开成无限递归
```

**这是整个 LCM 里最聪明的部分**：它不是"把所有 summary 都展开塞给你"，而是**"用一个专门的子 agent 来读扩展出来的内容，只返回回答"**——把"解压缩"和"提问"分开，主 agent 不污染上下文。

---

## 6. 配置参数（当前系统实测）

`/home/ec2-user/.openclaw/openclaw.json`:

```json5
{
  plugins: {
    slots: { contextEngine: "lossless-claw" },
    entries: {
      "lossless-claw": {
        enabled: true,
        config: {
          freshTailCount: 32,           // 保护最近 32 条消息不被压缩
          contextThreshold: 0.75,       // 达到 budget 75% 触发压缩
          incrementalMaxDepth: -1       // -1 = 不做 incremental condense
        }
      }
    }
  }
}
```

刘奎设成 `incrementalMaxDepth: -1` —— 只做 leaf 压缩，不自动做 condensed 上卷。DAG 不会无限长高。上卷走 `compact`/`compactFullSweep` 手动触发路径。

### 默认值（`src/db/config.ts`）

| 参数 | 默认值 | 含义 |
|------|--------|------|
| `contextThreshold` | 0.75 | 触发阈值（占 budget 比例） |
| `freshTailCount` | 8（OpenClaw override 32） | 保护尾部消息数 |
| `leafMinFanout` | 视内部 | 合并 leaf 的最小数量 |
| `condensedMinFanout` | 视内部 | 合并 condensed 的最小数量 |
| `incrementalMaxDepth` | 1（OpenClaw override -1） | 每次 leaf 后顺带上卷层数 |
| `leafChunkTokens` | 20000 | 单次 leaf 处理原始 token 数 |
| `leafTargetTokens` | 600 | leaf summary 目标 token |
| `condensedTargetTokens` | 900 | condensed summary 目标 token |
| `maxRounds` | 10 | 一次 sweep 最多跑多少轮 |
| `summaryMaxOverageFactor` | 3 | 摘要超出目标的容忍倍数（超了 capSummaryText） |

---

## 7. 可以借鉴给 ZeroClaw 的 5 个硬核点

（这部分是判断题，不是描述题——结合 ZeroClaw 当前 `one2x/compaction.rs` 来对比）

### 7.1 ⭐⭐⭐ DAG 存储 + 可展开

**现状**：ZeroClaw 压缩后产出 `[CONTEXT SUMMARY ...]` 一条 assistant message 回写到 history；raw messages 丢进 Memory::store 走记忆层。**不可逆展开**，子模型看到的 summary 是"硬压缩"。

**LCM 做法**：压缩不删 raw，只改 `context_items` 指针。需要时 `lcm_expand` 拉原消息/子 summary 回来。

**建议**：ZeroClaw 可以加一个独立 SQLite DB（和 session state 分开）做 summary→message 的映射表。不需要抄整个 DAG，**先抄第一层**（leaf→raw message）就能让 agent 有"深挖"能力。

### 7.2 ⭐⭐⭐ Dynamic System Prompt Addition

**现状**：ZeroClaw 无论压缩多少层，system prompt 不变。

**LCM 做法**：assembler 根据当前 DAG 最大 depth + condensed 数量，注入不同强度的 "expand first" 规则。

**建议**：这是**零成本增强**，ZeroClaw `agent_sse.rs` 或 prompt 构建层加 10 行代码就能做。当 history 里出现 `[CONTEXT SUMMARY ...]` 时，在 system prompt 末尾追加"遇到压缩段落先调用 memory recall 工具验证"。

### 7.3 ⭐⭐⭐ Distilled Sub-Agent for Recall

**现状**：ZeroClaw 的 memory recall 是直接把 top-k 结果塞回主 agent context，污染主 agent。

**LCM 做法**：`lcm_expand_query` 专门起一个子 agent 读扩展内容，只返回回答文本，主 agent context 不污染。

**建议**：值得抄。ZeroClaw 已经有 sub-agent 架构（orchestrator），加一个 `memory_deep_recall` tool，内部 spawn 子 agent 读扩展 summary，只返回聚焦答案。

### 7.4 ⭐⭐ 多级降级 summarize

**现状**：ZeroClaw `compaction.rs` 有 PASS/FAIL 质检门（这已经比 LCM 好），但失败只是 retry，没有分级降级。

**LCM 做法**：`normal → aggressive → fallback(截断) → capped`，最坏情况也能拿到一个摘要。

**建议**：ZeroClaw PASS/FAIL 失败后可以走 aggressive prompt 再试一次，再失败走确定性截断，不要整个放弃。

### 7.5 ⭐⭐ DAG 完整性校验模块

**现状**：ZeroClaw 没有。

**LCM 做法**：600 行独立 `integrity.ts`，定期校验 `summary_parents` 边无环、`context_items` ordinal 连续、`summary_messages` 引用的 message 存在。

**建议**：ZeroClaw 如果上 DAG 存储，这个是配套必须的——SQLite 层可以用 FOREIGN KEY + triggers 实装一部分。

---

## 8. 不建议抄的部分

- **FTS5 虚拟表**：ZeroClaw 用 Rust，rusqlite 的 FTS5 支持要额外开编译 flag，不划算。CJK 支持还需要 tokenizer 定制。**建议**走向量索引（LanceDB 已有）+ LIKE 兜底即可。
- **expansion-auth.ts 的 grantId 机制**：太复杂，ZeroClaw 目前子 agent 边界清晰，不需要这层授权。
- **bootstrap 从 session 文件冷启动**：ZeroClaw session 状态在自己 schema 里，不需要兼容外部 session 文件。

---

## 9. 与之前 comparison 文档的修正

| 之前 comparison 里的说法 | 实际情况 |
|-------------------------|---------|
| "LCM 是 OpenClaw 核心功能" | ❌ 是插件（`@martian-engineering/lossless-claw`） |
| "OpenClaw 双模式：Markdown + LCM DAG" | ✅ 两个是分开的：MEMORY.md/memory/ 是用户可编辑文件，LCM 是运行时 session DAG。两者不交叉。 |
| "压缩是无损的" | ⚠️ 更准确说法：**可展开/可逆引用**。raw message 不删，summary 指向它们，但 summary 本身是 LLM 生成的有损压缩 |
| "lcm_expand 立即返回展开内容" | ⚠️ `lcm_expand_query` 实际是 spawn 子 agent 异步处理，~120s 量级 |

---

## 10. 建议 ZeroClaw 三步抄作业

**Phase 1（1 周，零风险）**：
- 在 `agent_sse.rs` / prompt 构建处加 dynamic system prompt：history 含 `[CONTEXT SUMMARY]` 时追加"先 memory recall 再回答"规则
- `compaction.rs` PASS/FAIL 失败后加一轮 aggressive retry（改 prompt 指令）

**Phase 2（2-3 周）**：
- 新建 `zeroclaw-memory-storage` crate：独立 SQLite DB，schema 模仿 LCM 的 summaries + summary_messages 两表
- 压缩时同时写 SQLite（不改现有 Memory::store 路径），加 `memory_expand_summary(summary_id)` tool

**Phase 3（可选，1-2 月）**：
- DAG 上卷（condensed 层）
- Expansion 子 agent 路由（distillForSubagent 等价实装）

---

## 参考

- 源码位置: `/home/ec2-user/.openclaw/extensions/lossless-claw/src/` （v0.5.3，MIT）
- 上游: https://github.com/martian-engineering/lossless-claw （待确认是否开源）
- OpenClaw 官方 context-engine 文档: `openclaw/docs/concepts/context-engine.md`
- 本文作者实际读取的关键文件:
  - `db/migration.ts` (L430-570) — DAG schema DDL
  - `compaction.ts` (L398-520) — compact 主循环
  - `assembler.ts` (L60-135) — systemPromptAddition 动态注入
  - `expansion.ts` (L109-225) — ExpansionOrchestrator
  - `tools/lcm-expand-query-tool.ts` (793 行) — 子 agent 路由

