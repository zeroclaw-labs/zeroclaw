# ZeroClaw Upstream Sync SOP

## Overview

本项目是 ZeroClaw 的 One2X 定制 fork。One2X 代码通过 `one2x` Cargo feature flag 控制编译，但实现已经是 **hybrid layout**，不是“所有代码都在 `src/one2x/`”：

- root crate 只保留必须依赖 root 类型的 handler / wiring
- runtime、channels、gateway、config 各自保留自己的 canonical one2x 实现
- upstream-owned 文件里尽量只留小型 hook / 注册调用

**核心原则：业务实现放到最贴近所属 crate 的 `one2x` 模块；上游文件只留最小接线。**

## Architecture

### Root crate (`src/one2x/`) — v6 layout

v6 把大部分 one2x 代码搬到了子 crate。根 crate 下只剩真正依赖根 crate 类型
（`approval`、`tools`、`gateway::AppState`）的那部分：

```
src/one2x/
├── mod.rs              # 协调中心：register_gateway_routes() 注册 IoC 路由闭包
├── web_channel.rs      # WebSocket 实时通道 (F-04)
├── agent_sse.rs        # SSE Agent 端点 (F-05)
└── gateway_ext.rs      # pairing-aware WS handler 包装
```

### Sub-crate one2x 模块 — canonical v6 实现位置

| 功能 | 位置 |
|------|------|
| Runtime-side pre-LLM hygiene / planning detection | `crates/zeroclaw-runtime/src/one2x/mod.rs` |
| 多阶段分块压缩 + 质量验证 | `crates/zeroclaw-runtime/src/one2x/compaction.rs`（hook 在 `crates/zeroclaw-runtime/src/agent/context_compressor.rs`） |
| Channel-side session hygiene / tool pairing / fast approval | `crates/zeroclaw-channels/src/one2x.rs` |
| Gateway 路由 IoC 钩子 | `crates/zeroclaw-gateway/src/one2x.rs` |
| Root-crate route/channel wiring | `src/one2x/mod.rs` |
| `WebChannelConfig` 定义 | `crates/zeroclaw-config/src/scattered_types.rs`（`#[cfg(feature = "one2x")]`） |

### 历史（2026-04-16 清理）

根 crate 曾经有 `agent_hooks.rs` / `session_hygiene.rs` / `tool_pairing.rs` /
`compaction.rs` / `config.rs` 五个文件，都是 v5 时代的实现。v6 拆 crate 后它们
已经和对应子 crate 的 canonical 版本分叉，且没有任何活的 `mod` 声明引用它们。
2026-04-16 的 polish 阶段把这五个死文件从磁盘上删了 —— git 历史保留完整，
需要回溯可以 `git log --all -- src/one2x/{agent_hooks,session_hygiene,tool_pairing,compaction,config}.rs`。
遇到上游合并冲突时，请去上面那张表对应的子 crate 位置查看 canonical 实现，
**不要**尝试复活根 crate 的 v5 副本。

### Upstream Integration Points

上游文件中的自定义改动，按冲突风险排列：

| 文件 | 改动类型 | 行数 | 冲突风险 |
|------|---------|------|---------|
| `Cargo.toml` | feature 声明 | 2 | 极低 |
| `src/lib.rs` | module 声明 | 2 | 极低 |
| `src/main.rs` | module 声明 + `register_integrations()` | 3 | 极低 |
| `Dockerfile.debian` | feature 参数 | 1 | 极低 |
| `crates/zeroclaw-channels/src/orchestrator/mod.rs` | injected web channel + 3 session hygiene hooks + fast approval + hooks firing | ~40 | 中 |
| `crates/zeroclaw-infra/src/session_store.rs` | `session_path` visibility: `fn` → `pub fn` | 1 | 极低 |
| `crates/zeroclaw-runtime/src/agent/loop_.rs` | planning_nudge + pre-LLM hygiene hooks + hooks firing + skill creation | ~40 | 中 |
| `crates/zeroclaw-runtime/src/agent/context_compressor.rs` | context floor + key-facts flush + multi-stage compaction hook | ~50 | 中 |
| `crates/zeroclaw-config/src/schema.rs` | cfg-gated `channels.web` 字段 + alias | 6 | 低 |
| `crates/zeroclaw-gateway/src/lib.rs` | `extend_router()` 调用 + gateway stop hook | ~10 | 低 |
| `crates/zeroclaw-api/src/memory_traits.rs` | `list_by_prefix` default method | ~13 | 低 |
| `crates/zeroclaw-memory/src/sqlite.rs` | `list_by_prefix` SQLite impl | ~43 | 低 |
| `crates/zeroclaw-memory/src/lib.rs` | audited memory 工厂接入 | ~40 | 低 |
| `crates/zeroclaw-runtime/src/daemon/mod.rs` | heartbeat validation | ~22 | 低 |
| `crates/zeroclaw-runtime/src/tools/shell.rs` | session ID env | ~9 | 低 |
| `crates/zeroclaw-runtime/src/tools/skill_tool.rs` | session ID env | ~9 | 低 |
| `crates/zeroclaw-providers/src/reliable.rs` | stream idle timeout + retry jitter | ~40 | 低 |
| `crates/zeroclaw-runtime/src/agent/tool_execution.rs` | case-insensitive tool lookup | ~5 | 低 |
| `crates/zeroclaw-runtime/src/skills/creator.rs` | post-create skill audit | ~14 | 低 |
| `crates/zeroclaw-runtime/src/heartbeat/engine.rs` | `with_hooks()` + heartbeat tick hook | ~15 | 低 |

