# ZeroClaw 初学者详细教程：功能、架构、原理与改造实战

> 面向人群：第一次接触 ZeroClaw，目标是从“能用”走到“能改”。
> 
> 教程目标：帮你建立完整心智模型，并给出可直接执行的学习与改造路径。

---

## 0. 如何使用本教程

建议按“读一节、做一节”的节奏推进，不要一次性读完。

- **阶段 A（理解）**：第 1~5 章，建立总体认知。
- **阶段 B（上手）**：第 6~9 章，跑通并定位核心代码。
- **阶段 C（改造）**：第 10~14 章，做一次安全可回滚的扩展。

你可以把这份文档当作“主导航页”，配合以下参考文档：

- 命令参考：`docs/commands-reference.md`
- 配置参考：`docs/config-reference.md`
- Provider 参考：`docs/providers-reference.md`
- Channel 参考：`docs/channels-reference.md`
- 运维手册：`docs/operations-runbook.md`

---

## 1. 项目定位（一句话）

ZeroClaw 是一个 **Trait 驱动 + 工厂组装 + 安全默认收敛** 的 Rust 智能体运行时。

你可以把它理解成：

- `Provider` = 大模型接入层
- `Channel` = 消息入口/出口
- `Tool` = 行动能力（读写文件、shell、web、硬件等）
- `Memory` = 长短期记忆
- `RuntimeAdapter` = 运行环境能力边界
- `Agent loop` = 统一调度中枢

关键入口：

- CLI 入口：`src/main.rs`
- 核心导出：`src/lib.rs`
- 配置契约：`src/config/schema.rs`

---

## 2. 项目核心功能地图（先知道“它能做什么”）

### 2.1 运行形态

- 命令行交互：`zeroclaw agent`
- HTTP 网关：`zeroclaw gateway`
- 守护进程：`zeroclaw daemon`
- 服务化：`zeroclaw service ...`

### 2.2 功能面

- 多模型统一接入（OpenAI/Anthropic/Gemini/Ollama/...）
- 多渠道消息输入输出（Telegram/Discord/Slack/...）
- 工具调用闭环（模型决策 + 工具执行 + 结果回灌）
- 可切换记忆后端（sqlite/markdown/postgres/qdrant/none）
- 可观测与可靠性（事件、指标、重试、回退、循环检测）
- 安全策略（权限、自主等级、审批、路径约束、风险控制）

你看到的“功能很多”，本质来自同一个设计：**统一 trait + 模块化实现 + 配置驱动组装**。

---

## 3. 架构总览：四层模型（必须掌握）

### 3.1 四层分解

1. **接口层（Contracts）**
   - 代码：`src/*/traits.rs`
   - 作用：定义稳定能力边界。
2. **实现层（Implementations）**
   - 代码：`src/providers/*.rs`、`src/channels/*.rs`、`src/tools/*.rs` 等
   - 作用：每个具体平台/能力的实现细节。
3. **组装层（Factory + Config Wiring）**
   - 代码：`src/providers/mod.rs`、`src/memory/mod.rs`、`src/channels/mod.rs`、`src/tools/mod.rs`
   - 作用：按配置创建并组合实现。
4. **编排层（Orchestration）**
   - 代码：`src/agent/loop_.rs`、`src/daemon/`、`src/gateway/`
   - 作用：驱动运行时生命周期。

### 3.2 目录到职责速查

- `src/agent/`：对话编排、工具循环、提示词构造、调度
- `src/providers/`：模型协议适配、路由、可靠性封装
- `src/channels/`：平台消息协议适配
- `src/tools/`：工具定义与执行
- `src/memory/`：记忆读写、检索与后端抽象
- `src/security/`：策略、权限、审计、防护
- `src/runtime/`：执行环境能力声明（shell/fs/long-running）
- `src/observability/`：事件、指标、日志/otel/prometheus
- `src/gateway/`：webhook/SSE/WS/OpenAI 兼容入口
- `src/peripherals/`：硬件板卡能力接入

