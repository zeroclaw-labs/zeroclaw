# Daily Intel

本目录存放 One2X / Medeo 团队对外部 agent 项目的 **每日情报扫描**。

## 目的

让我们对以下项目形成稳定学习节奏：
- 官方开源 `zeroclaw`
- `meta-harness-tbench2-artifact`
- `hermes-agent`
- `openclaw`
- `claude-code`

这里记录的是 **情报**，不是已经落地的能力。

## 本地源码位置

daily intel 默认直接基于本地 workspace 扫描，而不是临时联网拉源码：

| Source | Local Path | Notes |
|---|---|---|
| zeroclaw open-source / our fork baseline | `/Users/liukui/Documents/GitHub/zeroclaw` | 当前本地仓库含 `origin` + `upstream` |
| hermes-agent | `/Users/liukui/Documents/GitHub/hermes-agent` | git clone |
| openclaw | `/Users/liukui/Documents/GitHub/openclaw` | git clone |
| meta-harness-tbench2-artifact | `/Users/liukui/Documents/GitHub/meta-harness-tbench2-artifact` | git clone |
| claude-code | `/Users/liukui/Documents/GitHub/claude-code` | 当前是源码快照，不是 git clone |

说明：
- 对 git clone，优先用本地 branch / HEAD / 上次扫描记录做 diff。
- 对非 git 快照，按目录快照或手工刷新后的文件差异做扫描。

## 对比前预备动作

正确流程不是“直接看当前本地目录”，而是：

1. 先把本地代码库更新到最新
2. 再基于更新后的本地代码做 diff 和对比学习

推荐命令：

```bash
git -C /Users/liukui/Documents/GitHub/zeroclaw fetch --all --prune
git -C /Users/liukui/Documents/GitHub/hermes-agent fetch --all --prune
git -C /Users/liukui/Documents/GitHub/openclaw fetch --all --prune
git -C /Users/liukui/Documents/GitHub/meta-harness-tbench2-artifact fetch --all --prune
```

如果某个本地仓库就是拿来跟踪默认分支的，也可以在确认没有本地改动后执行：

```bash
git -C <repo> pull --ff-only
```

`claude-code` 当前是本地源码快照，不是 git clone。
对它的正确做法不是 `git fetch`，而是先刷新本地快照，再参与当天比较。

## 文件命名

每个工作日一个文件：

```text
YYYY-MM-DD.md
```

例如：

```text
2026-04-19.md
2026-04-20.md
```

## 推荐结构

```md
# Daily Intel - 2026-04-19

## Scan Scope
- zeroclaw open-source:
- meta-harness-tbench2-artifact:
- hermes-agent:
- openclaw:
- claude-code:

## High-Signal Findings
- [ADOPT] ...
- [EXPERIMENT] ...
- [WATCH] ...
- [IGNORE] ...

## Mapping To Our Product
- 当前问题：
- 可落点：
- 风险：

## Backlog Updates
- ADP-00X added / updated

## Next Action
- ...
```

## 标记含义

- `ADOPT`：问题存在且价值明确，建议进入 backlog
- `EXPERIMENT`：值得做 PoC，但收益/风险还不够确定
- `WATCH`：先观察，不立刻投入
- `IGNORE`：与当前主链路无关

## 规则

1. 每条发现必须写“它解决什么问题”，不能只贴 commit。
2. 每条发现必须写“如果做，落在哪一层”，至少到 crate / 模块级。
3. 只有 `ADOPT` / `EXPERIMENT` 可以更新 `dev/ADOPTION-BACKLOG.md`。
4. 情报不等于承诺，进入 backlog 也不等于一定实现。
5. 官方开源 `zeroclaw` 和我们当前产品仓库要分开记录，避免把 upstream 变化误记成我方现状。
6. 本目录默认以“更新后的本地 workspace”为准；不要直接基于过期本地代码做结论。
