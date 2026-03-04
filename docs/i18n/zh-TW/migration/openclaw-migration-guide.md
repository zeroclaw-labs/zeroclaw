# OpenClaw → ZeroClaw 遷移指南

本指南將引導您將 OpenClaw 部署遷移至 ZeroClaw。內容涵蓋設定轉換、端點變更，以及您需要了解的架構差異。

## 快速開始

```bash
# 1. 轉換您的 OpenClaw 設定
python scripts/convert-openclaw-config.py ~/.openclaw/openclaw.json -o config.toml

# 2. 相容層已內建於 ZeroClaw — 無需複製任何檔案。
#    端點實作位於 src/gateway/openclaw_compat.rs，
#    且已在 src/gateway/mod.rs 中接入路由。

# 3. 建置與部署
cargo build --release
```

---

## 架構：變更內容與原因

OpenClaw 的設計是一個 **OpenAI 相容的 API 伺服器**。您像呼叫遠端 LLM 一樣使用它 — 傳送 `messages[]`，取回一個 completion。Gateway 本質上是一個加入了系統提示詞和工具能力的代理伺服器。

ZeroClaw 是一個**獨立的訊息閘道**。它在內部擁有完整的 agent 迴圈。Channel（WhatsApp、Linq、Nextcloud Talk）傳送單一訊息字串，ZeroClaw 負責處理一切：系統提示詞建構、工具呼叫、記憶回溯、上下文增強和回應生成。

這代表沒有內建的 `/v1/chat/completions` 端點可以執行完整的 agent 迴圈。`openai_compat.rs` 中的端點使用較簡易的聊天路徑，不包含工具或記憶功能。

### 本工具套件新增的功能

兩個新的端點彌補了這個差距：

| 端點 | 格式 | Agent 迴圈 | 使用場景 |
|------|------|-----------|----------|
| `POST /api/chat` | ZeroClaw 原生 JSON | 完整（含工具 + 記憶） | **建議**用於新整合 |
| `POST /v1/chat/completions` | OpenAI 相容 | 完整（含工具 + 記憶） | 現有呼叫端的**直接替換相容** |

兩個端點都經由 `run_gateway_chat_with_tools` → `agent::process_message` 路由，這與 Linq、WhatsApp 及所有原生 channel 使用的程式碼路徑相同。

---

## 端點參考

### POST /api/chat（建議使用）

簡潔的 ZeroClaw 原生端點。

**請求：**
```json
{
  "message": "What's on my schedule today?",
  "session_id": "optional-session-id",
  "context": [
    "User: Can you check my calendar?",
    "Assistant: Sure, let me look that up."
  ]
}
```

- `message`（必填）：使用者的訊息。
- `session_id`（選填）：將記憶操作限定在某個 session 範圍內。
- `context`（選填）：近期對話歷史紀錄。用於提供語意記憶以外的滾動上下文給 agent。

**回應：**
```json
{
  "reply": "Here's what I found on your schedule...",
  "model": "us.anthropic.claude-sonnet-4-6",
  "session_id": "optional-session-id"
}
```

**驗證方式：** `Authorization: Bearer <gateway_token>`

### POST /v1/chat/completions（相容墊片）

為 OpenAI 相容呼叫端提供的直接替換方案。接受標準 OpenAI 格式，提取最後一則使用者訊息及對話歷史，並經由完整的 agent 迴圈路由。

**請求：** 標準 OpenAI chat completions 格式。
```json
{
  "messages": [
    {"role": "user", "content": "Hello"},
    {"role": "assistant", "content": "Hi! How can I help?"},
    {"role": "user", "content": "What's my email?"}
  ],
  "model": "claude-sonnet-4-6",
  "stream": false
}
```

**回應：** 標準 OpenAI chat completions 格式。
```json
{
  "id": "chatcmpl-abc123",
  "object": "chat.completion",
  "created": 1709000000,
  "model": "claude-sonnet-4-6",
  "choices": [{
    "index": 0,
    "message": {"role": "assistant", "content": "Your email is..."},
    "finish_reason": "stop"
  }],
  "usage": {"prompt_tokens": 42, "completion_tokens": 15, "total_tokens": 57}
}
```

