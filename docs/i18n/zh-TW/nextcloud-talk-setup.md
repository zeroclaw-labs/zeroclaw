# Nextcloud Talk 設定指南（繁體中文）

本指南涵蓋 ZeroClaw 的 Nextcloud Talk 原生整合設定。

## 1. 此整合的功能

- 透過 `POST /nextcloud-talk` 接收 Talk bot webhook 事件。
- 當有設定密鑰時，驗證 webhook 簽章（HMAC-SHA256）。
- 透過 Nextcloud OCS API 將 bot 回覆傳送回 Talk 聊天室。

## 2. 設定

在 `~/.zeroclaw/config.toml` 中加入以下區段：

```toml
[channels_config.nextcloud_talk]
base_url = "https://cloud.example.com"
app_token = "nextcloud-talk-app-token"
webhook_secret = "optional-webhook-secret"
allowed_users = ["*"]
```

欄位說明：

- `base_url`：Nextcloud 基礎 URL。
- `app_token`：Bot 應用程式 token，用於 OCS 傳送 API 的 `Authorization: Bearer <token>` 驗證。
- `webhook_secret`：用於驗證 `X-Nextcloud-Talk-Signature` 的共用密鑰。
- `allowed_users`：允許的 Nextcloud actor ID（`[]` 拒絕所有，`"*"` 允許所有）。

環境變數覆寫：

- 當設定了 `ZEROCLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET` 時，會覆寫 `webhook_secret`。

## 3. 閘道端點

啟動 daemon 或 gateway 並暴露 webhook 端點：

```bash
zeroclaw daemon
# 或
zeroclaw gateway --host 127.0.0.1 --port 3000
```

將你的 Nextcloud Talk bot webhook URL 設定為：

- `https://<your-public-url>/nextcloud-talk`

## 4. 簽章驗證機制

當設定了 `webhook_secret` 時，ZeroClaw 會驗證：

- 標頭 `X-Nextcloud-Talk-Random`
- 標頭 `X-Nextcloud-Talk-Signature`

驗證公式：

- `hex(hmac_sha256(secret, random + raw_request_body))`

若驗證失敗，閘道會回傳 `401 Unauthorized`。

## 5. 訊息路由行為

- ZeroClaw 會忽略 bot 發起的 webhook 事件（`actorType = bots`）。
- ZeroClaw 會忽略非訊息/系統事件。
- 回覆路由使用 webhook payload 中的 Talk 聊天室 token。

## 6. 快速驗證清單

1. 首次驗證時，設定 `allowed_users = ["*"]`。
2. 在目標 Talk 聊天室中傳送一則測試訊息。
3. 確認 ZeroClaw 在同一聊天室中接收並回覆。
4. 將 `allowed_users` 限縮為明確的 actor ID。

## 7. 疑難排解

- `404 Nextcloud Talk not configured`：缺少 `[channels_config.nextcloud_talk]` 設定區段。
- `401 Invalid signature`：`webhook_secret`、random 標頭或原始 body 簽章不一致。
- 收到 webhook `200` 但沒有回覆：事件被過濾（bot/系統/未授權使用者/非訊息 payload）。
