# Actions 源政策

本文档定义了本仓库当前的 GitHub Actions 源代码控制政策。

## 当前政策

- 仓库 Actions 权限：已启用
- 允许的 Actions 模式：已选择

已选白名单（质量门控、Beta 发布和稳定发布工作流中当前使用的所有 Actions）：

| Action | 使用位置 | 目的 |
|--------|---------|---------|
| `actions/checkout@v4` | 所有工作流 | 仓库检出 |
| `actions/upload-artifact@v4` | release、promote-release | 上传构建产物 |
| `actions/download-artifact@v4` | release、promote-release | 下载构建产物用于打包 |
| `dtolnay/rust-toolchain@stable` | 所有工作流 | 安装 Rust 工具链（1.92.0） |
| `Swatinem/rust-cache@v2` | 所有工作流 | Cargo 构建/依赖缓存 |
| `softprops/action-gh-release@v2` | release、promote-release | 创建 GitHub Releases |
| `docker/setup-buildx-action@v3` | release、promote-release | Docker Buildx 设置 |
| `docker/login-action@v3` | release、promote-release | GHCR 认证 |
| `docker/build-push-action@v6` | release、promote-release | 多平台 Docker 镜像构建和推送 |

等效的白名单模式：

- `actions/*`
- `dtolnay/rust-toolchain@*`
- `Swatinem/rust-cache@*`
- `softprops/action-gh-release@*`
- `docker/*`

## 工作流

| 工作流 | 文件 | 触发条件 |
|----------|------|---------|
| 质量门控 | `.github/workflows/checks-on-pr.yml` | 指向 `master` 的拉取请求 |
| Beta 发布 | `.github/workflows/release-beta-on-push.yml` | 推送到 `master` |
| 稳定发布 | `.github/workflows/release-stable-manual.yml` | 手动 `workflow_dispatch` |

## 变更控制

记录每个政策变更时包含：

- 变更日期/时间（UTC）
- 操作者
- 原因
- 白名单变更（新增/移除的模式）
- 回滚说明

使用以下命令导出当前有效政策：

```bash
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions/selected-actions
```

## 护栏

- 任何新增或变更 `uses:` Action 源的 PR 必须包含白名单影响说明。
- 新的第三方 Action 在加入白名单前需要显式的维护者评审。
- 仅为验证过的缺失 Action 扩展白名单；避免宽泛的通配符例外。

## 变更日志

- 2026-03-10：重命名工作流 — CI → 质量门控（`checks-on-pr.yml`）、Beta 发布 → Release Beta（`release-beta-on-push.yml`）、升级发布 → Release Stable（`release-stable-manual.yml`）。向质量门控添加了 `lint` 和 `security` 作业。添加了跨平台构建（`cross-platform-build-manual.yml`）。
- 2026-03-05：完整工作流重构 — 将 22 个工作流替换为 3 个（CI、Beta 发布、升级发布）
    - 移除不再使用的模式：`DavidAnson/markdownlint-cli2-action@*`、`lycheeverse/lychee-action@*`、`EmbarkStudios/cargo-deny-action@*`、`rustsec/audit-check@*`、`rhysd/actionlint@*`、`sigstore/cosign-installer@*`、`Checkmarx/vorpal-reviewdog-github-action@*`、`useblacksmith/*`
    - 新增：`Swatinem/rust-cache@*`（替代 `useblacksmith/*` rust-cache 分支）
    - 保留：`actions/*`、`dtolnay/rust-toolchain@*`、`softprops/action-gh-release@*`、`docker/*`
- 2026-03-05：CI 构建优化 — 添加了 mold 链接器、cargo-nextest、CARGO_INCREMENTAL=0
    - 由于 GHA 缓存后端不稳定导致构建失败，移除了 sccache

## 回滚

紧急解除阻塞路径：

1. 临时将 Actions 政策设置回 `all`。
2. 识别缺失条目后恢复选中的白名单。
3. 记录事件和最终白名单变更。
