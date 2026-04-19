# External Capability Adoption Backlog

只记录已经通过 daily intel 初筛、值得继续投入时间的候选能力。

## 状态定义

- `new`：刚进入 backlog，尚未评估
- `rfc`：正在做 mini-RFC / 设计说明
- `poc`：正在做 PoC
- `verify`：正在跑真实 case / 回归
- `adopted`：已进入产品主干
- `watch`：暂不做，但持续观察
- `rejected`：确认不做

## 记录模板

| ID | Source | Topic | Candidate | Why Now | Target Layer | Status | Owner | Evidence |
|----|--------|-------|-----------|---------|--------------|--------|-------|----------|
| ADP-000 | meta-harness-tbench2-artifact | harness bootstrap | Initial environment snapshot injection before first tool turn | 我们的 coding/terminal agent 仍会浪费首轮在 `pwd/ls/which` 这类探索上 | runtime prompt assembly + sandbox introspection | new | TBD | daily intel |
| ADP-001 | hermes-agent | learning loop | Autoresearch-style learnings ledger | 我们缺稳定的 daily -> weekly 学习闭环 | docs/process + runtime hooks | new | TBD | intel note |
| ADP-002 | openclaw | context management | Expandable context summary / deep recall | 我们当前 compaction 仍是单向损失 | memory + tools + compaction | watch | TBD | `docs/one2x/porting-rfc.md` |
| ADP-003 | claude-code | execution model | Stronger plan-to-execution guardrails | 我们已有 planning nudge，但可继续收紧交互约束 | runtime loop + approval UX | new | TBD | intel note |

## 使用规则

1. 只有 `ADOPT` 或 `EXPERIMENT` 级别的发现才能进入本表。
2. 每条记录必须写清楚 `Why Now`，不能只写“别人做了”。
3. `Target Layer` 必须明确到 crate / 模块层级，避免抽象空转。
4. `adopted` 后，要把结果同步到：
   - `dev/custom-features.md`
   - 对应 RFC / 实现文档
   - 真实回归记录
