# ZeroClaw 自主化治理 SOP

## 目标

把当前仓库从“长期跟随 upstream 的 One2X fork”升级成“我们自己完全控制的产品仓库”。

这意味着：
- 我们的仓库和默认分支才是 **source of truth**
- 官方开源 `zeroclaw`、`meta-harness-tbench2-artifact`、`hermes-agent`、`openclaw`、`claude-code` 都只是一类 **外部情报输入**
- 外部项目的代码和想法，必须先经过我们的评估、适配、验证，才能进入主干

## 非目标

- 不是停止学习外部项目
- 不是彻底断开 upstream `zeroclaw`
- 不是每天自动把别人的改动 merge 进来

外部项目继续看，但它们不再定义我们的路线图。

## 为什么必须切换治理模式

### 1. 继续把自己当 fork，决策权永远在外面

如果默认动作仍然是“跟 upstream 对齐，再把我们的 patch 挂上去”，那本质上还是在被上游牵着走。

更合理的模式是：
- 我们定义产品目标、架构边界、兼容策略
- upstream 只是一个 supplier
- `UPSTREAM-SYNC-SOP` 只负责“引入外部变化”，不再负责“定义产品演进方式”

### 2. “完全自控项目”不能长期依赖 upstream 品牌

`docs/maintainers/trademark.md` 已明确说明：
- `ZeroClaw` 是 ZeroClaw Labs 的商标
- fork 不应把 “ZeroClaw” 当成自己的主品牌

所以如果目标是长期完全自控，最佳实践不是继续把外部品牌当成我们的产品名，而是：
- 代码层面可保留 “based on ZeroClaw”
- 产品层面必须逐步切到我们自己的名称、镜像名、部署名、文档名

### 3. 学习必须从“追 patch”升级成“情报 -> 选择 -> 落地”

Hermes、OpenClaw、Claude Code 最有价值的不是某一个 commit，而是：
- 他们怎么定义问题
- 他们把能力放在哪一层
- 他们用什么约束避免系统失控

所以学习流程必须是：
1. 发现外部变化
2. 抽象成能力/模式
3. 映射到我们当前痛点
4. 做 PoC 和真实回归
5. 决定 adopt / experiment / watch / ignore

## 治理原则

### 原则 1：我方仓库是唯一主干

- 默认开发分支使用我方产品主干，而不是 `upstream/master`
- `one2x/custom-v7` 只作为当前切换前的 bootstrap baseline
- 所有新功能先满足我方产品需求，再考虑和 upstream 的关系

### 原则 2：外部项目是情报源，不是代码权威

- 官方开源 `zeroclaw`：看 runtime / channel / config / memory / web dashboard 的通用工程演进
- `meta-harness-tbench2-artifact`：看 benchmark-driven harness 设计、环境快照注入、prompt bootstrap、Terminal-Bench 风格评测
- `hermes-agent`：看 research loop、学习机制、身份治理、自改进流程
- `openclaw`：看插件生态、LCM、memory 扩展、渠道化能力
- `claude-code`：看 coding-agent 交互约束、审批/执行模型、工程 UX

### 原则 3：只有通过验证的能力才能进主干

任何外部能力进入我方主干前，必须满足：
- 有明确要解决的我方问题
- 有适配方案，不是机械照搬
- 有真实 case 验证
- 有回滚方式

### 原则 4：业务专属能力优先“去 fork 化”

凡是已经被证明是我方长期必需的能力，不应永远挂在“`one2x` 临时 patch”里。

应逐步区分三类：
- **Core product capability**：长期保留，应逐步去 `one2x` 化
- **Compatibility shim**：只为兼容某类 provider / upstream 结构
- **Business-only adapter**：只服务 videoclaw / Medeo 业务链路

## 角色定义

### Product Owner

负责回答：
- 当前 30 天最值得吸收的能力是什么
- 哪些外部变化只看不做
- 哪些能力应升级为产品核心

### Scout

负责每天扫描：
- 官方开源 `zeroclaw`
- `meta-harness-tbench2-artifact`
- `hermes-agent`
- `openclaw`
- `claude-code`

输出标准化情报，不直接改主干。

### Evaluator

负责把情报翻译成我方语言：
- 解决什么问题
- 放在哪一层
- 风险是什么
- 值不值得做 PoC

### Implementer

负责做最小可验证落地：
- feature branch
- 真实 case
- 回归验证
- 文档同步

## 固定产物

本 SOP 依赖下面 4 类产物：

### 1. 每日情报

