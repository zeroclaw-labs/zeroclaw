# OpenClaw 架构权威版 — LCM + Memory 深度合并

> 作者：刘小奎 🦞  2026-04-18
> 合并自 `openclaw-deepdive.md`（全景 1425 行）+ `openclaw-lcm-deepdive.md`（LCM 专论 593 行）
> 目标：一份文档讲清 OpenClaw 到底是什么、各层职责、哪些是核心 / 哪些是插件 / 哪些是可抄的工程点

---

## 0. 一句话总纲

**OpenClaw = pi-coding-agent 核心（平坦 CompactionEntry 压缩 + Provider 插件化 Memory）+ lossless-claw 插件（DAG 可展开摘要）+ memory-wiki 插件（人类可读知识层）+ active-memory 插件（实时注入）。**

理解这句话，就理解了 OpenClaw 所有架构。下面只是展开。

---

## 1. 分层总览

```
┌──────────────────────────────────────────────────────────────────┐
│ 应用层：Agent Loop + Tools + Channels                            │
│  ├ pi-coding-agent（核心调度、tool call、provider 路由）         │
│  └ Extensions（Feishu/Telegram/Discord/IM 插件等）               │
├──────────────────────────────────────────────────────────────────┤
│ 压缩层（ContextEngine slot，可插拔）                             │
│  ├ legacy（默认）：平坦 CompactionEntry 链 + firstKeptEntryId    │
│  └ lossless-claw（插件）：DAG 摘要 + 可双向展开 + 4 工具         │
├──────────────────────────────────────────────────────────────────┤
│ 记忆层（Memory Provider，多实现并存）                            │
│  ├ memory-core：MEMORY.md / memory/YYYY-MM-DD.md 文件即记忆      │
│  ├ memory-lancedb：向量检索（FTS5 + embedding，加速层）         │
│  ├ memory-wiki：结构化知识（证据/新鲜度/冲突），git-diff 审计    │
│  └ active-memory：主动监控并提升重要记忆到 MEMORY.md             │
├──────────────────────────────────────────────────────────────────┤
│ 存储层                                                           │
│  ├ 文件系统（source of truth）：.md / .json / 扩展配置           │
│  └ SQLite（lossless-claw 专用）：DAG + FTS5 + 原消息映射         │
└──────────────────────────────────────────────────────────────────┘
```

**关键：核心 Memory 的 source of truth 是 Markdown 文件；向量是加速层。SQLite 只服务 lossless-claw 插件。**

---

## 2. 压缩层：核心 vs lossless-claw

### 2.1 pi-coding-agent 核心（legacy 引擎，默认开启）

- 位置：`docs/concepts/compaction.md`、`plugin-sdk/src/plugins/compaction-provider.d.ts`
- 机制：**平坦链式** `CompactionEntry`，记录 `firstKeptEntryId` 作为保留起点；压缩时把保留点之前的消息整块替换为一段摘要，保留点之后的消息照常进入 context
- 可配：
  - `identifierPolicy: "strict" | "off" | "custom"` —— ID 保护策略（压缩时 sum_xxx 等标识符是否硬保护）
  - `agents.defaults.compaction.model` —— 为压缩指定专用模型（更便宜/更大）
  - `registerCompactionProvider()` —— 插件可替换整套压缩逻辑
- **Pre-compaction memory flush**：压缩前静默跑一次 memory 写入（ZeroClaw 抄的就是这个）
- 没有 DAG、没有"展开"概念——摘要一旦生成就是死的

### 2.2 lossless-claw 插件（`@martian-engineering/lossless-claw v0.5.3`）

- 位置：`~/.openclaw/extensions/lossless-claw/`（独立 npm 包，MIT 许可，Martian Engineering 出品）
- 启用：`plugins.slots.contextEngine = "lossless-claw"`
- 规模：16,577 行 TypeScript
- 文件结构（关键）：
  - `engine.ts` 2838 行 — 总编排
  - `compaction.ts` 1663 行 — 多级压缩（leaf → condensed）
  - `summarize.ts` 1391 行 — LLM 摘要 + 重试
  - `assembler.ts` 1088 行 — 组装 context
  - `summary-store.ts` 1133 行 — DAG 读写
  - `conversation-store.ts` 892 行 — 原消息与 part
  - `integrity.ts` 600 行 — DAG 完整性校验
  - `expansion.ts` 383 行 — 展开子树
  - `retrieval.ts` 361 行 — FTS5/regex 检索

