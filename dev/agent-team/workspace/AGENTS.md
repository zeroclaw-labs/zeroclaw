# 团队成员

你可以通过 `delegate` 工具和 `swarm` 工具调度以下团队成员。根据任务性质选择合适的人。

## design — 策划师
- **专长**：游戏系统设计、GDD 文档、数值策划、玩法设计、里程碑规划
- **何时委派**：需要写策划案、定义功能需求、做系统设计、规划开发节奏
- **工具权限**：file_read, file_write, web_search_tool, memory, delegate

## dev — 开发者
- **专长**：微信小游戏开发、Canvas/WebGL 渲染、游戏逻辑、性能优化
- **何时委派**：需要写代码、实现功能、修 Bug、性能优化、技术调研
- **工具权限**：file_read, file_write, file_edit, shell, web_search_tool, memory

## art — 美术设计师
- **专长**：UI/UX 设计、HUD 布局、美术方向、配色方案、视觉反馈
- **何时委派**：需要设计界面、定义视觉风格、画 HUD 布局、写 CSS
- **工具权限**：file_read, file_write, web_search_tool, browser, memory

## intel — 情报员
- **专长**：竞品分析、市场调研、行业趋势、玩家反馈
- **何时委派**：需要调研竞品、分析市场、收集行业数据
- **工具权限**：web_search_tool, web_fetch, browser, file_read, file_write, memory

## ops — 运营官
- **专长**：用户增长、活动策划、数据分析、渠道运营
- **何时委派**：需要制定运营策略、设计活动、分析运营数据
- **工具权限**：web_search_tool, web_fetch, file_read, file_write, memory

## qa — 测试员
- **专长**：自动化测试、Bug 追踪、性能测试、兼容性测试
- **何时委派**：需要写测试、检查代码质量、做性能基准测试
- **工具权限**：shell, file_read, file_write, browser, memory

---

## 协作流程（Swarm）

| Swarm 名称 | 策略 | 成员 | 适用场景 |
|------------|------|------|---------|
| `auto` | router（AI 自动选人） | 全体 | 不确定谁做最合适，让 AI 判断 |
| `game_team` | sequential（流水线） | design → dev → art | 从策划到实现的完整开发流程 |
| `game_full` | sequential（流水线） | intel → design → dev → art → qa | 从调研到测试的全链路 |

## 委派规则

1. **单一任务** → 用 `delegate` 指定具体的人
2. **不确定谁做** → 用 `swarm` 调用 `auto`，让 AI 自动分配
3. **需要多人协作** → 用 `swarm` 调用 `game_team` 或 `game_full`
4. **需要多人并行** → 用 `delegate` 的 `parallel` 参数同时委派多人
5. **耗时任务** → 用 `delegate` 的 `background: true` 异步执行

## 示例

```
用户："帮我写一个消除类小游戏的策划案"
→ delegate(agent="design", prompt="写一个消除类微信小游戏的 GDD")

用户："分析一下羊了个羊的玩法"
→ delegate(agent="intel", prompt="分析羊了个羊的核心玩法、变现模式和用户留存策略")

用户："做一个弹珠游戏"
→ swarm(swarm="game_team", prompt="做一个物理弹珠类微信小游戏")

用户："帮我看看代码有没有性能问题"
→ delegate(agent="qa", prompt="检查项目代码的性能问题")

用户："这个任务交给最合适的人"
→ swarm(swarm="auto", prompt="<任务内容>")
```