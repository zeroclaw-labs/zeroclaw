# JhedaiClaw 发布流程

本操作手册定义了维护者的标准发布流程。

最后验证时间：**2026 年 2 月 21 日**。

## 发布目标

- 保持发布可预测和可重复。
- 仅从 `master` 分支已有的代码发布。
- 发布前验证多目标产物。
- 即使在高 PR 量下也保持定期发布节奏。

## 标准节奏

- 补丁/次要版本：每周或每两周一次。
- 紧急安全修复：按需发布。
- 不要等待非常大的提交批次积累。

## 工作流契约

发布自动化位于：

- `.github/workflows/pub-release.yml`
- `.github/workflows/pub-homebrew-core.yml`（手动 Homebrew 公式 PR，机器人所有）

模式：

- 标签推送 `v*`：发布模式。
- 手动触发：仅验证或发布模式。
- 每周计划：仅验证模式。

发布模式护栏：

- 标签必须符合类 semver（语义化版本）格式 `vX.Y.Z[-后缀]`。
- 标签必须已存在于 origin 上。
- 标签提交必须可以从 `origin/master` 访问。
- GitHub Release 发布完成前，匹配的 GHCR 镜像标签（`ghcr.io/<所有者>/<仓库>:<标签>`）必须可用。
- 发布前验证产物。

## 维护者流程

### 1) `master` 分支预检查

1. 确保最新 `master` 分支上的必需检查为绿色。
2. 确认没有高优先级事件或已知回归未解决。
3. 确认最近 `master` 提交上的安装程序和 Docker 工作流健康。

### 2) 运行验证构建（不发布）

手动运行 `Pub Release`：

- `publish_release`: `false`
- `release_ref`: `master`

预期结果：

- 完整目标矩阵构建成功。
- `verify-artifacts` 确认所有预期归档文件存在。
- 不发布 GitHub Release。

### 3) 创建发布标签

在同步到 `origin/master` 的干净本地检出上：

```bash
scripts/release/cut_release_tag.sh vX.Y.Z --push
```

此脚本强制要求：

- 工作树干净
- `HEAD == origin/master`
- 标签不重复
- 符合类 semver 标签格式

### 4) 监控发布运行

标签推送后，监控：

1. `Pub Release` 发布模式
2. `Pub Docker Img` 发布作业

预期发布输出：

- 发布归档文件
- `SHA256SUMS`
- `CycloneDX` 和 `SPDX` SBOM（软件物料清单，Software Bill of Materials）
- cosign 签名/证书
- GitHub Release 说明 + 资产

### 5) 发布后验证

1. 验证 GitHub Release 资产可下载。
2. 验证已发布版本的 GHCR 标签（`vX.Y.Z`）和发布提交 SHA 标签（`sha-<12位>`）。
3. 验证依赖发布资产的安装路径（例如引导二进制下载）。

### 6) 发布 Homebrew Core 公式（机器人所有）

手动运行 `Pub Homebrew Core`：

- `release_tag`: `vX.Y.Z`
- 先运行 `dry_run`: `true`，再运行 `false`

非试运行所需的仓库设置：

- 密钥：`HOMEBREW_CORE_BOT_TOKEN`（专用机器人账户的令牌，而非个人维护者账户）
- 变量：`HOMEBREW_CORE_BOT_FORK_REPO`（例如 `jhedaiclaw-release-bot/homebrew-core`）
- 可选变量：`HOMEBREW_CORE_BOT_EMAIL`

工作流护栏：

- 发布标签必须匹配 `Cargo.toml` 版本
- 公式源 URL 和 SHA256 从标记的 tarball 更新
- 公式许可证标准化为 `Apache-2.0 OR MIT`
- PR 从机器人 fork 提交到 `Homebrew/homebrew-core:master`

## 紧急/恢复路径

如果标签推送发布在产物验证后失败：

1. 在 `master` 上修复工作流或打包问题。
2. 以发布模式重新运行手动 `Pub Release`，参数：
   - `publish_release=true`
   - `release_tag=<现有标签>`
   - 发布模式下 `release_ref` 会自动固定到 `release_tag`
3. 重新验证发布的资产。

## 运营注意事项

- 保持发布变更小且可回滚。
- 每个版本优先使用一个发布 Issue/检查清单，以便交接清晰。
- 避免从临时功能分支发布。