**串流模式：** 設定 `stream: true` 以取得 SSE 回應。注意：串流是模擬的 — 完整回應會先生成，再以分塊方式串流傳送。這是為了維持 API 相容性。

**驗證方式：** `Authorization: Bearer <gateway_token>`

---

## 設定對應表

### Provider 與模型

| OpenClaw (JSON) | ZeroClaw (TOML) |
|-----------------|-----------------|
| `agent.model = "anthropic/claude-opus-4-6"` | `default_provider = "anthropic"` + `default_model = "claude-opus-4-6"` |
| `agent.model = "openai/gpt-4o"` | `default_provider = "openai"` + `default_model = "gpt-4o"` |
| `agent.temperature = 0.7` | `default_temperature = 0.7` |

Bedrock 設定：
```toml
default_provider = "bedrock"
default_model = "us.anthropic.claude-sonnet-4-6"
```

### Gateway

| OpenClaw | ZeroClaw |
|----------|----------|
| `gateway.port = 18789` | `[gateway]` `port = 42617` |
| `gateway.bind = "127.0.0.1"` | `[gateway]` `host = "127.0.0.1"` |
| `gateway.auth.mode = "token"` | `[gateway]` `require_pairing = true` |

### 記憶體

OpenClaw 將狀態儲存在 `~/.openclaw/`。ZeroClaw 使用可設定的後端：

```toml
[memory]
backend = "sqlite"              # sqlite | postgres | qdrant | markdown | none
auto_save = true
embedding_provider = "openai"   # openai | custom:URL | none
embedding_model = "text-embedding-3-small"
vector_weight = 0.7             # 語意搜尋權重
keyword_weight = 0.3            # BM25 關鍵字搜尋權重
```

### Channel

| OpenClaw Channel | ZeroClaw 狀態 |
|------------------|--------------|
| WhatsApp | 原生支援（`/whatsapp`） |
| Telegram | 原生支援（channels_config） |
| Discord | 原生支援（channels_config） |
| Slack | 原生支援（channels_config） |
| Matrix | 原生支援（channels_config） |
| Lark/飛書 | 原生支援 |
| Nextcloud Talk | 原生支援（`/nextcloud-talk`） |
| Linq | 原生支援（`/linq`） |
| Signal | 不支援，請使用 /api/chat 橋接 |
| iMessage | 不支援，請使用 /api/chat 橋接 |
| Google Chat | 不支援，請使用 /api/chat 橋接 |
| MS Teams | 不支援，請使用 /api/chat 橋接 |
| WebChat | 不支援，請使用 /api/chat 或 /v1/chat/completions |

對於不支援的 channel，請將您現有的整合指向 ZeroClaw 的 `/api/chat` 端點，取代 OpenClaw 的 `/v1/chat/completions`。

---

## 呼叫端遷移範例

### 遷移前（OpenClaw）