## Routine Sync Workflow

### Prerequisites

```bash
# 确保 upstream remote 已配置
git remote -v | grep upstream
# 如果没有：
git remote add upstream https://github.com/zeroclaw-labs/zeroclaw.git
```

### Step 1: Prepare

```bash
# 确保在最新的 custom 分支上
git checkout one2x/custom-v7  # 或当前最新版本
git pull origin one2x/custom-v7

# 检查上游更新量
git fetch upstream master
git rev-list --count HEAD..upstream/master
```

### Step 1.5: Feature Parity Audit ⚠️ MANDATORY

**在开始 cherry-pick 之前**，检查上游是否已实现我们的自定义功能。
如果上游采纳了某个功能，需要在 cherry-pick 时跳过对应 commit（否则会引入重复逻辑）。

```bash
# 运行自动检查（upstream 已在 Step 1 中 fetch 过，无需重复 fetch）
./dev/check-parity.sh

# 如果需要 fetch（未执行 Step 1 的情况）
./dev/check-parity.sh --fetch
```

**结果解读：**

| 输出 | 含义 | 行动 |
|------|------|------|
| `✓ KEEP` | 上游无相同实现 | 继续 cherry-pick，无需操作 |
| `⚠ REVIEW` | 上游可能有类似实现 | 按下方流程人工判断 |

**REVIEW 处理流程：**

```bash
# 1. 查看 custom-features.md 中该功能的「等价性标准」
cat dev/custom-features.md

# 2. 对比上游实现
git show upstream/master:src/path/to/file | grep -A 20 "pattern"

# 3. 判断：
#    - 等价（全部标准满足）→ 执行 custom-features.md 中的「删除步骤」
#      然后在 Step 2 的 cherry-pick 中跳过该功能的 commit
#    - 不等价 → 保留我们的代码，更新下方 Feature Parity Tracking 表格

# 4. 更新 custom-features.md 中该功能的「状态」字段
```

**重要**：即使检查结果全部为 KEEP，也要记录本次 upstream commit hash 供下次对比：

```bash
git rev-parse upstream/master > dev/.last-parity-check
```

### Step 2: Run Merge Script

```bash
# 自动创建新版本分支并 cherry-pick
./dev/merge-upstream.sh

# 或指定版本号
./dev/merge-upstream.sh v5

# 先预览不执行
./dev/merge-upstream.sh --dry-run
```

**脚本自动完成：**
1. Fetch upstream/master
2. 创建 `one2x/custom-vN` 分支
3. 逐个 cherry-pick 自定义 commit
4. 运行 `cargo fmt` + `cargo clippy` + `cargo test`
5. 报告结果

### Step 3: Handle Conflicts (if any)

脚本会精确报告冲突位置：

```
[ERROR] CONFLICT during cherry-pick of: abc1234 feat: xxx
Conflicted files:
  crates/zeroclaw-gateway/src/lib.rs
```

**解决方法：**

```bash
# 1. 打开冲突文件，查找 <<<< 标记
# 2. 保留上游代码 + 我们的注册调用
# 3. 标记解决
git add crates/zeroclaw-gateway/src/lib.rs
git cherry-pick --continue

# 4. 如果需要放弃重来
git cherry-pick --abort
git checkout one2x/custom-v7
git branch -D one2x/custom-v8
```

### Step 4: Verify

```bash
# 有 feature
cargo check --features one2x
cargo clippy --features one2x -- -D warnings

# 无 feature (确保不破坏上游)
cargo check

# 测试
cargo test
```