---

## 4. 关键设计原理（理解“为什么这样做”）

### 4.1 Trait-first：稳定边界优先

优势：

- 新能力通常不需要改主循环
- 可以替换实现而不破坏调用方
- 便于单测与模拟（mock）

典型路径：**新增能力 = 实现 trait + 在 `mod.rs` 工厂注册**。

### 4.2 配置即行为

`src/config/schema.rs` 是运行契约：

- 改配置，运行行为即变化
- CLI / daemon / gateway 共享同一套配置语义
- 配置字段要当“公开 API”维护

### 4.3 编排中枢最小化

`src/agent/loop_.rs` 目标是保持“流程稳定、逻辑明确”：

- 发送请求
- 解析工具调用
- 执行工具
- 回灌结果
- 检查停止条件

复杂性尽量下沉到 provider/tool/channel 的具体实现中。

### 4.4 默认安全保守

`src/security/policy.rs` 展示了安全默认值：

- 默认 `supervised`
- 高风险命令可阻断
- 路径访问可限制
- 可要求审批

这决定了 ZeroClaw 更偏“可控生产运行”，而不是“无约束黑盒代理”。

---

## 5. 运行原理（从一条输入到一条输出）

下面是一次完整 turn 的简化时序：

1. **输入接入**：来自 CLI 或 channel/gateway。
2. **上下文构建**：系统提示词 + 历史 + 记忆 + 可用工具 schema。
3. **Provider 请求**：发送统一消息结构给模型。
4. **模型响应判定**：
   - 直接文本 -> 结束
   - tool calls -> 进入执行
5. **工具执行**：串行/并行执行工具，汇总 `ToolResult`。
6. **结果回灌**：把 tool 输出作为后续上下文继续请求模型。
7. **循环控制**：迭代上限、循环检测、异常处理。
8. **输出下发与记录**：发送至 channel，写入 memory，记录 observability。

关键代码：`src/agent/loop_.rs`。

---

## 6. 七大核心 Trait 教程式解读

> 这一章是“改造前必读”。

### 6.1 `Provider`（`src/providers/traits.rs`）

你要关注 4 件事：

- 输入模型：`ChatMessage` / `ChatRequest`
- 输出模型：`ChatResponse`
- 工具调用：`ToolCall`
- 停止原因归一：`NormalizedStopReason`

理解要点：

- Provider 不是“只返回文本”，它可能驱动工具调用。
- 如果你新增 provider，必须保证工具调用语义与 stop reason 兼容主循环。

### 6.2 `Channel`（`src/channels/traits.rs`）

关键方法：

- `send`
- `listen`
- `health_check`
- `send_approval_prompt`（默认有降级实现）

理解要点：

- `listen` 是长运行输入流。
- 不同平台差异（thread、draft、typing）在 channel 内部吸收。

### 6.3 `Tool`（`src/tools/traits.rs`）

关键方法：

- `parameters_schema()`：告诉模型参数结构
- `execute(args)`：真正动作执行
- `spec()`：统一生成工具声明

理解要点：

- Tool 是“行动原子能力”。
- 参数 schema 质量直接影响模型调用正确率。

### 6.4 `Memory`（`src/memory/traits.rs`）

关键方法：

- `store` / `recall` / `get` / `list` / `forget`

理解要点：

- 记忆分类是行为语义，不只是存储字段。
- 后端替换应保持接口一致，避免上层感知差异。

### 6.5 `Observer`（`src/observability/traits.rs`）

关键对象：

- `ObserverEvent`
- `ObserverMetric`

理解要点：

- 可观测是运行时稳定性的“神经系统”。
- 上报时要避免泄露敏感内容。

### 6.6 `RuntimeAdapter`（`src/runtime/traits.rs`）

关键能力：

- `has_shell_access`
- `has_filesystem_access`
- `supports_long_running`
- `build_shell_command`

理解要点：

