# JhedaiClaw 项目分类快照（2026-02-18）

截止日期：**2026 年 2 月 18 日**。

本快照捕获开放 PR/Issue 信号，以指导文档和信息架构工作。

## 数据来源

通过 GitHub CLI 从 `jhedai/jhedaiclaw` 收集：

- `gh repo view ...`
- `gh pr list --state open --limit 500 ...`
- `gh issue list --state open --limit 500 ...`
- 对于文档相关项使用 `gh pr/issue view <id> ...`

## 仓库动态

- 开放 PR：**30**
- 开放 Issue：**24**
- Star：**11,220**
- Fork：**1,123**
- 默认分支：`master`
- GitHub API 上的许可证元数据：`Other`（未检测到 MIT）

## PR 标签压力（开放 PR）

按频率排列的主要信号：

1. `risk: high` — 24
2. `experienced contributor` — 14
3. `size: S` — 14
4. `ci` — 11
5. `size: XS` — 10
6. `dependencies` — 7
7. `principal contributor` — 6

对文档的影响：

- CI/安全/服务变更仍然是高 churn 领域。
- 面向运维人员的文档应优先考虑"变更内容"可见性和快速故障排除路径。

## Issue 标签压力（开放 Issue）

按频率排列的主要信号：

1. `experienced contributor` — 12
2. `enhancement` — 8
3. `bug` — 4

对文档的影响：

- 功能和性能请求仍然超过说明文档。
- 故障排除和操作参考应保持在顶部导航附近。

## 与文档相关的开放 PR

- [#716](https://github.com/jhedai/jhedaiclaw/pull/716) — OpenRC 支持（服务行为/文档影响）
- [#725](https://github.com/jhedai/jhedaiclaw/pull/725) — shell 补全命令（CLI 文档影响）
- [#732](https://github.com/jhedai/jhedaiclaw/pull/732) — CI Action 替换（贡献者工作流文档影响）
- [#759](https://github.com/jhedai/jhedaiclaw/pull/759) — 守护进程/渠道响应处理修复（渠道故障排除影响）
- [#679](https://github.com/jhedai/jhedaiclaw/pull/679) — 配对锁定计数变更（安全行为文档影响）

## 与文档相关的开放 Issue

- [#426](https://github.com/jhedai/jhedaiclaw/issues/426) — 明确要求更清晰的功能文档
- [#666](https://github.com/jhedai/jhedaiclaw/issues/666) — 操作手册和告警/日志指南请求
- [#745](https://github.com/jhedai/jhedaiclaw/issues/745) — Docker 拉取失败（`ghcr.io`）表明有部署故障排除需求
- [#761](https://github.com/jhedai/jhedaiclaw/issues/761) — Armbian 编译错误凸显了平台故障排除需求
- [#758](https://github.com/jhedai/jhedaiclaw/issues/758) — 存储后端灵活性请求影响配置/参考文档

## 推荐的文档待办事项（优先级顺序）

1. **保持文档信息架构稳定和清晰**
   - 维护 `docs/SUMMARY.md` + 分类索引作为规范导航。
   - 保持本地化中心与相同的顶层文档映射对齐。

2. **保护运维人员的可发现性**
   - 在顶层 README/中心中保留 `operations-runbook` + `troubleshooting` 链接。
   - 问题重复出现时添加平台特定的故障排除片段。

3. **积极跟踪 CLI/配置漂移**
   - 当触及这些表面的 PR 合并时，更新 `commands/providers/channels/config` 参考。

4. **区分当前行为与提案**
   - 在安全路线图文档中保留提案横幅。
   - 保持运行时契约文档（`config/runbook/troubleshooting`）标记清晰。

5. **维护快照规范**
   - 保持快照带日期戳且不可变。
   - 为每个文档冲刺创建新的快照文件，而非修改历史快照。

## 快照说明

这是有时间限制的快照（2026-02-18）。规划新的文档冲刺前请重新运行 `gh` 查询。
