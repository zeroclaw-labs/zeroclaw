# CI 工作流地图

本文档解释每个 GitHub 工作流的作用、运行时机以及是否应该阻塞合并。

关于 PR、合并、推送和发布的逐事件交付行为，请参见 [`.github/workflows/master-branch-flow.md`](../../../../.github/workflows/master-branch-flow.md)。

## 合并阻塞 vs 可选

合并阻塞检查应保持小巧且具有确定性。可选检查对自动化和维护很有用，但不应阻塞正常开发。

### 合并阻塞

- `.github/workflows/ci-run.yml`（`CI`）
    - 目的：Rust 验证（`cargo fmt --all -- --check`、`cargo clippy --locked --all-targets -- -D clippy::correctness`、变更 Rust 行的严格增量代码检查门控、`test`、发布构建冒烟测试）+ 文档变更时的质量检查（`markdownlint` 仅阻塞变更行上的问题；链接检查仅扫描变更行上添加的链接）
    - 附加行为：对于影响 Rust 代码的 PR 和推送，`CI Required Gate` 要求 `lint` + `test` + `build` 全部通过（无 PR 专属构建绕过）
    - 附加行为：变更 `.github/workflows/**` 的 PR 要求至少一名 `WORKFLOW_OWNER_LOGINS` 中的用户批准（仓库变量 fallback：`theonlyhennygod,JordanTheJet`）
    - 附加行为：代码检查门控在 `test`/`build` 之前运行；当 PR 上的代码检查/文档门控失败时，CI 会发布带有失败门控名称和本地修复命令的可操作反馈评论
    - 合并门控：`CI Required Gate`
- `.github/workflows/workflow-sanity.yml`（`Workflow Sanity`）
    - 目的：检查 GitHub 工作流文件（`actionlint`、制表符检查）
    - 推荐用于变更工作流的 PR
- `.github/workflows/pr-intake-checks.yml`（`PR Intake Checks`）
    - 目的：CI 前的安全 PR 检查（模板完整性、新增行的制表符/尾随空格/冲突标记），带有即时置顶反馈评论

### 非阻塞但重要

- `.github/workflows/pub-docker-img.yml`（`Docker`）
    - 目的：`master` PR 的 Docker 冒烟检查，仅在标签推送（`v*`）时发布镜像
- `.github/workflows/sec-audit.yml`（`Security Audit`）
    - 目的：依赖项安全公告检查（`rustsec/audit-check`，固定 SHA）和政策/许可证检查（`cargo deny`）
- `.github/workflows/sec-codeql.yml`（`CodeQL Analysis`）
    - 目的：计划/手动运行的静态分析，用于发现安全问题
- `.github/workflows/sec-vorpal-reviewdog.yml`（`Sec Vorpal Reviewdog`）
    - 目的：使用 reviewdog 注解对支持的非 Rust 文件（`.py`、`.js`、`.jsx`、`.ts`、`.tsx`）进行手动安全编码反馈扫描
    - 噪音控制：默认排除常见测试/夹具路径和测试文件模式（`include_tests=false`）
- `.github/workflows/pub-release.yml`（`Release`）
    - 目的：在验证模式下构建发布产物（手动/计划），在标签推送或手动发布模式下发布 GitHub Release
- `.github/workflows/pub-homebrew-core.yml`（`Pub Homebrew Core`）
    - 目的：针对标记发布的手动、机器人拥有的 Homebrew core 公式升级 PR 流程
    - 护栏：发布标签必须匹配 `Cargo.toml` 版本
- `.github/workflows/pr-label-policy-check.yml`（`Label Policy Sanity`）
    - 目的：验证 `.github/label-policy.json` 中的共享贡献者等级政策，并确保标签工作流使用该政策
- `.github/workflows/test-rust-build.yml`（`Rust Reusable Job`）
    - 目的：可复用的 Rust 设置/缓存 + 命令运行器，供工作流调用者使用

### 可选仓库自动化

