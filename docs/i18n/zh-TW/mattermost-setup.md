# Mattermost 整合指南（繁體中文）

ZeroClaw 透過 Mattermost REST API v4 原生支援 Mattermost 整合。此整合特別適用於自架、私有或隔離網路環境，在這些場景中通訊主權是核心需求。

## 前置條件

1.  **Mattermost 伺服器**：一個正在運作的 Mattermost 實例（自架或雲端）。
2.  **Bot 帳號**：
    - 前往 **主選單 > 整合 > Bot 帳號**。
    - 點選 **新增 Bot 帳號**。
    - 設定使用者名稱（例如 `zeroclaw-bot`）。
    - 啟用 **post:all** 和 **channel:read** 權限（或其他適當的範圍）。
    - 儲存 **Access Token**。
3.  **頻道 ID**：
    - 開啟你希望 bot 監聽的 Mattermost 頻道。
    - 點選頻道標題並選擇 **檢視資訊**。
    - 複製 **ID**（例如 `7j8k9l...`）。

## 設定

在 `config.toml` 的 `[channels_config]` 區段中加入以下內容：

```toml
[channels_config.mattermost]
url = "https://mm.your-domain.com"
bot_token = "your-bot-access-token"
channel_id = "your-channel-id"
allowed_users = ["user-id-1", "user-id-2"]
thread_replies = true
mention_only = true
```

### 設定欄位說明

| 欄位 | 說明 |
|---|---|
| `url` | Mattermost 伺服器的基礎 URL。 |
| `bot_token` | Bot 帳號的 Personal Access Token。 |
| `channel_id` | （選填）要監聽的頻道 ID。`listen` 模式為必填。 |
| `allowed_users` | （選填）允許與 bot 互動的 Mattermost 使用者 ID 清單。使用 `["*"]` 允許所有人。 |
| `thread_replies` | （選填）是否將頂層使用者訊息以討論串方式回覆。預設值：`true`。已存在討論串中的回覆一律保持在同一討論串內。 |
| `mention_only` | （選填）設為 `true` 時，僅處理明確提及 bot 使用者名稱的訊息（例如 `@zeroclaw-bot`）。預設值：`false`。 |

## 討論串對話

ZeroClaw 在兩種模式下都支援 Mattermost 討論串：
- 如果使用者在現有討論串中發送訊息，ZeroClaw 一律在同一討論串中回覆。
- 若 `thread_replies = true`（預設），頂層訊息會以該貼文的討論串方式回覆。
- 若 `thread_replies = false`，頂層訊息會直接在頻道根層級回覆。

## 僅提及模式

當 `mention_only = true` 時，ZeroClaw 會在 `allowed_users` 授權之後額外套用一層過濾：

- 沒有明確提及 bot 的訊息會被忽略。
- 含有 `@bot_username` 的訊息會被處理。
- 在將內容傳送給模型之前，`@bot_username` 標記會被移除。

此模式適用於繁忙的共用頻道，可減少不必要的模型呼叫。

## 安全注意事項

Mattermost 整合專為**通訊主權**而設計。透過自架 Mattermost 伺服器，你的代理程式通訊紀錄完全保留在自有基礎架構內，避免第三方雲端的日誌記錄。
