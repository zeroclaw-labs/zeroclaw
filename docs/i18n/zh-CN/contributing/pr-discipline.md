# PR 规范

ZeroClaw 拉取请求的质量、署名、隐私和交接规则。

## 隐私/敏感数据（必填）

将隐私和中立性视为合并门控，而非尽力而为的指南。

- 永远不要在代码、文档、测试、夹具、快照、日志、示例或提交消息中提交个人或敏感数据。
- 禁止的数据包括（非详尽）：真实姓名、个人邮箱、电话号码、地址、访问令牌、API 密钥、凭证、ID 和私有 URL。
- 使用中立的项目范围占位符（例如 `user_a`、`test_user`、`project_bot`、`example.com`）代替真实身份数据。
- 测试名称/消息/夹具必须是非个人的、以系统为中心的；避免第一人称或特定身份的语言。
- 如果不可避免需要类似身份的上下文，仅使用 ZeroClaw 范围的角色/标签（例如 `ZeroClawAgent`、`ZeroClawOperator`、`zeroclaw_user`）。
- 推荐的身份安全命名调色板：
    - 参与者标签：`ZeroClawAgent`、`ZeroClawOperator`、`ZeroClawMaintainer`、`zeroclaw_user`
    - 服务/运行时标签：`zeroclaw_bot`、`zeroclaw_service`、`zeroclaw_runtime`、`zeroclaw_node`
    - 环境标签：`zeroclaw_project`、`zeroclaw_workspace`、`zeroclaw_channel`
- 如果复现外部事件，提交前脱敏和匿名化所有有效负载。
- 推送前，专门审查 `git diff --cached` 查找意外的敏感字符串和身份泄露。

## 被取代 PR 的署名（必填）

当一个 PR 取代另一个贡献者的 PR 并继承了实质性代码或设计决策时，显式保留作者署名。

- 在合并提交消息中，为每个其工作被实质性包含的被取代贡献者添加一个 `Co-authored-by: 姓名 <邮箱>` 尾部。
- 使用 GitHub 认可的邮箱（`<login@users.noreply.github.com>` 或贡献者已验证的提交邮箱）。
- 将尾部放在提交消息末尾的空行之后，单独占行；永远不要将它们编码为转义的 `\\n` 文本。
- 在 PR 正文中，列出被取代的 PR 链接，并简要说明从每个 PR 中合并了什么。
- 如果没有实际合并代码/设计（仅灵感），不要使用 `Co-authored-by`；在 PR 说明中给予感谢即可。

## 被取代 PR 模板

### PR 标题/正文模板

- 推荐标题格式：`feat(<范围>): 统一并取代 #<pr_a>、#<pr_b> [和 #<pr_n>]`
- 在 PR 正文中包含：

```md
## 取代
- #<pr_a> 作者 @<author_a>
- #<pr_b> 作者 @<author_b>

## 合并范围
- 来自 #<pr_a>：<实质性合并的内容>
- 来自 #<pr_b>：<实质性合并的内容>

## 署名
- 为实质性合并的贡献者添加了 Co-authored-by 尾部：是/否
- 如果否，说明原因

## 非目标
- <显式列出未继承的内容>

## 风险和回滚
- 风险：<摘要>
- 回滚：<恢复提交/PR 策略>
```

### 提交消息模板

```text
feat(<范围>): 统一并取代 #<pr_a>、#<pr_b> [和 #<pr_n>]

<一段关于合并结果的摘要>

取代：
- #<pr_a> 作者 @<author_a>
- #<pr_b> 作者 @<author_b>

合并范围：
- <子系统或功能_a>：来自 #<pr_x>
- <子系统或功能_b>：来自 #<pr_y>

Co-authored-by: <姓名 A> <login_a@users.noreply.github.com>
Co-authored-by: <姓名 B> <login_b@users.noreply.github.com>
```

## 交接模板（代理 -> 代理 / 维护者）

交接工作时，包含：

1. 变更了什么
2. 没有变更什么
3. 已运行的验证和结果
4. 剩余风险/未知项
5. 推荐的下一步操作