- RuntimeAdapter 决定“这台环境能做什么”。
- 对工具能力开关有直接影响。

### 6.7 `Peripheral`（`src/peripherals/traits.rs`）

关键方法：

- `connect` / `disconnect` / `health_check`
- `tools()`

理解要点：

- 硬件能力通过工具暴露给 agent。
- 外设属于可插拔行动层，不应污染主流程。

---

## 7. 从 CLI 命令理解系统（最实用）

### 7.1 新手最小命令集

```bash
zeroclaw onboard
zeroclaw status
zeroclaw doctor
zeroclaw agent
```

用途：

- `onboard`：初始化配置
- `status`：看当前运行配置摘要
- `doctor`：看系统健康
- `agent`：进入核心对话循环

### 7.2 运维常用命令集

```bash
zeroclaw channel list
zeroclaw channel doctor
zeroclaw gateway
zeroclaw daemon
zeroclaw service status
```

建议先在本地前台跑通，再转 service。

---

## 8. 配置驱动实战：哪些字段最关键

先阅读：`docs/config-reference.md` + `src/config/schema.rs`。

### 8.1 你必须先理解的配置域

- Provider：默认模型/地址/鉴权
- Agent：工具迭代上限、循环检测
- Security：autonomy、审批、风险控制
- Memory：后端类型和连接参数
- Channel：平台 token、allowlist、回复策略
- Runtime：执行环境能力

### 8.2 配置改造原则

- 一次只改一个逻辑点
- 先 `status` 再 `doctor`
- 对高风险配置（security/runtime/gateway）先准备回滚方案

---

## 9. 安全模型教程（必须单独学习）

重点文件：`src/security/policy.rs`。

### 9.1 自主等级心智模型

- `read_only`：观察优先
- `supervised`：默认，危险动作可要求审批
- `full`：策略边界内自动执行

### 9.2 安全边界常见项

- 命令允许列表与上下文规则
- 禁止路径与工作区约束
- 敏感读写限制
- 动作频率限制

### 9.3 新手常见误区

- 误以为“工具可用 = 一定能执行”
- 忽略运行时与安全策略的双重限制
- 在未评估风险前直接提高 autonomy

---

## 10. 改造实战 A：新增一个最小 Tool（推荐第一练）

> 目标：在不触碰高风险模块的情况下，完成一次完整扩展。

### 10.1 设计目标

实现一个只读工具，例如 `workspace_summary`：

- 输入：可选目录
- 输出：目录下文件数量、Rust 文件数量
- 风险：低（只读）

### 10.2 实现步骤

1. 在 `src/tools/` 增加工具文件，实现 `Tool` trait。
2. 定义清晰的 `parameters_schema()`。
3. 在工具注册流程中挂载（查看 `src/tools/mod.rs` 现有模式）。
4. 通过 `zeroclaw agent` 触发工具调用路径验证。

### 10.3 验证清单

- 参数缺失/类型错误时，是否返回显式错误？
- 输出是否结构化且可读？
- 不应写文件、不应越权读敏感路径。

---

## 11. 改造实战 B：新增 Provider（中阶）

> 目标：理解“协议适配 + 工厂注册 + 失败路径”的标准流程。

### 11.1 实现清单

1. 新建 `src/providers/<name>.rs`。
2. 实现 `Provider` trait。
3. 在 `src/providers/mod.rs` 的创建流程注册 key。
4. 对 stop reason/tool call 做兼容映射。

### 11.2 高风险点

- API 错误格式不统一导致解析失败
- 工具调用字段兼容问题
- 未处理超时、429、重试退避

建议先做最小功能，再做可靠性增强。

---

## 12. 改造实战 C：新增 Channel（中阶）

### 12.1 最小可运行要求

- `listen` 能稳定产出 `ChannelMessage`
- `send` 能输出回复
- `health_check` 能反映真实可用性

### 12.2 平台差异注意点

- 线程消息语义（thread）
- webhook 与轮询模式差异
- allowlist 与 mention 触发策略