**核心数据流：**
1. 每条消息 → `ingest` → 写 `messages` + `message_parts` + `context_items` 末尾追加 `message` 项
2. Token 到达 `contextThreshold * budget`（默认 0.75）→ `compact` 触发
3. 压缩分两类：
   - **Leaf pass**：旧消息折叠成 `leaf summary`（depth=0），保留 summary→messages 映射（`summary_messages` 表）
   - **Condensed pass**：多个 leaf 上卷成 `condensed summary`（depth≥1），DAG 边在 `summary_parents`
4. `context_items` 重写：被折叠的 message 指针替换成 summary 指针；`freshTailCount=32` 条尾部保留原始 message
5. `assemble` 按 `context_items` 顺序拼 prompt，summary 渲染为 `<summary id="sum_xxx">…</summary>`
6. **原始消息永不删**——只是 `context_items` 指针不再指向它

**4 个工具（暴露给 Agent）：**

| 工具 | 用途 | 成本 |
|------|------|------|
| `lcm_grep` | regex / 全文检索 summaries + messages | 便宜 |
| `lcm_describe` | 查 summary 元数据 + 子树 manifest | 便宜 |
| `lcm_expand` | 按 ID 展开 DAG 拿子 summary / 原 message | 中 |
| `lcm_expand_query` | 展开 + 起子 agent 蒸馏出聚焦答案 | 贵（~120s） |

**Dynamic System Prompt**：context 里 depth≥2 或 condensed ≥ 2 时，lossless-claw 自动注入一段 uncertainty checklist，提示 LLM"涉及精确事实先展开"。

### 2.3 对比

| 维度 | legacy（pi-coding-agent 核心） | lossless-claw（插件） |
|------|-------------------------------|----------------------|
| 数据结构 | 平坦 CompactionEntry 链 | SQLite DAG |
| 可逆性 | 不可逆 | 可双向展开（唯一） |
| 原消息保留 | 替换 | 永久保留（映射表） |
| 工具数 | 0 | 4 |
| 多层摘要 | 否 | 是（leaf / condensed / 深层 condensed） |
| 子 agent 蒸馏 | 无 | 有（`lcm_expand_query`） |

**重要结论**：LCM 不是 OpenClaw 原生能力，是第三方插件的贡献。`<summary id="sum_xxx">` 出现在上下文里，是因为装了 lossless-claw。

---

## 3. DAG Schema（lossless-claw SQLite，DDL 摘录）

```sql
-- 会话
CREATE TABLE conversations (
  conversation_id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL,
  session_key TEXT, title TEXT,
  bootstrapped_at TEXT, created_at TEXT, updated_at TEXT
);

-- 原消息（永不删）
CREATE TABLE messages (
  message_id INTEGER PRIMARY KEY AUTOINCREMENT,
  conversation_id INTEGER NOT NULL,
  seq INTEGER NOT NULL,
  role TEXT CHECK (role IN ('system','user','assistant','tool')),
  content TEXT NOT NULL,
  token_count INTEGER NOT NULL, created_at TEXT,
  UNIQUE (conversation_id, seq)
);

-- 消息组件（tool_call / reasoning / patch / file / subtask / …）
CREATE TABLE message_parts (
  part_id TEXT PRIMARY KEY,
  message_id INTEGER NOT NULL,
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
  metadata TEXT,
  UNIQUE (message_id, ordinal)
);

-- 摘要节点（DAG 顶点）
CREATE TABLE summaries (
  summary_id TEXT PRIMARY KEY,           -- "sum_<hex>"
  conversation_id INTEGER NOT NULL,
  kind TEXT CHECK (kind IN ('leaf','condensed')),
  depth INTEGER NOT NULL DEFAULT 0,       -- 0 = leaf
  content TEXT NOT NULL,
  token_count INTEGER NOT NULL,
  earliest_at TEXT, latest_at TEXT,
  descendant_count INTEGER,
  descendant_token_count INTEGER,
  source_message_token_count INTEGER,
  file_ids TEXT NOT NULL DEFAULT '[]'
);

-- Leaf → 原消息 映射（可逆的根基）
CREATE TABLE summary_messages (
  summary_id TEXT NOT NULL,
  message_id INTEGER NOT NULL,
  ordinal INTEGER NOT NULL,
  PRIMARY KEY (summary_id, message_id)
);

-- DAG 边（多对多；condensed 可多 parent）
CREATE TABLE summary_parents (
  summary_id TEXT NOT NULL,
  parent_summary_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL,
  PRIMARY KEY (summary_id, parent_summary_id)
);

-- 当前喂给 LLM 的顺序列表（message + summary 混编）
CREATE TABLE context_items (
  conversation_id INTEGER NOT NULL,
  ordinal INTEGER NOT NULL,
  item_type TEXT CHECK (item_type IN ('message','summary')),
  message_id INTEGER, summary_id TEXT,
  PRIMARY KEY (conversation_id, ordinal)
);

-- 附件
CREATE TABLE large_files (
  file_id TEXT PRIMARY KEY,
  conversation_id INTEGER NOT NULL,
  ...
);

-- 全文检索
CREATE VIRTUAL TABLE messages_fts  USING fts5(...);
CREATE VIRTUAL TABLE summaries_fts USING fts5(...);
```