位置：
- `docs/one2x/daily-intel/YYYY-MM-DD.md`

用途：
- 记录当天从五个外部项目看到的变化
- 给出 `ADOPT / EXPERIMENT / WATCH / IGNORE` 结论

默认扫描源以本地 workspace 为准：
- `zeroclaw`：`/Users/liukui/Documents/GitHub/zeroclaw`（我方 fork，本地含 `upstream` remote）
- `hermes-agent`：`/Users/liukui/Documents/GitHub/hermes-agent`
- `openclaw`：`/Users/liukui/Documents/GitHub/openclaw`
- `meta-harness-tbench2-artifact`：`/Users/liukui/Documents/GitHub/meta-harness-tbench2-artifact`
- `claude-code`：`/Users/liukui/Documents/GitHub/claude-code`（当前是本地源码快照，不是 git clone）

执行原则：
- **每次对比前先更新本地代码库**
- **更新完成后只基于本地代码做分析**

### 2. Adoption Backlog

位置：
- `dev/ADOPTION-BACKLOG.md`

用途：
- 只收“值得继续推进”的能力
- 不把噪音直接塞进 `custom-features.md`

### 3. 当前产品差异清单

位置：
- `dev/custom-features.md`

用途：
- 记录当前已经真实存在于产品代码中的 delta
- 不记录尚未落地的想法

### 4. 外部输入 SOP

位置：
- `dev/UPSTREAM-SYNC-SOP.md`

用途：
- 当我们决定“要吸收 upstream `zeroclaw` 的一部分变化”时使用
- 它是 import 机制，不再是产品治理总纲

## 迁移分阶段 SOP

### Phase 0：冻结基线

目标：先把“当前我们是谁”冻结下来。

操作：
1. 把 `one2x/custom-v7` 视为切换前基线。
2. 所有现有业务链路、部署镜像、回归 case 先在这个基线上稳定。
3. 文档中明确：
   - 当前主分支候选
   - 当前产品名候选
   - 当前外部依赖边界

完成标准：
- 有明确的基线 commit
- 有稳定部署方式
- 有完整回归 case 列表

### Phase 1：治理切换

目标：从“fork 心智”切到“自控产品心智”。

操作：
1. 选定新的产品主干分支。
   - 推荐：在 `one2x/custom-v7` 基础上切新的默认开发分支
   - 不建议继续把版本心智绑在 `custom-vN`
2. 明确项目命名策略。
   - 仓库内可保留 “derived from ZeroClaw” 说明
   - 镜像、部署、产品文档应切到我方品牌
3. 保留 `upstream` remote，但改成只读输入源。
4. 规定：任何外部变化都不能直接 merge 到产品主干，必须先过 daily intel / backlog / RFC / 验证。

完成标准：
- 团队默认开发分支不再是“fork 分支思维”
- 项目命名策略已定
- upstream 被重新定义为 vendor input

### Phase 2：工程去 fork 化

目标：把已经长期存在的 One2X 能力，从“临时 patch”升级成产品结构的一部分。

操作：
1. 逐项审查 `dev/custom-features.md`。
2. 对每个功能打标签：
   - `CORE`
   - `COMPAT`
   - `BIZ_ONLY`
3. 对 `CORE` 功能制定去 `one2x` 化计划：
   - 是否移出 `one2x` feature
   - 是否改成常驻能力
   - 是否迁移到更稳定的 crate 边界
4. 对 `COMPAT` 功能保留 vendor-oriented 注释和回归 case。
5. 对 `BIZ_ONLY` 功能继续隔离，但不再假装这是“上游同步补丁”。

完成标准：
- `custom-features.md` 中每个功能都有归类
- 至少一批长期核心能力开始脱离 fork-only 表述

### Phase 3：每日学习系统

目标：让外部学习成为稳定机制，而不是偶发研究。

#### 每日节奏

建议每天固定一次，推荐工作日上午。

#### 推荐调度方式

第一版不要追求“全自动决策”，只自动化信息收集和草稿生成，判断仍由人完成。

建议节奏：
- `09:00` 先更新本地 workspace 中五个输入源
- `09:10` 基于更新后的本地代码生成当天待分析清单
- `09:30-10:00` 完成 intel 分类和 backlog 更新

推荐做法：
- git 仓库保留为本地 clone；每次对比前先执行 `fetch --all --prune`，需要时再 `pull --ff-only`
- 非 git 输入源（如当前 `claude-code` 快照）先刷新本地源码快照，再按目录快照处理
- 调度器只负责 `diff scope + note stub`
- `ADOPT / EXPERIMENT / WATCH / IGNORE` 结论必须人工确认