参考：`docs/channels-reference.md`、`src/channels/traits.rs`

---

## 13. 可靠性与可观测：把“能跑”变成“稳定跑”

### 13.1 可靠性关键机制

- Provider 重试与回退（`src/providers/reliable.rs`）
- 工具循环迭代上限（`src/agent/loop_.rs`）
- 循环检测（无进展 / ping-pong / 失败连击）

### 13.2 可观测关键机制

- 事件：`ObserverEvent`
- 指标：`ObserverMetric`

实践建议：

- 新增功能时，至少保证错误可定位（组件、原因、上下文摘要）。
- 日志里不要带敏感 token 或原始秘密数据。

---

## 14. 推荐学习计划（升级版 14 天）

### 第 1~3 天：建立基础

- 跑通 `onboard/status/doctor/agent`
- 阅读 `README.md`、`docs/commands-reference.md`

### 第 4~6 天：读架构与契约

- 读 `src/*/traits.rs`
- 读 `src/agent/loop_.rs`
- 画出你自己的“消息流图”

### 第 7~9 天：做第一个扩展

- 新增一个只读 tool
- 完整验证异常路径

### 第 10~12 天：读安全与运维

- 读 `src/security/policy.rs`
- 读 `docs/operations-runbook.md`
- 用守护模式试跑

### 第 13~14 天：准备中阶改造

- 在 Provider 或 Channel 里选择一个扩展方向
- 写下设计、风险、回滚、验证计划

---

## 15. 改造前后检查单（可复制）

### 15.1 改造前

- 我修改的是哪一层？（trait / impl / factory / orchestration）
- 是否可通过“新增实现 + 注册”完成，而非重写核心？
- 是否涉及高风险目录（`security/`、`runtime/`、`gateway/`、`tools/`）？

### 15.2 改造后

- 命令行为是否与文档一致？
- 异常路径是否显式报错？
- 默认配置是否仍然安全？
- 是否容易回滚（单一责任、最小改动）？

---

## 16. 常见问题（FAQ）

### Q1：为什么我加了 Tool，模型却不调用？

先检查：

- `parameters_schema()` 是否可被模型理解
- 工具描述是否过于模糊
- 工具是否真的注册到了运行时（检查初始化路径）

### Q2：为什么 channel 配好了但没响应？

先跑：

- `zeroclaw channel doctor`
- 检查 allowlist 是否放行
- 检查 webhook/polling 模式与 token 是否匹配

### Q3：为什么工具循环提前停止？

可能原因：

- 达到 `max_tool_iterations`
- 触发 loop detection
- 某工具连续失败导致模型无法推进

排查入口：`src/agent/loop_.rs` + 相关日志。

---

## 17. 你下一步最该做什么

如果你现在是初学者，建议立刻做这三件事：

1. 用 `zeroclaw agent` 跑一个真实任务，观察是否触发工具调用。
2. 打开 `src/agent/loop_.rs`，按本教程第 5 章对照阅读一遍。
3. 做一次“只读 tool 扩展”练习（第 10 章）。

完成这三步后，你就从“会用 ZeroClaw”进入“能改 ZeroClaw”的阶段了。

---

## 18. 附录：高价值源码阅读清单

按优先级排序：

1. `src/agent/loop_.rs`
2. `src/providers/traits.rs`
3. `src/tools/traits.rs`
4. `src/channels/traits.rs`
5. `src/memory/traits.rs`
6. `src/runtime/traits.rs`
7. `src/security/policy.rs`
8. `src/providers/mod.rs`
9. `src/tools/mod.rs`
10. `src/config/schema.rs`

---

### 结语

ZeroClaw 的学习曲线看似陡峭，实则非常工程化：

- 先掌握 trait 契约
- 再理解工厂注册
- 最后进入编排和安全边界

只要坚持“最小变更 + 明确验证 + 可回滚”，你会很快具备稳定改造这个项目的能力。