**值得记的 invariant**：
- `messages` 是永不可变的 append-only log
- `summary_messages` 提供 leaf → 原消息的精确回溯
- `summary_parents` 是 DAG 边集合，没有循环
- `context_items` 是"当前视图"——每次 compaction 都会重写
- `bootstrapped_at` 标记会话是否做过初次装载（决定 first-turn vs subsequent-turn 行为）

---

## 4. Memory 层：四个扩展的职责分工

### 4.1 memory-core — 文件即记忆

- 位置：`extensions/memory-core/`
- 核心设计：**Markdown 文件是 source of truth**
  - `MEMORY.md` —— 用户级长期记忆（手写 + agent 自动 promote）
  - `memory/YYYY-MM-DD.md` —— 每日笔记（今天 + 昨天自动加载到 context）
  - `DREAMS.md` —— 实验性"做梦"日志
- 工具：
  - `memory_search` —— 混合检索（向量+关键词，auto-detect）
  - `memory_get` —— 读文件或指定行范围
- 为什么这是好设计：
  1. 用户可 `vim`、可 `git diff`、可 `grep`，不是黑盒
  2. Agent 说"记一下 X"立即生效（append 一行到 MEMORY.md）
  3. 向量索引是**加速层，不是存储层**——向量挂了记忆不丢

### 4.2 memory-lancedb — 向量加速

- 位置：`extensions/memory-lancedb/`
- 本质：把 memory-core 的 Markdown 切片后送进 LanceDB（SQLite 风格的向量库）
- 嵌入模型：`azure/text-embedding-3-small`（1536 维，默认线上配置）
- **加速语义**：源文件改了，向量索引会异步重建；向量不在不影响功能（降级到 FTS5/grep）
- 已知坑：`@lancedb/lancedb` 每次 openclaw npm update 之后要手动 `npm install` 重装

### 4.3 memory-wiki — 结构化知识

- 位置：`extensions/memory-wiki/`
- 解决的问题：纯 markdown 记忆不处理"矛盾、新鲜度、证据来源"
- 能力：
  - 每个条目带 `evidence`（出处）、`confidence`、`created_at`、`conflicts_with`
  - 工具：`wiki_search` / `wiki_get` / `wiki_apply` / `wiki_lint`
  - `wiki_lint` 会扫描过期条目、无证据条目、冲突条目
- 相当于"给长期记忆加了 git + linter"

### 4.4 active-memory — 实时提升

- 位置：`extensions/active-memory/`
- 监控当前会话里哪些记忆"在高频被用"，提议 promote 到 `MEMORY.md`
- 与 memory-wiki 配合：promote 时自动带上 evidence 链

---

## 5. ZeroClaw 视角：能抄什么 / 不能抄什么

### 5.1 可直接抄的（高 ROI 低风险）