### Step 5: Push & Deploy

```bash
# 推送新分支
git push -u origin one2x/custom-vN
```

### Step 6: Build & Deploy Image

```bash
# 1. 准备构建上下文（在 videoclaw-ops 目录）
cd videoclaw-ops/apps/zeroclaw
rsync -a --exclude='target' --exclude='.git' --exclude='node_modules' \
  /path/to/zeroclaw/ zeroclaw-src/

# 2. 构建镜像（约 13 分钟）
docker build --platform linux/amd64 \
  -t loveops-prod-acr-registry.ap-southeast-1.cr.aliyuncs.com/platform/zeroclaw:vN.0.0 .

# 3. 推送镜像
TOKEN=$(aliyun cr GetAuthorizationToken --InstanceId cri-e71dfjucxw8ipc7m --region ap-southeast-1 | python3 -c "import json,sys; print(json.load(sys.stdin)['AuthorizationToken'])")
echo "$TOKEN" | docker login --username=cr_temp_user --password-stdin loveops-prod-acr-registry.ap-southeast-1.cr.aliyuncs.com
docker push loveops-prod-acr-registry.ap-southeast-1.cr.aliyuncs.com/platform/zeroclaw:vN.0.0

# 4. 清理构建残留
rm -rf zeroclaw-src

# 5. 更新 manifests（自动同步 ZEROCLAW_IMAGE + image-puller）
./scripts/update-zeroclaw-version.sh vN.0.0
./scripts/update-zeroclaw-version.sh vN.0.0 --env prod  # 如果需要

# 6. 提交推送
git add -A
git commit -m "deploy: upgrade ZeroClaw to vN.0.0"
git push
```

**自动化要点**：
- `scripts/update-zeroclaw-version.sh` 确保 ZEROCLAW_IMAGE 和 image-puller DaemonSet 同步更新
- Image-puller 预热新镜像到所有节点，后续 agent 创建秒级启动
- 已有 agent 保持旧版本运行，通过 idle auto-suspend → wake 自动升级

## Conflict Resolution Guide

### 最常见冲突场景

| 场景 | 位置 | 解决方法 |
|------|------|---------|
| 上游改 channel orchestrator | `crates/zeroclaw-channels/src/orchestrator/mod.rs` | 保留上游新增 + 重新挂回我们的 injected web channel / session hygiene / fast approval hooks |
| 上游改 router 结构 | `crates/zeroclaw-gateway/src/lib.rs` | 保留上游改动 + 保留我们的 `extend_router()` 调用 |
| 上游改启动顺序 | `src/main.rs` | 保留上游改动 + 确认 `register_integrations()` 仍在 gateway/runtime 启动前执行 |
| 上游改 ChannelsConfig | `crates/zeroclaw-config/src/schema.rs` | 保留上游字段 + 保留我们的 cfg-gated `channels.web` 字段 |
| 上游改 compaction 流程 | `crates/zeroclaw-runtime/src/agent/context_compressor.rs` | 保留上游压缩逻辑 + 重新挂回 key-facts flush / multi-stage hook |

### 冲突解决原则

1. **上游代码优先** — 任何功能性代码以上游为准
2. **保留注册调用** — 我们的改动仅是 1-3 行注册调用，添加到合适位置
3. **cfg 门控不可丢** — 所有自定义改动必须有 `#[cfg(feature = "one2x")]`
4. **编译两次验证** — 有 feature 和无 feature 都必须编译通过

## Adding New Custom Features

### 添加新的自定义功能

```bash
# 1. 先判断功能属于哪个 crate：
#    - root-only HTTP / AppState / approval 依赖 → src/one2x/
#    - runtime 行为钩子 → crates/zeroclaw-runtime/src/one2x/
#    - channel / orchestrator 行为 → crates/zeroclaw-channels/src/one2x.rs
#    - gateway IoC → crates/zeroclaw-gateway/src/one2x.rs

# 2. 把 canonical 实现放在所属 crate 的 one2x 模块

# 3. 只在 upstream-owned 文件里加最小 hook：
#    - #[cfg(feature = "one2x")] 门控
#    - 1~数行注册 / 调用
#    - 在文档里记录集成点
```

### 新功能 checklist

