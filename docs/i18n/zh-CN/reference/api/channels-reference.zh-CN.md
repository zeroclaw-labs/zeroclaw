# 渠道参考文档

本文档是 JhedaiClaw 渠道配置的权威参考。

对于加密 Matrix 房间，还请阅读专用操作手册：

- [Matrix E2EE（端到端加密）指南](../../security/matrix-e2ee-guide.zh-CN.md)

## 快速路径

- 需要按渠道查看完整配置参考：跳转到 [按渠道配置示例](#4-按渠道配置示例)。
- 需要无响应诊断流程：跳转到 [故障排除清单](#6-故障排除清单)。
- 需要 Matrix 加密房间帮助：使用 [Matrix E2EE 指南](../../security/matrix-e2ee-guide.zh-CN.md)。
- 需要 Nextcloud Talk 机器人安装：使用 [Nextcloud Talk 安装指南](../../setup-guides/nextcloud-talk-setup.zh-CN.md)。
- 需要部署/网络假设（轮询 vs webhook）：使用 [网络部署](../../ops/network-deployment.zh-CN.md)。

## 常见问题：Matrix 安装通过但无回复

这是最常见的症状（与 issue #499 同类）。请按顺序检查：

1. **白名单不匹配**：`allowed_users` 不包含发送者（或为空）。
2. **错误的房间目标**：机器人未加入配置的 `room_id` / 别名目标房间。
3. **令牌/账户不匹配**：令牌有效但属于另一个 Matrix 账户。
4. **E2EE 设备身份缺口**：`whoami` 不返回 `device_id` 且配置未提供该值。
5. **密钥共享/信任缺口**：房间密钥未共享给机器人设备，因此加密事件无法解密。
6. **运行时状态陈旧**：配置已更改但 `jhedaiclaw daemon` 未重启。

---

## 1. 配置命名空间

所有渠道设置都位于 `~/.jhedaiclaw/config.toml` 的 `channels_config` 下。

```toml
[channels_config]
cli = true
```

每个渠道通过创建其子表来启用（例如 `[channels_config.telegram]`）。

## 聊天内运行时模型切换（Telegram / Discord）

运行 `jhedaiclaw channel start`（或守护进程模式）时，Telegram 和 Discord 现在支持发送者范围的运行时切换：

- `/models` — 显示可用提供商和当前选择
- `/models <provider>` — 为当前发送者会话切换提供商
- `/model` — 显示当前模型和缓存的模型 ID（如果可用）
- `/model <model-id>` — 为当前发送者会话切换模型
- `/new` — 清除对话历史并开始新会话

注意事项：

- 切换提供商或模型仅清除该发送者的内存中对话历史，以避免跨模型上下文污染。
- `/new` 清除发送者的对话历史，但不改变提供商或模型选择。
- 模型缓存预览来自 `jhedaiclaw models refresh --provider <ID>`。
- 这些是运行时聊天命令，不是 CLI 子命令。

## 入站图像标记协议

JhedaiClaw 通过内联消息标记支持多模态输入：

- 语法：`[IMAGE:<source>]`
- `<source>` 可以是：
  - 本地文件路径
  - 数据 URI（`data:image/...;base64,...`）
  - 仅当 `[multimodal].allow_remote_fetch = true` 时支持远程 URL

操作说明：

- 标记解析在提供商调用前应用于用户角色消息。
- 提供商能力在运行时强制执行：如果所选提供商不支持视觉，请求将失败并返回结构化能力错误（`capability=vision`）。
- Linq webhook 中 `image/*` MIME 类型的 `media` 部分会自动转换为此标记格式。

## 渠道矩阵

### 构建功能开关（`channel-matrix`、`channel-lark`）

Matrix 和 Lark 支持在编译时控制。

- 默认构建是精简的（`default = []`），不包含 Matrix/Lark。
- 仅包含硬件支持的典型本地检查：

```bash
cargo check --features hardware
```

- 需要时显式启用 Matrix：

```bash
cargo check --features hardware,channel-matrix
```

- 需要时显式启用 Lark：

```bash
cargo check --features hardware,channel-lark
```

如果存在 `[channels_config.matrix]`、`[channels_config.lark]` 或 `[channels_config.feishu]`，但对应的功能未编译进去，`jhedaiclaw channel list`、`jhedaiclaw channel doctor` 和 `jhedaiclaw channel start` 会报告该渠道在此构建中被故意跳过。

---

## 2. 交付模式概览

| 渠道           | 接收模式                                  | 需要公共入站端口？                          |
| -------------- | ----------------------------------------- | ------------------------------------------- |
| CLI            | 本地 stdin/stdout                         | 否                                          |
| Telegram       | 轮询                                      | 否                                          |
| Discord        | 网关/websocket                            | 否                                          |
| Slack          | 事件 API                                  | 否（基于令牌的渠道流）                      |
| Mattermost     | 轮询                                      | 否                                          |
| Matrix         | 同步 API（支持 E2EE）                     | 否                                          |
| Signal         | signal-cli HTTP 桥接                      | 否（本地桥接端点）                          |
| WhatsApp       | webhook（云 API）或 websocket（网页模式） | 云 API：是（公共 HTTPS 回调），网页模式：否 |
| Nextcloud Talk | webhook（`/nextcloud-talk`）              | 是（公共 HTTPS 回调）                       |
| Webhook        | 网关端点（`/webhook`）                    | 通常是                                      |
| Email          | IMAP 轮询 + SMTP 发送                     | 否                                          |
| IRC            | IRC 套接字                                | 否                                          |
| Lark           | websocket（默认）或 webhook               | 仅 webhook 模式需要                         |
| Feishu         | websocket（默认）或 webhook               | 仅 webhook 模式需要                         |
| DingTalk       | 流模式                                    | 否                                          |
| QQ             | 机器人网关                                | 否                                          |
| Linq           | webhook（`/linq`）                        | 是（公共 HTTPS 回调）                       |
| iMessage       | 本地集成                                  | 否                                          |
| Nostr          | 中继 websocket（NIP-04 / NIP-17）         | 否                                          |

---

## 3. 白名单语义

对于具有入站发送者白名单的渠道：

- 空白名单：拒绝所有入站消息。
- `"*"`：允许所有入站发送者（仅用于临时验证）。
- 显式列表：仅允许列出的发送者。

字段名称因渠道而异：

- `allowed_users`（Telegram/Discord/Slack/Mattermost/Matrix/IRC/Lark/Feishu/DingTalk/QQ/Nextcloud Talk）
- `allowed_from`（Signal）
- `allowed_numbers`（WhatsApp）
- `allowed_senders`（Email/Linq）
- `allowed_contacts`（iMessage）
- `allowed_pubkeys`（Nostr）

---

## 4. 按渠道配置示例

### 4.1 Telegram

```toml
[channels_config.telegram]
bot_token = \"123456:telegram-token\"
allowed_users = [\"*\"]
stream_mode = \"off\"               # 可选: off | partial
draft_update_interval_ms = 1000   # 可选: 部分流的编辑节流
mention_only = false              # 可选: 群组中需要@提及
interrupt_on_new_message = false  # 可选: 取消同一发送者同一聊天中进行中的请求
```

Telegram 注意事项：

- `interrupt_on_new_message = true` 会在对话历史中保留被中断的用户轮次，然后在最新消息上重新开始生成。
- 中断范围是严格的：同一聊天中的同一发送者。来自不同聊天的消息独立处理。

### 4.2 Discord

```toml
[channels_config.discord]
bot_token = \"discord-bot-token\"
guild_id = \"123456789012345678\"   # 可选
allowed_users = [\"*\"]
listen_to_bots = false
mention_only = false
```

### 4.3 Slack

```toml
[channels_config.slack]
bot_token = \"xoxb-...\"
app_token = \"xapp-...\"             # 可选
channel_id = \"C1234567890\"         # 可选: 单频道; 省略或 \"*\" 表示所有可访问频道
allowed_users = [\"*\"]
```

Slack 监听行为：

- `channel_id = \"C123...\"`：仅监听该频道。
- `channel_id = \"*\"` 或省略：自动发现并监听所有可访问频道。

### 4.4 Mattermost

```toml
[channels_config.mattermost]
url = \"https://mm.example.com\"
bot_token = \"mattermost-token\"
channel_id = \"channel-id\"          # 监听所需
allowed_users = [\"*\"]
```

### 4.5 Matrix

```toml
[channels_config.matrix]
homeserver = \"https://matrix.example.com\"
access_token = \"syt_...\"
user_id = \"@jhedaiclaw:matrix.example.com\"   # 可选，推荐用于 E2EE
device_id = \"DEVICEID123\"                  # 可选，推荐用于 E2EE
room_id = \"!room:matrix.example.com\"       # 或房间别名（#ops:matrix.example.com）
allowed_users = [\"*\"]
```

加密房间故障排除请参见 [Matrix E2EE 指南](../../security/matrix-e2ee-guide.zh-CN.md)。

### 4.6 Signal

```toml
[channels_config.signal]
http_url = \"http://127.0.0.1:8686\"
account = \"+1234567890\"
group_id = \"dm\"                    # 可选: \"dm\" / 群组 ID / 省略
allowed_from = [\"*\"]
ignore_attachments = false
ignore_stories = true
```

### 4.7 WhatsApp

JhedaiClaw 支持两个 WhatsApp 后端：

- **云 API 模式**（`phone_number_id` + `access_token` + `verify_token`）
- **WhatsApp 网页模式**（`session_path`，需要构建标志 `--features whatsapp-web`）

云 API 模式：

```toml
[channels_config.whatsapp]
access_token = \"EAAB...\"
phone_number_id = \"123456789012345\"
verify_token = \"your-verify-token\"
app_secret = \"your-app-secret\"     # 可选但推荐
allowed_numbers = [\"*\"]
```

WhatsApp 网页模式：

```toml
[channels_config.whatsapp]
session_path = \"~/.jhedaiclaw/state/whatsapp-web/session.db\"
pair_phone = \"15551234567\"         # 可选; 省略使用二维码流程
pair_code = \"\"                     # 可选自定义配对码
allowed_numbers = [\"*\"]
```

注意事项：

- 使用 `cargo build --features whatsapp-web` 构建（或等效的运行命令）。
- 将 `session_path` 保留在持久存储上，以避免重启后重新链接。
- 回复路由使用发起聊天的 JID，因此直接和群组回复都能正常工作。

### 4.8 Webhook 渠道配置（网关）

`channels_config.webhook` 启用特定于 webhook 的网关行为。

```toml
[channels_config.webhook]
port = 8080
secret = \"optional-shared-secret\"
```

使用网关/守护进程运行并验证 `/health`。

### 4.9 Email

```toml
[channels_config.email]
imap_host = \"imap.example.com\"
imap_port = 993
imap_folder = \"INBOX\"
smtp_host = \"smtp.example.com\"
smtp_port = 465
smtp_tls = true
username = \"bot@example.com\"
password = \"email-password\"
from_address = \"bot@example.com\"
poll_interval_secs = 60
allowed_senders = [\"*\"]
```

### 4.10 IRC

```toml
[channels_config.irc]
server = \"irc.libera.chat\"
port = 6697
nickname = \"jhedaiclaw-bot\"
username = \"jhedaiclaw\"              # 可选
channels = [\"#jhedaiclaw\"]
allowed_users = [\"*\"]
server_password = \"\"                # 可选
nickserv_password = \"\"              # 可选
sasl_password = \"\"                  # 可选
verify_tls = true
```

### 4.11 Lark

```toml
[channels_config.lark]
app_id = \"cli_xxx\"
app_secret = \"xxx\"
encrypt_key = \"\"                    # 可选
verification_token = \"\"             # 可选
allowed_users = [\"*\"]
mention_only = false              # 可选: 群组中需要@提及（私信始终允许）
use_feishu = false
receive_mode = \"websocket\"          # 或 \"webhook\"
port = 8081                          # webhook 模式所需
```

### 4.12 Feishu

```toml
[channels_config.feishu]
app_id = \"cli_xxx\"
app_secret = \"xxx\"
encrypt_key = \"\"                    # 可选
verification_token = \"\"             # 可选
allowed_users = [\"*\"]
receive_mode = \"websocket\"          # 或 \"webhook\"
port = 8081                          # webhook 模式所需
```

迁移说明：

- 旧配置 `[channels_config.lark] use_feishu = true` 仍向后兼容。
- 新安装推荐使用 `[channels_config.feishu]`。

### 4.13 Nostr

```toml
[channels_config.nostr]
private_key = \"nsec1...\"                   # 十六进制或 nsec bech32（静态加密）
# 中继默认使用 relay.damus.io, nos.lol, relay.primal.net, relay.snort.social
# relays = [\"wss://relay.damus.io\", \"wss://nos.lol\"]
allowed_pubkeys = [\"hex-or-npub\"]          # 空 = 拒绝所有, \"*\" = 允许所有
```

Nostr 同时支持 NIP-04（传统加密私信）和 NIP-17（礼物包装私有消息）。
回复自动使用发送者使用的相同协议。当 `secrets.encrypt = true`（默认）时，私钥通过 `SecretStore` 静态加密。

引导式设置支持：

```bash
jhedaiclaw onboard
```

向导现在包含专用的 **Lark** 和 **Feishu** 步骤，包括：

- 针对官方开放平台认证端点的凭证验证
- 接收模式选择（`websocket` 或 `webhook`）
- 可选的 webhook 验证令牌提示（推荐用于更强的回调真实性检查）

运行时令牌行为：

- `tenant_access_token` 会根据认证响应中的 `expire`/`expires_in` 缓存并设置刷新截止时间。
- 当 Feishu/Lark 返回 HTTP `401` 或业务错误代码 `99991663`（`Invalid access token`）时，发送请求会在令牌失效后自动重试一次。
- 如果重试仍然返回令牌无效响应，发送调用会失败并返回上游状态/响应体，以便于故障排除。

### 4.14 DingTalk

```toml
[channels_config.dingtalk]
client_id = \"ding-app-key\"
client_secret = \"ding-app-secret\"
allowed_users = [\"*\"]
```

### 4.15 QQ

```toml
[channels_config.qq]
app_id = \"qq-app-id\"
app_secret = \"qq-app-secret\"
allowed_users = [\"*\"]
```

### 4.16 Nextcloud Talk

```toml
[channels_config.nextcloud_talk]
base_url = \"https://cloud.example.com\"
app_token = \"nextcloud-talk-app-token\"
webhook_secret = \"optional-webhook-secret\"  # 可选但推荐
allowed_users = [\"*\"]
```

注意事项：

- 入站 webhook 端点：`POST /nextcloud-talk`。
- 签名验证使用 `X-Nextcloud-Talk-Random` 和 `X-Nextcloud-Talk-Signature`。
- 如果设置了 `webhook_secret`，无效签名会被拒绝并返回 `401`。
- `JHEDAICLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET` 会覆盖配置中的密钥。
- 完整操作手册请参见 [nextcloud-talk-setup.md](../../setup-guides/nextcloud-talk-setup.zh-CN.md)。

### 4.16 Linq

```toml
[channels_config.linq]
api_token = \"linq-partner-api-token\"
from_phone = \"+15551234567\"
signing_secret = \"optional-webhook-signing-secret\"  # 可选但推荐
allowed_senders = [\"*\"]
```

注意事项：

- Linq 使用合作伙伴 V3 API 支持 iMessage、RCS 和 SMS。
- 入站 webhook 端点：`POST /linq`。
- 签名验证使用 `X-Webhook-Signature`（HMAC-SHA256）和 `X-Webhook-Timestamp`。
- 如果设置了 `signing_secret`，无效或过期（>300秒）的签名会被拒绝。
- `JHEDAICLAW_LINQ_SIGNING_SECRET` 会覆盖配置中的密钥。
- `allowed_senders` 使用 E.164 电话号码格式（例如 `+1234567890`）。

### 4.17 iMessage

```toml
[channels_config.imessage]
allowed_contacts = [\"*\"]
```

---

## 5. 验证工作流

1. 为初始验证配置一个带有宽松白名单（`"*"`）的渠道。
2. 运行：

```bash
jhedaiclaw onboard --channels-only
jhedaiclaw daemon
```

3. 从预期的发送者发送消息。
4. 确认收到回复。
5. 将白名单从 `"*"` 收紧为显式 ID。

---

## 6. 故障排除清单

如果渠道显示已连接但不响应：

1. 确认发送者身份被正确的白名单字段允许。
2. 确认机器人账户在目标房间/频道中的成员资格/权限。
3. 确认令牌/密钥有效（且未过期/被撤销）。
4. 确认传输模式假设：
   - 轮询/websocket 渠道不需要公共入站 HTTP
   - webhook 渠道需要可访问的 HTTPS 回调
5. 配置更改后重启 `jhedaiclaw daemon`。

专门针对 Matrix 加密房间，请使用：

- [Matrix E2EE 指南](../../security/matrix-e2ee-guide.zh-CN.md)

---

## 7. 操作附录：日志关键词矩阵

使用本附录进行快速分类。首先匹配日志关键词，然后按照上述故障排除步骤操作。

### 7.1 推荐捕获命令

```bash
RUST_LOG=info jhedaiclaw daemon 2>&1 | tee /tmp/jhedaiclaw.log
```

然后过滤渠道/网关事件：

```bash
rg -n \"Matrix|Telegram|Discord|Slack|Mattermost|Signal|WhatsApp|Email|IRC|Lark|DingTalk|QQ|iMessage|Nostr|Webhook|Channel\" /tmp/jhedaiclaw.log
```

### 7.2 关键词表

| 组件                       | 启动 / 健康信号                                                                                                 | 认证 / 策略信号                                                                                                                                                                       | 传输 / 失败信号                                                                                                           |
| -------------------------- | --------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| Telegram                   | `Telegram channel listening for messages...`                                                                    | `Telegram: ignoring message from unauthorized user:`                                                                                                                                  | `Telegram poll error:` / `Telegram parse error:` / `Telegram polling conflict (409):`                                     |
| Discord                    | `Discord: connected and identified`                                                                             | `Discord: ignoring message from unauthorized user:`                                                                                                                                   | `Discord: received Reconnect (op 7)` / `Discord: received Invalid Session (op 9)`                                         |
| Slack                      | `Slack channel listening on #` / `Slack channel_id not set (or '*'); listening across all accessible channels.` | `Slack: ignoring message from unauthorized user:`                                                                                                                                     | `Slack poll error:` / `Slack parse error:` / `Slack channel discovery failed:`                                            |
| Mattermost                 | `Mattermost channel listening on`                                                                               | `Mattermost: ignoring message from unauthorized user:`                                                                                                                                | `Mattermost poll error:` / `Mattermost parse error:`                                                                      |
| Matrix                     | `Matrix channel listening on room` / `Matrix room ... is encrypted; E2EE decryption is enabled via matrix-sdk.` | `Matrix whoami failed; falling back to configured session hints for E2EE session restore:` / `Matrix whoami failed while resolving listener user_id; using configured user_id hint:`  | `Matrix sync error: ... retrying...`                                                                                      |
| Signal                     | `Signal channel listening via SSE on`                                                                           | （白名单检查由 `allowed_from` 强制执行）                                                                                                                                              | `Signal SSE returned ...` / `Signal SSE connect error:`                                                                   |
| WhatsApp（渠道）           | `WhatsApp channel active (webhook mode).` / `WhatsApp Web connected successfully`                               | `WhatsApp: ignoring message from unauthorized number:` / `WhatsApp Web: message from ... not in allowed list`                                                                         | `WhatsApp send failed:` / `WhatsApp Web stream error:`                                                                    |
| Webhook / WhatsApp（网关） | `WhatsApp webhook verified successfully`                                                                        | `Webhook: rejected — not paired / invalid bearer token` / `Webhook: rejected request — invalid or missing X-Webhook-Secret` / `WhatsApp webhook verification failed — token mismatch` | `Webhook JSON parse error:`                                                                                               |
| Email                      | `Email polling every ...` / `Email sent to ...`                                                                 | `Blocked email from ...`                                                                                                                                                              | `Email poll failed:` / `Email poll task panicked:`                                                                        |
| IRC                        | `IRC channel connecting to ...` / `IRC registered as ...`                                                       | （白名单检查由 `allowed_users` 强制执行）                                                                                                                                             | `IRC SASL authentication failed (...)` / `IRC server does not support SASL...` / `IRC nickname ... is in use, trying ...` |
| Lark / Feishu              | `Lark: WS connected` / `Lark event callback server listening on`                                                | `Lark WS: ignoring ... (not in allowed_users)` / `Lark: ignoring message from unauthorized user:`                                                                                     | `Lark: ping failed, reconnecting` / `Lark: heartbeat timeout, reconnecting` / `Lark: WS read error:`                      |
| DingTalk                   | `DingTalk: connected and listening for messages...`                                                             | `DingTalk: ignoring message from unauthorized user:`                                                                                                                                  | `DingTalk WebSocket error:` / `DingTalk: message channel closed`                                                          |
| QQ                         | `QQ: connected and identified`                                                                                  | `QQ: ignoring C2C message from unauthorized user:` / `QQ: ignoring group message from unauthorized user:`                                                                             | `QQ: received Reconnect (op 7)` / `QQ: received Invalid Session (op 9)` / `QQ: message channel closed`                    |
| Nextcloud Talk（网关）     | `POST /nextcloud-talk — Nextcloud Talk bot webhook`                                                             | `Nextcloud Talk webhook signature verification failed` / `Nextcloud Talk: ignoring message from unauthorized actor:`                                                                  | `Nextcloud Talk send failed:` / `LLM error for Nextcloud Talk message:`                                                   |
| iMessage                   | `iMessage channel listening (AppleScript bridge)...`                                                            | （联系人白名单由 `allowed_contacts` 强制执行）                                                                                                                                        | `iMessage poll error:`                                                                                                    |
| Nostr                      | `Nostr channel listening as npub1...`                                                                           | `Nostr: ignoring NIP-04 message from unauthorized pubkey:` / `Nostr: ignoring NIP-17 message from unauthorized pubkey:`                                                               | `Failed to decrypt NIP-04 message:` / `Failed to unwrap NIP-17 gift wrap:` / `Nostr relay pool shut down`                 |

### 7.3 运行时监管关键词

如果特定渠道任务崩溃或退出，`channels/mod.rs` 中的渠道监管器会输出：

- `Channel <name> exited unexpectedly; restarting`
- `Channel <name> error: ...; restarting`
- `Channel message worker crashed:`

这些消息表示自动重启行为已激活，你应该检查前面的日志以查找根本原因。
