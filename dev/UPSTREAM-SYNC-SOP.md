# ZeroClaw Upstream Sync SOP

## Overview

本项目是 ZeroClaw 的 One2X 定制 fork。所有自定义功能封装在 `src/one2x/` 模块中，通过 `one2x` Cargo feature flag 控制编译。

**核心原则：自定义实现放 `one2x/`，上游文件只留注册调用。**

## Architecture

```
src/one2x/
├── mod.rs           # 协调中心：extend_router() + extend_channels()
├── web_channel.rs   # WebSocket 实时通道 (从 channels/web.rs 迁移)
├── agent_sse.rs     # SSE Agent 端点 (从 gateway/agent_sse.rs 迁移)
├── gateway_ext.rs   # WS channel handler (从 gateway/mod.rs 提取)
└── config.rs        # WebChannelConfig (从 config/schema.rs 提取)
```

### Upstream Integration Points

上游文件中的自定义改动，按冲突风险排列：

| 文件 | 改动类型 | 行数 | 冲突风险 |
|------|---------|------|---------|
| `Cargo.toml` | feature 声明 | 2 | 极低 |
| `lib.rs` | module 声明 | 2 | 极低 |
| `main.rs` | module 声明 | 2 | 极低 |
| `Dockerfile.debian` | feature 参数 | 1 | 极低 |
| `channels/mod.rs` | `extend_channels()` 调用 + `pub(crate)` | 5 | 低 |
| `config/schema.rs` | cfg-gated `web` field + defaults | 6 | 低 |
| `gateway/mod.rs` | `extend_router()` 调用 | 3 | 低 |
| `cron/scheduler.rs` | cfg-gated match arm | 2 | 低 |
| `gateway/api.rs` | memory prefix/get 扩展 | ~50 | 中 |
| `memory/traits.rs` | `list_by_prefix` default method | ~13 | 低 |
| `memory/sqlite.rs` | `list_by_prefix` SQLite impl | ~43 | 低 |
| `daemon/mod.rs` | heartbeat validation | ~22 | 低 |
| `tools/cron_add.rs` | delivery enum values | 1 | 极低 |
| `tools/shell.rs` | session ID env | ~9 | 低 |
| `agent/loop_.rs` | task-local + helper | ~20 | 低 |

## Routine Sync Workflow

### Prerequisites

```bash
# 确保 upstream remote 已配置
git remote -v | grep upstream
# 如果没有：
git remote add upstream https://github.com/ArcadeLabsInc/zeroclaw.git
```

### Step 1: Prepare

```bash
# 确保在最新的 custom 分支上
git checkout one2x/custom-v4  # 或当前最新版本
git pull origin one2x/custom-v4

# 检查上游更新量
git fetch upstream master
git rev-list --count HEAD..upstream/master
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
  src/gateway/mod.rs
```

**解决方法：**

```bash
# 1. 打开冲突文件，查找 <<<< 标记
# 2. 保留上游代码 + 我们的注册调用
# 3. 标记解决
git add src/gateway/mod.rs
git cherry-pick --continue

# 4. 如果需要放弃重来
git cherry-pick --abort
git checkout one2x/custom-v4
git branch -D one2x/custom-v5
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
git push -u origin one2x/custom-v5

# 更新 loveops 中的镜像构建配置
# 1. loveops/.github/workflows/zeroclaw-image.yaml → 修改 default zeroclaw_ref
# 2. loveops/apps/zeroclaw/Dockerfile → 确认 features 列表包含 one2x
# 3. 触发镜像构建 workflow
```

## Conflict Resolution Guide

### 最常见冲突场景

| 场景 | 位置 | 解决方法 |
|------|------|---------|
| 上游新增 channel | `channels/mod.rs` | 保留上游新增 + 保留我们的 `extend_channels()` 调用 |
| 上游改 router 结构 | `gateway/mod.rs` | 保留上游改动 + 保留我们的 `extend_router()` 调用 |
| 上游改 ChannelsConfig | `config/schema.rs` | 保留上游字段 + 保留我们的 cfg-gated `web` field |
| 上游改 match arms | `cron/scheduler.rs` | 保留上游 arms + 保留我们的 cfg-gated `"web"` arm |

### 冲突解决原则

1. **上游代码优先** — 任何功能性代码以上游为准
2. **保留注册调用** — 我们的改动仅是 1-3 行注册调用，添加到合适位置
3. **cfg 门控不可丢** — 所有自定义改动必须有 `#[cfg(feature = "one2x")]`
4. **编译两次验证** — 有 feature 和无 feature 都必须编译通过

## Adding New Custom Features

### 添加新的自定义功能

```bash
# 1. 在 src/one2x/ 下创建新文件
touch src/one2x/my_feature.rs

# 2. 在 src/one2x/mod.rs 中声明模块
# pub mod my_feature;

# 3. 如果需要路由，在 extend_router() 中添加
# .route("/my-endpoint", get(my_feature::handler))

# 4. 如果需要 channel 注册，在 extend_channels() 中添加

# 5. 如果需要修改上游文件（不得已）：
#    - 用 #[cfg(feature = "one2x")] 门控
#    - 保持改动最小化
#    - 在 mod.rs 文档注释中记录集成点
```

### 新功能 checklist

- [ ] 实现代码在 `src/one2x/` 下
- [ ] 通过 `mod.rs` 的注册函数接入
- [ ] 上游文件改动有 cfg 门控
- [ ] `cargo check` 有/无 feature 都通过
- [ ] `cargo clippy -D warnings` 通过
- [ ] `cargo test` 通过
- [ ] `mod.rs` 文档注释更新了集成点表格

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
git checkout one2x/custom-v4  # 回到上一个稳定版本
git push -f origin one2x/custom-v4:one2x/custom-v5  # 如果已推送了 v5
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

## Version History

| 版本 | 基线 | 说明 |
|------|------|------|
| v3 | upstream ~v0.5.9 | 初始自定义版本 |
| v4 | upstream master (252 commits ahead) | cherry-pick 合并 + clippy fix |
| v4 (refactor) | 同上 | one2x 模块重构，注册函数模式 |