- [ ] canonical 实现在正确的 crate 边界内
- [ ] root-crate 只保留必须的 wiring / handler
- [ ] 通过注册函数或最小 hook 接入
- [ ] 上游文件改动有 cfg 门控
- [ ] `cargo check` 有/无 feature 都通过
- [ ] `cargo clippy -D warnings` 通过
- [ ] `cargo test` 通过
- [ ] `dev/custom-features.md` / `dev/UPSTREAM-SYNC-SOP.md` / `src/one2x/mod.rs` 注释已同步

## Contributing Back to Upstream

对于通用功能（不含 One2X 业务逻辑），应考虑贡献回上游以减少 fork 维护负担：

**可贡献的候选功能：**
- `list_by_prefix` memory trait extension
- Lark/Feishu cron delivery
- Heartbeat validation improvements

**流程：**
1. 在上游项目创建 PR
2. PR 合并后，这部分代码从自定义 commit 中移除
3. Fork 维护负担减少

## Emergency Procedures

### 回滚到上一个版本

```bash
git checkout one2x/custom-v7  # 回到上一个稳定版本
git push -f origin one2x/custom-v7:one2x/custom-v8  # 如果已推送了 v8
```

### 上游有 breaking change

```bash
# 1. 先在 dry-run 模式检查
./dev/merge-upstream.sh --dry-run

# 2. 如果冲突太多，可以手动 rebase
git checkout -b one2x/custom-v5 upstream/master
git cherry-pick <commit-1> <commit-2> ...  # 逐个应用，逐个解决

# 3. 实在不行，重新从上游开始，手动移植功能
```

## Feature Parity Tracking

> **自动化检查**：使用 `./dev/check-parity.sh` 取代手动表格扫描。
> **功能详细档案**：见 `dev/custom-features.md`（含等价性标准和删除步骤）。

### 历史采纳记录

已被上游采纳并从我们代码中移除的功能：

| 自定义功能 | 上游 PR/版本 | 移除时间 |
|-----------|------------|---------|
| Lark/Feishu cron delivery | 上游原生支持 | v5 |
| `strip_think_tags_inline` | 上游 #5505 | v5 |
| Drop orphan tool_results in Agent trim | 上游 #5485 | v5 |
| Clear history on orphan tool_call_id loop | 上游 #5537 cherry-pick | v5.2.0 |

### 当前保留功能

完整列表见 `dev/custom-features.md`，当前有 19 个活跃功能（F-01 至 F-19）。
其中 F-16 至 F-19 为自进化 Phase 0 改动（2026-04-16），无 feature flag，直接改进上游代码。
运行 `./dev/check-parity.sh` 查看实时状态。

## Bedrock Compatibility Fixes (custom patches)

上游的 trim/orphan 修复不覆盖所有代码路径。以下修复是我们针对 Bedrock 严格验证的补充：

| 修复 | 文件 | 说明 |
|------|------|------|
| Tool name `.` → `__` | `tools/skill_tool.rs` | Bedrock 只允许 `[a-zA-Z0-9_-]+` |
| Precise orphan detection in `trim_history` | `agent/history.rs` | 使用 JSON 结构检测而非字符串匹配 |
| Channel orphan error recovery | cherry-pick #5537 | 检测到 orphan 错误时清空 history |

**注意**：上游 `agent.rs` 的 trim 修复 (#5485) 只覆盖 Agent 模式路径，不覆盖 `history.rs` 的 `trim_history`（channel 模式使用）。我们的修复补充了 channel 路径。

## Version History

| 版本 | 基线 | 说明 |
|------|------|------|
| v3 | upstream ~v0.5.9 | 初始自定义版本 |
| v4 | upstream master (252 commits ahead) | cherry-pick 合并 + clippy fix |
| v4 (refactor) | 同上 | one2x 模块重构，注册函数模式 |
| v5 | upstream master (60 commits ahead of v4) | 合并上游，精简已被采纳的自定义代码 |
| v5.1.0 | v5 + tool name fix | Bedrock tool name `.` → `__` |
| v5.2.0 | v5.1.0 + orphan fixes | 精确 orphan 检测 + channel error recovery + #5537 cherry-pick |
| v5.3.0 | v5.2.0 + session hygiene | 工具结果截断 + 压缩后文件同步 + 启动时自修复；修复 `__` 分隔符测试 |
| v5.4.0 | v5.3.0 + agent hooks 接线 | planning 检测 + fast approval 正式生效；Parity Audit SOP + check-parity.sh |
| v6 | upstream sync + workspace extraction | canonical one2x 代码迁移到 runtime/channels/gateway 子 crate |
| v7 | 远端开源 ZeroClaw 合并基线 + V6 功能回灌 | 当前维护基线；使用 `one2x/custom-v7` |
