# 頻道參照手冊（繁體中文）

本文件是 ZeroClaw 頻道設定的正式參照手冊。

如需加密 Matrix 房間的操作指南，請另行參閱：
- [Matrix E2EE 指南](./matrix-e2ee-guide.md)

## 快速導覽

- 需要依頻道查看完整設定參考：跳至[各頻道設定範例](#4-各頻道設定範例)。
- 需要診斷無回應問題：跳至[故障排除清單](#6-故障排除清單)。
- 需要 Matrix 加密房間協助：參閱 [Matrix E2EE 指南](./matrix-e2ee-guide.md)。
- 需要 Nextcloud Talk 機器人設定：參閱 [Nextcloud Talk 設定](./nextcloud-talk-setup.md)。
- 需要部署/網路架構說明（輪詢 vs webhook）：參閱[網路部署](./network-deployment.md)。

## 常見問答：Matrix 設定通過但無回覆

這是最常見的症狀（與 issue #499 屬同一類問題）。請依序檢查以下項目：

1. **允許清單不符**：`allowed_users` 未包含發送者（或為空）。
2. **房間目標錯誤**：機器人未加入設定的 `room_id` / 房間別名所指向的房間。
3. **權杖/帳號不符**：權杖有效但屬於另一個 Matrix 帳號。
4. **E2EE 裝置身分缺漏**：`whoami` 未回傳 `device_id` 且設定中也未提供。
5. **金鑰共享/信任缺漏**：房間金鑰未分享給機器人裝置，導致無法解密加密事件。
6. **執行狀態過時**：設定已修改但 `zeroclaw daemon` 未重啟。

---

## 1. 設定命名空間

所有頻道設定位於 `~/.zeroclaw/config.toml` 的 `channels_config` 下。

```toml
[channels_config]
cli = true
```

每個頻道透過建立其子表來啟用（例如 `[channels_config.telegram]`）。

單一 ZeroClaw 執行環境可同時服務多個頻道：若設定了多個頻道子表，`zeroclaw channel start` 會在同一個行程中啟動所有頻道。頻道啟動採盡力模式：單一頻道初始化失敗會被回報並跳過，其餘頻道繼續運作。

## 聊天室內執行指令

當執行 `zeroclaw channel start`（或 daemon 模式）時，可用的執行指令包括：

Telegram/Discord 發送者範圍的模型路由：
- `/models` — 顯示可用 provider 與目前選擇
- `/models <provider>` — 為當前發送者工作階段切換 provider
- `/model` — 顯示當前模型與已快取的模型 ID（若有）
- `/model <model-id>` — 為當前發送者工作階段切換模型
- `/new` — 清除對話歷史並開始新的工作階段

受監督的工具核准（所有非 CLI 頻道）：
- `/approve-request <tool-name>` — 建立待核准請求
- `/approve-confirm <request-id>` — 確認待核准請求（僅限同一發送者 + 同一聊天室/頻道）
- `/approve-pending` — 列出當前發送者+聊天室/頻道範圍的待核准請求
- `/approve <tool-name>` — 直接一步核准並持久化（`autonomy.auto_approve`，相容路徑）
- `/unapprove <tool-name>` — 撤銷並移除已持久化的核准
- `/approvals` — 檢視執行階段授權、已持久化的核准清單，以及排除的工具

備註：

- 切換 provider 或模型只會清除該發送者的記憶體內對話歷史，以避免跨模型上下文污染。
- `/new` 清除發送者的對話歷史，不會變更 provider 或模型選擇。
- 模型快取預覽來自 `zeroclaw models refresh --provider <ID>`。
- 這些是執行階段的聊天指令，而非 CLI 子命令。
- 支援自然語言核准意圖，透過嚴格解析與政策控制：
  - `direct` 模式（預設）：`授权工具 shell` 立即授權。
  - `request_confirm` 模式：`授权工具 shell` 建立待核准請求，之後以請求 ID 確認。
  - `disabled` 模式：核准管理必須使用斜線指令。
- 可透過 `[autonomy].non_cli_natural_language_approval_mode_by_channel` 依頻道覆寫自然語言核准模式。
- 核准指令在 LLM 執行前即被攔截，因此模型無法透過工具呼叫自行升級權限。
- 可透過 `[autonomy].non_cli_approval_approvers` 限制誰能使用核准管理指令。
- 透過 `[autonomy].non_cli_natural_language_approval_mode` 設定自然語言核准模式。
- `autonomy.non_cli_excluded_tools` 會在執行階段從 `config.toml` 重新載入；`/approvals` 顯示目前生效的清單。
- 每則收到的訊息會將執行階段工具可用性快照注入 system prompt，其來源與執行時使用的排除政策相同。

## 入站圖片標記協定

ZeroClaw 透過行內訊息標記支援多模態輸入：

- 語法：``[IMAGE:<source>]``
- `<source>` 可為：
  - 本機檔案路徑
  - Data URI（`data:image/...;base64,...`）
  - 遠端 URL 僅在 `[multimodal].allow_remote_fetch = true` 時可用

操作備註：

- 標記解析在呼叫 provider 前套用於 user 角色的訊息。
- Provider 能力在執行階段強制檢查：若選取的 provider 不支援視覺功能，請求會以結構化能力錯誤失敗（`capability=vision`）。
- Linq webhook 中帶有 `image/*` MIME 類型的 `media` 部分會自動轉換為此標記格式。

## 頻道矩陣

### 編譯功能開關（`channel-matrix`、`channel-lark`）

Matrix 與 Lark 支援在編譯時控制。

- 預設建置包含 Lark/飛書（`default = ["channel-lark"]`），而 Matrix 需手動啟用。
- 如需精簡的本機建置（不含 Matrix/Lark）：

```bash
cargo check --no-default-features --features hardware
```

- 在自訂功能集中明確啟用 Matrix：

```bash
cargo check --no-default-features --features hardware,channel-matrix
```

- 在自訂功能集中明確啟用 Lark：

```bash
cargo check --no-default-features --features hardware,channel-lark
```

若 `[channels_config.matrix]`、`[channels_config.lark]` 或 `[channels_config.feishu]` 存在但對應功能未編譯，`zeroclaw channel list`、`zeroclaw channel doctor` 與 `zeroclaw channel start` 會回報該頻道在此建置中被刻意跳過。

---

## 2. 傳送模式一覽

| 頻道 | 接收模式 | 是否需要公開入站埠？ |
|---|---|---|
| CLI | 本機 stdin/stdout | 否 |
| Telegram | 輪詢 | 否 |
| Discord | gateway/websocket | 否 |
| Slack | events API | 否（以權杖為基礎的頻道流程） |
| Mattermost | 輪詢 | 否 |
| Matrix | sync API（支援 E2EE） | 否 |
| Signal | signal-cli HTTP 橋接 | 否（本機橋接端點） |
| WhatsApp | webhook（Cloud API）或 websocket（Web 模式） | Cloud API：是（公開 HTTPS 回呼），Web 模式：否 |
| Nextcloud Talk | webhook（`/nextcloud-talk`） | 是（公開 HTTPS 回呼） |
| Webhook | gateway 端點（`/webhook`） | 通常是 |
| Email | IMAP 輪詢 + SMTP 傳送 | 否 |
| IRC | IRC socket | 否 |
| Lark | websocket（預設）或 webhook | 僅 webhook 模式 |
| Feishu | websocket（預設）或 webhook | 僅 webhook 模式 |
| DingTalk | stream 模式 | 否 |
| QQ | bot gateway | 否 |
| Linq | webhook（`/linq`） | 是（公開 HTTPS 回呼） |
| iMessage | 本機整合 | 否 |
| Nostr | relay websocket（NIP-04 / NIP-17） | 否 |

---

## 3. 允許清單語意

對於有入站發送者允許清單的頻道：

- 空允許清單：拒絕所有入站訊息。
- `"*"`：允許所有入站發送者（僅用於臨時驗證）。
- 明確清單：僅允許已列出的發送者。

欄位名稱因頻道而異：

- `allowed_users`（Telegram/Discord/Slack/Mattermost/Matrix/IRC/Lark/Feishu/DingTalk/QQ/Nextcloud Talk）
- `allowed_from`（Signal）
- `allowed_numbers`（WhatsApp）
- `allowed_senders`（Email/Linq）
- `allowed_contacts`（iMessage）
- `allowed_pubkeys`（Nostr）

### 群組聊天觸發政策（Telegram/Discord/Slack/Mattermost/Lark/Feishu）

這些頻道支援明確的 `group_reply` 政策：

- `mode = "all_messages"`：回覆所有群組訊息（須通過頻道允許清單檢查）。
- `mode = "mention_only"`：在群組中，需要明確提及機器人。
- `allowed_sender_ids`：可繞過群組中 mention 閘門的發送者 ID。

重要行為：

- `allowed_sender_ids` 僅繞過 mention 閘門。
- 發送者允許清單（`allowed_users`）仍會優先執行。

範例結構：

```toml
[channels_config.telegram.group_reply]
mode = "mention_only"                      # all_messages | mention_only
allowed_sender_ids = ["123456789", "987"] # 選填；可用 "*"
```

---

## 4. 各頻道設定範例

### 4.1 Telegram

```toml
[channels_config.telegram]
bot_token = "123456:telegram-token"
allowed_users = ["*"]
stream_mode = "off"               # 選填：off | partial
draft_update_interval_ms = 1000   # 選填：partial streaming 的編輯節流間隔
mention_only = false              # 舊版備援；當 group_reply.mode 未設定時使用
interrupt_on_new_message = false  # 選填：取消同一發送者同一聊天室中進行中的請求
ack_enabled = true                # 選填：傳送 emoji 反應作為確認（預設：true）

[channels_config.telegram.group_reply]
mode = "all_messages"             # 選填：all_messages | mention_only
allowed_sender_ids = []           # 選填：可繞過 mention 閘門的發送者 ID
```

Telegram 備註：

- `interrupt_on_new_message = true` 會將被中斷的使用者輪次保留在對話歷史中，然後以最新訊息重新開始生成。
- 中斷範圍嚴格限定：同一發送者在同一聊天室。來自不同聊天室的訊息會獨立處理。
- `ack_enabled = false` 停用傳送至收到訊息的 emoji 反應確認。

### 4.2 Discord

```toml
[channels_config.discord]
bot_token = "discord-bot-token"
guild_id = "123456789012345678"   # 選填
allowed_users = ["*"]
listen_to_bots = false
mention_only = false              # 舊版備援；當 group_reply.mode 未設定時使用

[channels_config.discord.group_reply]
mode = "all_messages"             # 選填：all_messages | mention_only
allowed_sender_ids = []           # 選填：可繞過 mention 閘門的發送者 ID
```

### 4.3 Slack

```toml
[channels_config.slack]
bot_token = "xoxb-..."
app_token = "xapp-..."             # 選填
channel_id = "C1234567890"         # 選填：單一頻道；省略或填 "*" 監聽所有可存取頻道
allowed_users = ["*"]

[channels_config.slack.group_reply]
mode = "all_messages"              # 選填：all_messages | mention_only
allowed_sender_ids = []            # 選填：可繞過 mention 閘門的發送者 ID
```

Slack 監聽行為：

- `channel_id = "C123..."`：僅監聽該頻道。
- `channel_id = "*"` 或省略：自動探索並監聽所有可存取的頻道。

### 4.4 Mattermost

```toml
[channels_config.mattermost]
url = "https://mm.example.com"
bot_token = "mattermost-token"
channel_id = "channel-id"          # 監聽所需
allowed_users = ["*"]
mention_only = false               # 舊版備援；當 group_reply.mode 未設定時使用

[channels_config.mattermost.group_reply]
mode = "all_messages"              # 選填：all_messages | mention_only
allowed_sender_ids = []            # 選填：可繞過 mention 閘門的發送者 ID
```

### 4.5 Matrix

```toml
[channels_config.matrix]
homeserver = "https://matrix.example.com"
access_token = "syt_..."
user_id = "@zeroclaw:matrix.example.com"   # 選填，E2EE 建議設定
device_id = "DEVICEID123"                  # 選填，E2EE 建議設定
room_id = "!room:matrix.example.com"       # 或房間別名（#ops:matrix.example.com）
allowed_users = ["*"]
mention_only = false                       # 選填：啟用時僅回應私訊 / @提及 / 回覆機器人
```

加密房間故障排除請參閱 [Matrix E2EE 指南](./matrix-e2ee-guide.md)。

### 4.6 Signal

```toml
[channels_config.signal]
http_url = "http://127.0.0.1:8686"
account = "+1234567890"
group_id = "dm"                    # 選填："dm" / 群組 id / 省略
allowed_from = ["*"]
ignore_attachments = false
ignore_stories = true
```

### 4.7 WhatsApp

ZeroClaw 支援兩種 WhatsApp 後端：

- **Cloud API 模式**（`phone_number_id` + `access_token` + `verify_token`）
- **WhatsApp Web 模式**（`session_path`，需要編譯旗標 `--features whatsapp-web`）

Cloud API 模式：

```toml
[channels_config.whatsapp]
access_token = "EAAB..."
phone_number_id = "123456789012345"
verify_token = "your-verify-token"
app_secret = "your-app-secret"     # 選填但建議設定
allowed_numbers = ["*"]
```

WhatsApp Web 模式：

```toml
[channels_config.whatsapp]
session_path = "~/.zeroclaw/state/whatsapp-web/session.db"
pair_phone = "15551234567"         # 選填；省略則使用 QR 碼流程
pair_code = ""                     # 選填自訂配對碼
allowed_numbers = ["*"]
```

備註：

- 以 `cargo build --features whatsapp-web`（或同等的 run 指令）建置。
- 將 `session_path` 放在持久化儲存空間，避免重啟後需重新連結。
- 回覆路由使用原始聊天 JID，因此直接回覆與群組回覆均可正確運作。

### 4.8 Webhook 頻道設定（Gateway）

`channels_config.webhook` 啟用 webhook 專屬的 gateway 行為。

```toml
[channels_config.webhook]
port = 8080
secret = "optional-shared-secret"
```

以 gateway/daemon 模式執行並驗證 `/health`。

### 4.9 Email

```toml
[channels_config.email]
imap_host = "imap.example.com"
imap_port = 993
imap_folder = "INBOX"
smtp_host = "smtp.example.com"
smtp_port = 465
smtp_tls = true
username = "bot@example.com"
password = "email-password"
from_address = "bot@example.com"
poll_interval_secs = 60
allowed_senders = ["*"]
```

### 4.10 IRC

```toml
[channels_config.irc]
server = "irc.libera.chat"
port = 6697
nickname = "zeroclaw-bot"
username = "zeroclaw"              # 選填
channels = ["#zeroclaw"]
allowed_users = ["*"]
server_password = ""                # 選填
nickserv_password = ""              # 選填
sasl_password = ""                  # 選填
verify_tls = true
```

### 4.11 Lark

```toml
[channels_config.lark]
app_id = "your_lark_app_id"
app_secret = "your_lark_app_secret"
encrypt_key = ""                    # 選填
verification_token = ""             # 選填
allowed_users = ["*"]
mention_only = false                # 舊版備援；當 group_reply.mode 未設定時使用
use_feishu = false
receive_mode = "websocket"          # 或 "webhook"
port = 8081                          # webhook 模式所需

[channels_config.lark.group_reply]
mode = "all_messages"               # 選填：all_messages | mention_only
allowed_sender_ids = []             # 選填：可繞過 mention 閘門的發送者 open_id
```

### 4.12 飛書（Feishu）

```toml
[channels_config.feishu]
app_id = "your_lark_app_id"
app_secret = "your_lark_app_secret"
encrypt_key = ""                    # 選填
verification_token = ""             # 選填
allowed_users = ["*"]
receive_mode = "websocket"          # 或 "webhook"
port = 8081                          # webhook 模式所需

[channels_config.feishu.group_reply]
mode = "all_messages"               # 選填：all_messages | mention_only
allowed_sender_ids = []             # 選填：可繞過 mention 閘門的發送者 open_id
```

遷移備註：

- 舊版設定 `[channels_config.lark] use_feishu = true` 仍支援向下相容。
- 新設定建議使用 `[channels_config.feishu]`。
- 入站 `image` 訊息會轉換為多模態標記（`[IMAGE:data:image/...;base64,...]`）。
- 若圖片下載失敗，ZeroClaw 會轉送備援文字而非靜默丟棄訊息。

### 4.13 Nostr

```toml
[channels_config.nostr]
private_key = "nsec1..."                   # hex 或 nsec bech32（靜態加密）
# relays 預設為 relay.damus.io, nos.lol, relay.primal.net, relay.snort.social
# relays = ["wss://relay.damus.io", "wss://nos.lol"]
allowed_pubkeys = ["hex-or-npub"]          # 空 = 拒絕全部，"*" = 允許全部
```

Nostr 同時支援 NIP-04（舊版加密私訊）與 NIP-17（gift-wrapped 私密訊息）。回覆會自動使用發送者所用的相同協定。私鑰在 `secrets.encrypt = true`（預設值）時透過 `SecretStore` 進行靜態加密。

互動式上線精靈支援：

```bash
zeroclaw onboard --interactive
```

精靈現已包含專屬的 **Lark** 與**飛書**步驟，具備：

- 透過官方開放平台認證端點進行憑證驗證
- 接收模式選擇（`websocket` 或 `webhook`）
- 選填的 webhook 驗證權杖提示（建議啟用以加強回呼真實性檢查）

執行階段權杖行為：

- `tenant_access_token` 會根據認證回應中的 `expire`/`expires_in` 設定更新截止時間進行快取。
- 當飛書/Lark 回傳 HTTP `401` 或業務錯誤碼 `99991663`（`Invalid access token`）時，傳送請求會在權杖失效後自動重試一次。
- 若重試仍回傳權杖無效回應，傳送呼叫會以上游狀態/本文失敗，方便故障排除。

### 4.14 DingTalk

```toml
[channels_config.dingtalk]
client_id = "ding-app-key"
client_secret = "ding-app-secret"
allowed_users = ["*"]
```

### 4.15 QQ

```toml
[channels_config.qq]
app_id = "qq-app-id"
app_secret = "qq-app-secret"
allowed_users = ["*"]
receive_mode = "webhook" # webhook（預設）或 websocket（舊版備援）
environment = "production" # production（預設）或 sandbox
```

備註：

- `webhook` 模式為目前預設，於 `POST /qq` 服務入站回呼。
- 設定 `environment = "sandbox"` 可指向 `https://sandbox.api.sgroup.qq.com` 進行未發布機器人測試。
- QQ 驗證挑戰（`op = 13`）會自動使用 `app_secret` 簽名。
- 當 `X-Bot-Appid` 存在時會進行檢查，必須與 `app_id` 相符。
- 設定 `receive_mode = "websocket"` 以保留舊版 gateway WS 接收路徑。

### 4.16 Nextcloud Talk

```toml
[channels_config.nextcloud_talk]
base_url = "https://cloud.example.com"
app_token = "nextcloud-talk-app-token"
webhook_secret = "optional-webhook-secret"  # 選填但建議設定
allowed_users = ["*"]
```

備註：

- 入站 webhook 端點：`POST /nextcloud-talk`。
- 簽名驗證使用 `X-Nextcloud-Talk-Random` 與 `X-Nextcloud-Talk-Signature`。
- 若設定了 `webhook_secret`，無效簽名會被以 `401` 拒絕。
- `ZEROCLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET` 可覆寫設定中的 secret。
- 完整操作手冊請參閱 [nextcloud-talk-setup.md](./nextcloud-talk-setup.md)。

### 4.16 Linq

```toml
[channels_config.linq]
api_token = "linq-partner-api-token"
from_phone = "+15551234567"
signing_secret = "optional-webhook-signing-secret"  # 選填但建議設定
allowed_senders = ["*"]
```

備註：

- Linq 使用 Partner V3 API 支援 iMessage、RCS 與 SMS。
- 入站 webhook 端點：`POST /linq`。
- 簽名驗證使用 `X-Webhook-Signature`（HMAC-SHA256）與 `X-Webhook-Timestamp`。
- 若設定了 `signing_secret`，無效或過時（>300 秒）的簽名會被拒絕。
- `ZEROCLAW_LINQ_SIGNING_SECRET` 可覆寫設定中的 secret。
- `allowed_senders` 使用 E.164 電話號碼格式（例如 `+1234567890`）。

### 4.17 iMessage

```toml
[channels_config.imessage]
allowed_contacts = ["*"]
```

---

## 5. 驗證工作流程

1. 設定一個頻道，使用寬鬆的允許清單（`"*"`）進行初始驗證。
2. 執行：

```bash
zeroclaw onboard --channels-only
zeroclaw daemon
```

1. 從預期的發送者傳送一則訊息。
2. 確認收到回覆。
3. 將允許清單從 `"*"` 收緊為明確的 ID。

---

## 6. 故障排除清單

若頻道看似已連線但未回應：

1. 確認發送者身分已被正確的允許清單欄位所允許。
2. 確認機器人帳號在目標房間/頻道中有成員資格與權限。
3. 確認權杖/密鑰有效（且未過期/撤銷）。
4. 確認傳輸模式假設：
   - 輪詢/websocket 頻道不需要公開入站 HTTP
   - webhook 頻道需要可達的 HTTPS 回呼
5. 設定變更後重啟 `zeroclaw daemon`。

針對 Matrix 加密房間，請使用：
- [Matrix E2EE 指南](./matrix-e2ee-guide.md)

---

## 7. 操作附錄：日誌關鍵字矩陣

使用此附錄進行快速分流。先比對日誌關鍵字，再依循上述故障排除步驟。

### 7.1 建議擷取指令

```bash
RUST_LOG=info zeroclaw daemon 2>&1 | tee /tmp/zeroclaw.log
```

然後過濾頻道/gateway 事件：

```bash
rg -n "Matrix|Telegram|Discord|Slack|Mattermost|Signal|WhatsApp|Email|IRC|Lark|DingTalk|QQ|iMessage|Nostr|Webhook|Channel" /tmp/zeroclaw.log
```

### 7.2 關鍵字表

| 元件 | 啟動/健康訊號 | 授權/政策訊號 | 傳輸/失敗訊號 |
|---|---|---|---|
| Telegram | `Telegram channel listening for messages...` | `Telegram: ignoring message from unauthorized user:` | `Telegram poll error:` / `Telegram parse error:` / `Telegram polling conflict (409):` |
| Discord | `Discord: connected and identified` | `Discord: ignoring message from unauthorized user:` | `Discord: received Reconnect (op 7)` / `Discord: received Invalid Session (op 9)` |
| Slack | `Slack channel listening on #` / `Slack channel_id not set (or '*'); listening across all accessible channels.` | `Slack: ignoring message from unauthorized user:` | `Slack poll error:` / `Slack parse error:` / `Slack channel discovery failed:` |
| Mattermost | `Mattermost channel listening on` | `Mattermost: ignoring message from unauthorized user:` | `Mattermost poll error:` / `Mattermost parse error:` |
| Matrix | `Matrix channel listening on room` / `Matrix room ... is encrypted; E2EE decryption is enabled via matrix-sdk.` | `Matrix whoami failed; falling back to configured session hints for E2EE session restore:` / `Matrix whoami failed while resolving listener user_id; using configured user_id hint:` | `Matrix sync error: ... retrying...` |
| Signal | `Signal channel listening via SSE on` | （允許清單檢查由 `allowed_from` 執行） | `Signal SSE returned ...` / `Signal SSE connect error:` |
| WhatsApp（頻道） | `WhatsApp channel active (webhook mode).` / `WhatsApp Web connected successfully` | `WhatsApp: ignoring message from unauthorized number:` / `WhatsApp Web: message from ... not in allowed list` | `WhatsApp send failed:` / `WhatsApp Web stream error:` |
| Webhook / WhatsApp（gateway） | `WhatsApp webhook verified successfully` | `Webhook: rejected — not paired / invalid bearer token` / `Webhook: rejected request — invalid or missing X-Webhook-Secret` / `WhatsApp webhook verification failed — token mismatch` | `Webhook JSON parse error:` |
| Email | `Email polling every ...` / `Email sent to ...` | `Blocked email from ...` | `Email poll failed:` / `Email poll task panicked:` |
| IRC | `IRC channel connecting to ...` / `IRC registered as ...` | （允許清單檢查由 `allowed_users` 執行） | `IRC SASL authentication failed (...)` / `IRC server does not support SASL...` / `IRC nickname ... is in use, trying ...` |
| Lark / 飛書 | `Lark: WS connected` / `Lark event callback server listening on` | `Lark WS: ignoring ... (not in allowed_users)` / `Lark: ignoring message from unauthorized user:` | `Lark: ping failed, reconnecting` / `Lark: heartbeat timeout, reconnecting` / `Lark: WS read error:` |
| DingTalk | `DingTalk: connected and listening for messages...` | `DingTalk: ignoring message from unauthorized user:` | `DingTalk WebSocket error:` / `DingTalk: message channel closed` |
| QQ | `QQ: connected and identified` | `QQ: ignoring C2C message from unauthorized user:` / `QQ: ignoring group message from unauthorized user:` | `QQ: received Reconnect (op 7)` / `QQ: received Invalid Session (op 9)` / `QQ: message channel closed` |
| Nextcloud Talk（gateway） | `POST /nextcloud-talk — Nextcloud Talk bot webhook` | `Nextcloud Talk webhook signature verification failed` / `Nextcloud Talk: ignoring message from unauthorized actor:` | `Nextcloud Talk send failed:` / `LLM error for Nextcloud Talk message:` |
| iMessage | `iMessage channel listening (AppleScript bridge)...` | （聯絡人允許清單由 `allowed_contacts` 執行） | `iMessage poll error:` |
| Nostr | `Nostr channel listening as npub1...` | `Nostr: ignoring NIP-04 message from unauthorized pubkey:` / `Nostr: ignoring NIP-17 message from unauthorized pubkey:` | `Failed to decrypt NIP-04 message:` / `Failed to unwrap NIP-17 gift wrap:` / `Nostr relay pool shut down` |

### 7.3 執行階段監控器關鍵字

若特定頻道任務崩潰或退出，`channels/mod.rs` 中的頻道監控器會發出：

- `Channel <name> exited unexpectedly; restarting`
- `Channel <name> error: ...; restarting`
- `Channel message worker crashed:`

這些訊息表示自動重啟行為已啟動，應檢查先前的日誌以找出根本原因。