- `.github/workflows/pr-labeler.yml`（`PR Labeler`）
    - 目的：范围/路径标签 + 大小/风险标签 + 细粒度模块标签（`<module>: <component>`）
    - 附加行为：标签描述作为悬停提示自动管理，解释每个自动判断规则
    - 附加行为：provider/config/onboard/integration 变更中与提供商相关的关键词会提升为 `provider:*` 标签（例如 `provider:kimi`、`provider:deepseek`）
    - 附加行为：层级去重仅保留最具体的范围标签（例如 `tool:composio` 会抑制 `tool:core` 和 `tool`）
    - 附加行为：模块命名空间会被压缩 — 单个具体模块保留 `prefix:component` 格式；多个具体模块会折叠为仅 `prefix`
    - 附加行为：根据已合并 PR 数量为 PR 应用贡献者等级（`trusted` ≥5 个，`experienced` ≥10 个，`principal` ≥20 个，`distinguished` ≥50 个）
    - 附加行为：最终标签集按优先级排序（`risk:*` 优先，然后是 `size:*`，然后是贡献者等级，最后是模块/路径标签）
    - 附加行为：受管理的标签颜色按显示顺序排列，当存在多个标签时产生从左到右的平滑渐变效果
    - 手动治理：支持 `workflow_dispatch` 的 `mode=audit|repair` 参数，用于检查/修复整个仓库的受管理标签元数据偏差
    - 附加行为：手动编辑 PR 标签时会自动校正风险 + 大小标签（`labeled`/`unlabeled` 事件）；当维护者有意覆盖自动化风险选择时应用 `risk: manual`
    - 高风险启发式路径：`src/security/**`、`src/runtime/**`、`src/gateway/**`、`src/tools/**`、`.github/workflows/**`
    - 护栏：维护者可以应用 `risk: manual` 冻结自动化风险重计算
- `.github/workflows/pr-auto-response.yml`（`PR Auto Responder`）
    - 目的：首次贡献者引导 + 标签驱动的响应路由（`r:support`、`r:needs-repro` 等）
    - 附加行为：根据已合并 PR 数量为 Issue 应用贡献者等级（`trusted` ≥5 个，`experienced` ≥10 个，`principal` ≥20 个，`distinguished` ≥50 个），与 PR 等级阈值完全匹配
    - 附加行为：贡献者等级标签被视为自动化管理的（PR/Issue 上的手动添加/移除会被自动校正）
    - 护栏：基于标签的关闭路由仅适用于 Issue；PR 永远不会被路由标签自动关闭
- `.github/workflows/pr-check-stale.yml`（`Stale`）
    - 目的：陈旧 Issue/PR 生命周期自动化
- `.github/dependabot.yml`（`Dependabot`）
    - 目的：分组、速率限制的依赖更新 PR（Cargo + GitHub Actions）
- `.github/workflows/pr-check-status.yml`（`PR Hygiene`）
    - 目的：提醒陈旧但活跃的 PR 在队列饥饿前 rebase/重新运行必需检查

## 触发地图

- `CI`：推送到 `master`、针对 `master` 的 PR
- `Docker`：标签推送（`v*`）用于发布，匹配的 `master` PR 用于冒烟构建，手动触发仅用于冒烟测试
- `Release`：标签推送（`v*`）、每周计划（仅验证）、手动触发（验证或发布）
- `Pub Homebrew Core`：仅手动触发
- `Security Audit`：推送到 `master`、针对 `master` 的 PR、每周计划
- `Sec Vorpal Reviewdog`：仅手动触发
- `Workflow Sanity`：当 `.github/workflows/**`、`.github/*.yml` 或 `.github/*.yaml` 变更时的 PR/推送
- `Dependabot`：所有更新 PR 指向 `master`
- `PR Intake Checks`：`pull_request_target` 事件（opened/reopened/synchronize/edited/ready_for_review）
- `Label Policy Sanity`：当 `.github/label-policy.json`、`.github/workflows/pr-labeler.yml` 或 `.github/workflows/pr-auto-response.yml` 变更时的 PR/推送
- `PR Labeler`：`pull_request_target` 生命周期事件
- `PR Auto Responder`：Issue opened/labeled、`pull_request_target` opened/labeled
- `Stale PR Check`：每日计划、手动触发
- `PR Hygiene`：每 12 小时计划、手动触发

