# Internal Development Guidelines

本文件为公司内部二开规范，不回馈上游。

## Fork 管理

- 上游 remote: `upstream`（开源社区原始仓库）
- Fork 基线 tag: `fork-base-2026-03-12`
- 同步策略: 定期 `git merge upstream/master` 到 `master`，用 tag `upstream-sync-<date>` 标记同步点

## 分支命名

| 类型 | 前缀 | 示例 |
|------|------|------|
| 内部功能 | `feat/` | `feat/custom-auth` |
| 内部修复 | `bugfix/` | `bugfix/config-crash` |
| 回馈上游 | `contrib/` | `contrib/fix-typo-in-docs` |

规则：
- `feat/` 和 `bugfix/` 分支 PR 到 `master`
- `contrib/` 分支基于 `upstream/master` 创建，PR 到上游仓库
- `contrib/` 分支不得包含内部业务逻辑、密钥或公司信息

## 上游同步流程

```bash
git fetch upstream
git checkout master
git merge upstream/master
# 解决冲突后
git tag upstream-sync-$(date +%Y-%m-%d)
git push origin master --tags
```

## CLAUDE.md 维护规则

- 根目录 `CLAUDE.md`: 上游内容不做修改，仅末尾保留 `## Internal` 指引段落
- 本文件 (`.claude/CLAUDE.md`): 存放所有内部规范，上游不存在此文件，不会产生合并冲突
- 上游同步后如根 `CLAUDE.md` 有冲突，保留上游内容 + 重新追加 `## Internal` 段落即可