| 机制 | 来源 | 落地建议 |
|------|------|---------|
| Dynamic system prompt 注入（检测到 summary 触发 uncertainty checklist） | lossless-claw | 1-2 天，零风险 |
| Markdown 即记忆（source of truth）| memory-core | ZeroClaw 已有 `MarkdownMemory` backend，点亮即可 |
| Silent memory flush（压缩前静默写 memory）| pi-coding-agent | 3-5 天 |
| FTS5 + embedding 混合检索 | memory-lancedb | ZeroClaw SqliteMemory 已有 FTS5，加 embedding BLOB 合并即可 |

### 5.2 值得抄但要工程设计的（中 ROI 中风险）

| 机制 | 来源 | 落地建议 |
|------|------|---------|
| 单层 LCM expand（summary → 原消息）| lossless-claw 精简版 | ~10 天；新 `context_summary.rs` 模块 + `summary_messages` 映射表 + `context_expand` 工具 |
| 子 agent 蒸馏式深度回忆 | `lcm_expand_query` | ~15 天；独立子 agent，隔离 context 防止污染主线 |

### 5.3 不建议抄的（负 ROI）

- **多层 DAG 上卷**（leaf → condensed → 更深 condensed）：ZeroClaw 单 session 一般 <24h，单层 expand 解决 90% 场景；DAG 上卷带来的收益抵不过存储+维护成本
- **Memory-wiki 的证据系统**：需要 agent 有显式"记录证据来源"的训练，现有模型直接用效果差

### 5.4 ZeroClaw 当前短板（确认版）

之前 `agent-intelligence-comparison.md` 说 ZeroClaw Memory"只是向量碎片、缺人类可读中间层"——**过度贬低**。读 `crates/zeroclaw-memory/` 源码后更准确的描述是：

- 已有：5 backend（SqliteMemory/MarkdownMemory/QdrantMemory/LucidMemory/NoneMemory）、RetrievalPipeline（混合检索）、consolidation/decay/conflict、knowledge_graph + knowledge_extraction、AuditedMemory、NamespacedMemory、injection_guard
- 真正缺的：
  1. 没有"MEMORY.md / daily notes"这类**人类可读中间层的惯例**（API 支持，但没有上层 UX）
  2. 没有"可展开摘要"（compaction 是单向、有损）
  3. 没有"动态 system prompt 注入"机制

---

## 6. OpenClaw 内部依赖关系（文件层）

```
pi-coding-agent (core)
  │
  ├─ depends on: plugin-sdk（接口定义）
  │
  ├─ slot: contextEngine
  │    ├─ legacy（built-in，默认）
  │    └─ lossless-claw（插件，独立包）
  │
  ├─ slot: memoryProvider
  │    ├─ memory-core（extension，built-in）
  │    ├─ memory-lancedb（extension）
  │    ├─ memory-wiki（extension）
  │    └─ active-memory（extension）
  │
  └─ extensions:
       ├─ channel-feishu / telegram / discord / …
       ├─ lark-cli
       └─ 其它第三方
```

**配置**：`~/.openclaw/openclaw.json` + `plugins.slots.*` + `extensions.*`

---

## 7. 参考文档对应关系

| 文档 | 作用 | 状态 |
|------|------|------|
| `openclaw-deepdive.md`（1425 行）| 全景参考，16 章覆盖所有模块 | 保留（原始资料） |
| `openclaw-lcm-deepdive.md`（593 行）| LCM 源码专论 + DDL + 关键函数 | 保留（LCM 细节参考） |
| `openclaw-architecture-final.md`（本文）| 合并版、分层图、ZeroClaw 视角 | **首选入口** |
| `agent-intelligence-comparison.md` | 三家对比（Hermes/ZeroClaw/OpenClaw）| 已修正 LCM + Memory 错误 |
| `zeroclaw-porting-rfc.md` | P1/P2/P3 porting roadmap | 工程落地方案 |

**阅读顺序建议**：先看本文 → 有细节问题查两份 deepdive → 做决策参考 comparison → 动手开 porting RFC。

---

## 8. 修订记录

- **2026-04-18 02:15 UTC** v1.0：创建。合并 deepdive 全景版 + LCM 专论版。明确核心/插件边界，纠正先前"LCM = 核心特性"和"ZeroClaw Memory 原始"两处误解。