```typescript
const response = await fetch(`https://${host}/v1/chat/completions`, {
  method: "POST",
  headers: {
    "Authorization": `Bearer ${apiKey}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    model: "anthropic/claude-sonnet-4-6",
    messages: conversationHistory,
  }),
});
const data = await response.json();
const reply = data.choices[0].message.content;
```

### 遷移後 — 方案 A：使用 /api/chat（建議）

```typescript
const response = await fetch(`https://${host}/api/chat`, {
  method: "POST",
  headers: {
    "Authorization": `Bearer ${gatewayToken}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    message: userMessage,
    session_id: sessionId,
    context: recentMessages.map(m => `${m.role}: ${m.content}`),
  }),
});
const data = await response.json();
const reply = data.reply;
```

### 遷移後 — 方案 B：使用 /v1/chat/completions（零程式碼變更）

```typescript
// 與之前相同的程式碼 — 只需將目標指向 ZeroClaw 主機並使用 gateway token。
// 相容墊片會處理格式轉換。
const response = await fetch(`https://${host}/v1/chat/completions`, {
  method: "POST",
  headers: {
    "Authorization": `Bearer ${gatewayToken}`,
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    model: "claude-sonnet-4-6",
    messages: conversationHistory,
  }),
});
```

---

## 對話上下文：須知事項

ZeroClaw 的 `process_message` 每次呼叫都會重新開始。它使用**語意記憶回溯**（SQLite 混合搜尋，結合 embeddings 與 BM25）來浮現相關的過往上下文，而非有序的對話歷史。

這在實務上代表：

| 查詢類型 | 可行？ | 原因 |
|---------|-------|------|
| 「我的 email 是什麼？」 | 通常可行 | 如果之前討論過，語意回溯會找到 |
| 「你剛才說了什麼？」 | 不可行 | 沒有滾動歷史 — 無法取得前一輪對話 |
| 「總結我們的對話」 | 部分可行 | 語意回溯會浮現片段，但非完整歷史 |

**緩解方式：** 兩個端點都接受上下文注入。傳入近期對話歷史：
- `/api/chat`：使用 `context` 陣列欄位
- `/v1/chat/completions`：相容墊片會自動從 `messages[]` 陣列中提取最後 10 則訊息，並作為上下文前置

若要完整支援對話歷史，需要後續對 ZeroClaw agent 迴圈進行修改，以直接接受 `messages[]` 參數。

---

## 設定轉換器使用方式

```bash
# 基本轉換
python scripts/convert-openclaw-config.py ~/.openclaw/openclaw.json

# 指定輸出路徑
python scripts/convert-openclaw-config.py ~/.openclaw/openclaw.json -o ~/.zeroclaw/config.toml

# 預覽但不寫入
python scripts/convert-openclaw-config.py ~/.openclaw/openclaw.json --dry-run
```

轉換器會處理：provider/模型解析、gateway 設定、記憶體預設值、agent 組態和 channel 對應。它會產生遷移備註，標示出需要手動調整的部分。

---

## 部署檢查清單

- [ ] 執行設定轉換器並檢閱輸出
- [ ] 設定 API 金鑰：`export ZEROCLAW_API_KEY='...'`
- [ ] 建置：`cargo build --release`
- [ ] 部署（Docker 或原生）
- [ ] 配對：`curl -X POST http://<host>:<port>/pair -H 'X-Pairing-Code: ...'`
- [ ] 驗證健康狀態：`curl http://<host>:<port>/health`
- [ ] 測試 /api/chat：`curl -X POST http://<host>:<port>/api/chat -H 'Authorization: Bearer ...' -d '{"message":"hello"}'`
- [ ] 測試 /v1/chat/completions：`curl -X POST http://<host>:<port>/v1/chat/completions -H 'Authorization: Bearer ...' -d '{"messages":[{"role":"user","content":"hello"}]}'`
- [ ] 更新呼叫端指向新主機
- [ ] 監控日誌是否有錯誤

---

## 疑難排解

**`/v1/chat/completions` 回傳 405：** 端點未註冊。請確認您執行的 ZeroClaw 建置版本包含 `openclaw_compat` 模組（檢查 `src/gateway/mod.rs` 中的路由註冊）。

**401 未授權：** 已啟用配對功能，但您未傳送有效的 bearer token。請先執行 `/pair` 流程。

**Agent 回傳空白或通用回應：** 檢查 `default_provider` 和 `default_model` 是否設定正確，以及 provider API 金鑰是否可用（透過環境變數或設定檔）。

**「No user message found」：** 相容墊片預期 messages 陣列中至少有一則 `role: "user"` 的訊息。

**記憶功能無法運作：** 確認 `[memory]` backend 設定為 `"none"` 以外的值，且 `embedding_provider` 已設定有效的 API 金鑰以供 embedding 生成使用。
