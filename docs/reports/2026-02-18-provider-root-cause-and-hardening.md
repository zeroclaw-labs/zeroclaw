# ZeroClaw 深度排查与收尾报告（2026-02-18）

## 目标

围绕用户反馈完成三条主线排查与修复：

1. `reply_to -> reply_target` 编译回归是否已经解决。
2. DeepSeek“说一句动一下”（非持续工具执行）的根因与修复。
3. 全 providers 的模型目录是否存在过时/失效项，是否已淘汰并替换为当前可用项。

---

## 核心根因与修复

### 1) 编译回归：`reply_to` / `reply_target`

- 上游已修复并合并：
  - PR #541: https://github.com/zeroclaw-labs/zeroclaw/pull/541
  - PR #768: https://github.com/zeroclaw-labs/zeroclaw/pull/768
- 历史相关：
  - Issue #537: https://github.com/zeroclaw-labs/zeroclaw/issues/537
  - Issue #761: https://github.com/zeroclaw-labs/zeroclaw/issues/761

### 2) DeepSeek“说一句动一下”

#### 根因

- Channel agent loop 在部分路径中没有稳定走 provider 的统一 `chat(ChatRequest)` 工具调用链。
- OpenAI-compatible provider 遇到不支持 native `tools` 参数时，回退到纯 `chat_with_history`，导致工具协议语义丢失，表现为“只答一句，不持续执行任务”。

#### 已修复

- 统一 agent loop 走 `Provider::chat`，保持 native tool calling 语义。
- OpenAI-compatible provider 在 `tools` schema 不支持时，会先注入 prompt-guided tool instructions，再回退 `chat_with_history`。
- 增加对应测试覆盖，防止回归。

### 3) 模型目录过时/失效

#### 已处理

- onboarding default/curated 统一改到“当前可用批次”，并淘汰已废弃项：
  - Moonshot：移除 `kimi-latest`、`kimi-thinking-preview`，改为 `kimi-k2.5` / `kimi-k2-thinking` / `kimi-k2-0905-preview`
  - MiniMax：移除 `MiniMax-M2.1-lightning`，改为 `MiniMax-M2.5-highspeed`
  - GLM/ZAI：保留 `glm-5` / `glm-4.7` / `glm-4.5-air`
  - OpenRouter 默认更新至 `anthropic/claude-sonnet-4.6`
  - 新增 NVIDIA / Astrai curated defaults
- 修正 `Config::default()` 默认模型到 `anthropic/claude-sonnet-4.6`。

---

## 收尾新增改进（稳定性）

- 修复 `doctor models` 在 async runtime 中触发 blocking 客户端时可能出现的 runtime drop panic：
  - `models refresh` 与 `doctor models` 在 `main.rs` 使用 `tokio::task::spawn_blocking` 执行同步刷新逻辑。
- 改进 `doctor models` 错误分类：
  - 采用 error chain 聚合文本分类（auth/access/skipped/error），避免只显示顶层上下文导致误判。

---

## 线上核验与快照

### 官方/实时来源

- DeepSeek 模型与映射：
  - https://api-docs.deepseek.com/quick_start/pricing
- Moonshot 模型与废弃公告（`kimi-latest` 在 2026-01-28 停用）：
  - https://platform.moonshot.ai/docs/introduction
- MiniMax 模型总览：
  - https://platform.minimax.io/docs/guides/models-intro
- Z.AI 快速开始（GLM-5 主推）：
  - https://docs.z.ai/guides/overview/quick-start
- 可公开拉取目录：
  - OpenRouter: https://openrouter.ai/api/v1/models
  - Venice: https://api.venice.ai/api/v1/models
  - NVIDIA: https://integrate.api.nvidia.com/v1/models
  - Astrai: https://as-trai.com/v1/models

### doctor models 快照（本地执行）

- `nvidia`：`ok`（缓存命中，184 models）
- `moonshot`：`auth/access`（无 key 时提示 endpoint 需要 key）
- `perplexity`：`skipped`（当前不支持 live model discovery）

---

## 本轮代码位置（关键）

- `src/agent/loop_.rs`
- `src/providers/compatible.rs`
- `src/providers/reliable.rs`
- `src/onboard/wizard.rs`
- `src/main.rs`
- `src/doctor/mod.rs`
- `src/config/schema.rs`

---

## 验证

- `cargo check --locked --bin zeroclaw` ✅
- `cargo check --locked --tests --message-format short` ✅
- `cargo run --locked --bin zeroclaw -- doctor models --provider nvidia --use-cache` ✅

---

## 后续建议（可选）

1. 增加 `bench providers` 统一测时子命令（首 token / 完成时延 / 工具回合数）。
2. 对 `doctor models --provider <x>` 在 `skipped/auth-access` 场景返回 0 exit code（保留信息但不视为 hard fail）。
3. 更新 `cost` 默认价格表到当前主流模型定价，以免误导预算。

---

## 安全提醒

若曾在外部对话中暴露 API key，请立即轮换并废弃旧 key。