## 快速分类指南

1. `CI Required Gate` 失败：从 `.github/workflows/ci-run.yml` 开始排查。
2. PR 上的 Docker 失败：检查 `.github/workflows/pub-docker-img.yml` 的 `pr-smoke` 作业。
3. 发布失败（标签/手动/计划）：检查 `.github/workflows/pub-release.yml` 和 `prepare` 作业输出。
4. Homebrew 公式发布失败：检查 `.github/workflows/pub-homebrew-core.yml` 摘要输出和机器人令牌/fork 变量。
5. 安全检查失败：检查 `.github/workflows/sec-audit.yml` 和 `deny.toml`。
6. 工作流语法/代码检查失败：检查 `.github/workflows/workflow-sanity.yml`。
7. PR 提交检查失败：检查 `.github/workflows/pr-intake-checks.yml` 的置顶评论和运行日志。
8. 标签政策一致性失败：检查 `.github/workflows/pr-label-policy-check.yml`。
9. CI 中的文档检查失败：检查 `.github/workflows/ci-run.yml` 中的 `docs-quality` 作业日志。
10. CI 中的严格增量代码检查失败：检查 `lint-strict-delta` 作业日志，并与 `BASE_SHA` 差异范围比较。

## 维护规则

- 保持合并阻塞检查的确定性和可复现性（适用时使用 `--locked`）。
- 发布节奏和标签规范遵循 [`docs/contributing/release-process.md`](./release-process.zh-CN.md) 的"发布前验证"要求。
- 保持 `.github/workflows/ci-run.yml`、`dev/ci.sh` 和 `.githooks/pre-push` 中的 Rust 质量政策一致（`./scripts/ci/rust_quality_gate.sh` + `./scripts/ci/rust_strict_delta_gate.sh`）。
- 使用 `./scripts/ci/rust_strict_delta_gate.sh`（或 `./dev/ci.sh lint-delta`）作为变更 Rust 行的增量严格合并门控。
- 定期通过 `./scripts/ci/rust_quality_gate.sh --strict` 运行完整严格代码检查审计（例如通过 `./dev/ci.sh lint-strict`），并在聚焦的 PR 中跟踪清理工作。
- 通过 `./scripts/ci/docs_quality_gate.sh` 保持文档 Markdown 门控的增量性（阻塞变更行问题，单独报告基线问题）。
- 通过 `./scripts/ci/collect_changed_links.py` + lychee 保持文档链接门控的增量性（仅检查变更行上添加的链接）。
- 优先使用显式工作流权限（最小权限原则）。
- 保持 Actions 源政策限制为已批准的白名单模式（参见 [`docs/contributing/actions-source-policy.md`](./actions-source-policy.zh-CN.md)）。
- 实际可行时为耗时工作流使用路径过滤器。
- 保持文档质量检查低噪音（增量 Markdown + 增量新增链接检查）。
- 保持依赖更新量可控（分组 + PR 限制）。
- 避免将引导/社区自动化与合并门控逻辑混合。
- 测试层级：`cargo test --test component`、`cargo test --test integration`、`cargo test --test system`。
- 实时测试（仅手动）：`cargo test --test live -- --ignored`。

## 自动化副作用控制

- 优先使用可手动覆盖的确定性自动化（`risk: manual`），以应对上下文复杂的情况。
- 保持自动响应评论去重，防止分类噪音。
- 保持自动关闭行为仅适用于 Issue；维护者拥有 PR 关闭/合并决定权。
- 如果自动化出错，首先校正标签，然后带着显式理由继续评审。
- 在深度评审前使用 `superseded` / `stale-candidate` 标签清理重复或休眠的 PR。
