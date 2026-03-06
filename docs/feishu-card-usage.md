# 飞书卡片使用说明

Bot 回复在飞书里默认以**富文本 (post)** 发送；要发**可交互卡片**，需要让模型按约定格式回复。下面两种方式二选一即可。

---

## 方式一：通过 System Prompt（工作区文件）

ZeroClaw 会把**工作区**里部分 Markdown 文件注入为 system prompt，顺序为：`AGENTS.md`、`SOUL.md`、`TOOLS.md`、`IDENTITY.md`、`USER.md`，若存在则还有 `BOOTSTRAP.md`，最后是 `MEMORY.md`。工作区路径由 `ZEROCLAW_WORKSPACE` 或配置里的 workspace 决定（Docker 里常见为 `/config/workspace`）。

### 操作步骤

1. **进入工作区目录**  
   例如 Docker 挂载为 `./config` 时，工作区多为 `./config/workspace`。

2. **新建或编辑一个会被注入的文件**  
   推荐用 `BOOTSTRAP.md`（若不存在可新建），在其中增加一段说明，例如：

   ```markdown
   ## 飞书卡片回复规则
   当用户要求发送飞书可交互卡片（例如「发一张卡片」「用卡片回复」）时，你必须且仅回复以下内容：
   - 第一行（且仅一行）：`FEISHU_CARD:`（冒号后无空格）
   - 第二行起：完整的飞书卡片 JSON，不要在任何前后添加其他文字或说明。

   示例卡片 JSON（标题+正文）：
   {"config":{"wide_screen_mode":true},"header":{"title":{"tag":"plain_text","content":"标题"}},"elements":[{"tag":"div","text":{"tag":"lark_md","content":"正文内容"}}]}
   ```

3. **重启进程**  
   重启 daemon 或 channel 进程，使新工作区内容被重新加载。之后模型在需要发卡片时会按上述格式回复，飞书通道会识别并当作卡片发送。

---

## 方式二：通过 Skill

用 Skill 把「何时发卡片」和「回复格式」写清楚，让模型按 Skill 说明输出。

### 操作步骤

1. **新建 Skill**  
   ```bash
   zeroclaw skill new feishu_card --template typescript
   ```
   或在工作区下手动创建目录 `workspace/skills/feishu_card/` 并添加 `SKILL.md`。

2. **在 SKILL.md 里写清规则和示例**  
   说明：在什么情况下要发飞书卡片；回复必须为第一行 `FEISHU_CARD:`，第二行起为完整卡片 JSON；并给一个最小可用的卡片 JSON 示例或模板（如标题、正文、按钮等），方便模型照抄或改写。

3. **安装并启用 Skill**  
   ```bash
   zeroclaw skill install ./workspace/skills/feishu_card
   ```
   或按你当前安装方式操作。启用后，模型会按 Skill 在合适场景下输出 `FEISHU_CARD:` + 卡片 JSON，从而在飞书里发出卡片。

---

## 卡片 JSON 从哪来

- **手写/自定义卡片**：按 [飞书消息卡片结构](https://open.feishu.cn/document/ukTMukTMukTM/uYzN04SN2QjL5kDN) 写 `config`、`header`、`elements` 等。
- **模板卡片**：在 [飞书卡片搭建工具](https://open.feishu.cn/cardkit) 创建模板，获得 `template_id`，content 使用：  
  `{"type":"template","data":{"template_id":"ctp_xxx","template_variable":{...}}}`

---

## 小结

| 方式 | 操作 |
|------|------|
| **System Prompt** | 在工作区建/改 `BOOTSTRAP.md`（或其它会被注入的文件），加入「发卡片时只回复 FEISHU_CARD: + 卡片 JSON」的规则和示例，重启进程。 |
| **Skill** | 新建 Skill，在 `SKILL.md` 里写发卡片时机和回复格式（首行 `FEISHU_CARD:`，其余为卡片 JSON），并给示例；安装并启用该 Skill。 |

两种方式都是让模型的**最终回复**恰好是「第一行 `FEISHU_CARD:` + 换行 + 完整卡片 JSON」，飞书通道检测到该前缀后就会以 `msg_type: interactive` 发送。