等这个节奏稳定后，再考虑补自动脚本或 CI 定时任务。

标准流程：
1. 先更新本地 workspace 中五个输入源。
2. 再扫描更新后的本地代码变化。
3. 只看过去 24 小时或自上次扫描以来的增量。
4. 按主题聚类：
   - memory
   - compaction
   - planning / execution
   - tools / shell / approval
   - hooks / extensibility
   - skills / self-improvement
   - observability / ops
5. 写入当天 intel 文档。
6. 对每项变化标记：
   - `ADOPT`
   - `EXPERIMENT`
   - `WATCH`
   - `IGNORE`
7. 只有 `ADOPT` / `EXPERIMENT` 进入 backlog。

#### 五个来源的关注重点

官方开源 `zeroclaw`
- 看通用 runtime 演进、provider 兼容、安全、性能、hooks、memory

`meta-harness-tbench2-artifact`
- 重点看 environment bootstrapping、sandbox snapshot 注入、benchmark 发现的 prompt scaffold、Terminal-Bench 风格的 agent harness
- 这类输入更偏“如何提升 coding/terminal agent 首轮效率”，不是产品功能清单

`hermes-agent`
- 重点看 autoresearch loop、学习记录、身份治理、错误反馈闭环

`openclaw`
- 重点看 LCM、memory 插件、扩展系统、渠道插件化

`claude-code`
- 重点看 coding-agent 的交互约束、审批模型、执行模型、任务分解 UX
- 当前本地来源是源码快照而非 git clone，因此更适合做结构/模式扫描，而不是 commit feed 扫描

#### 每日输出必须回答的 5 个问题

1. 外部变化到底解决了什么问题？
2. 这个问题我们现在也有吗？
3. 如果做，应该落在哪个 crate / 哪一层？
4. 是 adopt、experiment、watch，还是 ignore？
5. 成功标准是什么？

### Phase 4：每周吸收评审

目标：避免“每天看很多，真正落地很少”。

每周一次，固定从 backlog 里选 1 到 3 个候选。

流程：
1. 选题
2. 做 mini-RFC 或简化设计说明
3. 建 feature branch
4. 做真实 case 验证
5. 决定：
   - merge
   - continue experiment
   - stop

完成标准：
- 每周有明确 adoption 决策
- backlog 持续收敛，而不是只增不减

## 决策门槛

任何外部能力要进入产品主干，至少满足下面 4 条中的 3 条：

- 解决当前线上/测试中真实存在的问题
- 明显提升完成率、稳定性、可维护性或开发效率
- 可以清楚落到某个 crate / 模块边界
- 有真实回归 case 能验证

以下情况默认不做：
- 只是“别人有，我们也想有”
- 对我方主链路没有帮助
- 需要大改架构但收益不清楚
- 只有 demo，没有验证路径

## 推荐目录与命名

### 每日情报目录

- `docs/one2x/daily-intel/2026-04-19.md`
- `docs/one2x/daily-intel/2026-04-20.md`

### 情报标题格式

推荐结构：
- 今日扫描范围
- 值得关注的变化
- 对我方的意义
- 建议动作
- backlog 更新

### backlog 编号

建议格式：
- `ADP-001`
- `ADP-002`

## 立即执行版（未来 10 天）

### Day 1-2

1. 确定“自控产品”的正式命名策略。
2. 确定新的默认开发分支策略。
3. 确认 `one2x/custom-v7` 只作为切换基线。

### Day 3-4

1. 建立 `docs/one2x/daily-intel/`
2. 建立 `dev/ADOPTION-BACKLOG.md`
3. 指定 daily scout 的执行时间，并固定五个输入源

### Day 5-7

1. 连续跑 3 天 daily intel
2. 每天只挑 1 到 2 个高价值点进入 backlog
3. 不急着写代码，先验证筛选机制是否有效

### Day 8-10

1. 从 backlog 里选第一个最值的能力
2. 做 PoC
3. 用真实 case 验证
4. 决定是否进入主干

## 关键结论

最佳实践不是“把 upstream sync 做得更勤”，而是：

- **我们拥有主干**
- **外部项目只提供情报**
- **每天学习，但每周只吸收少数高价值能力**
- **长期把 fork 结构改造成我方自己的产品结构**

如果以后确认走这条路，`dev/UPSTREAM-SYNC-SOP.md` 应被降级为“vendor import SOP”，而 `dev/PROJECT-OWNERSHIP-SOP.md` 才是总纲。
