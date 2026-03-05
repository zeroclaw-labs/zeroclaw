# ZeroClaw 設定參照（繁體中文）（操作者導向）

本文件為常用設定區段與預設值的高訊噪比參照。

最後驗證日期：**2026 年 2 月 28 日**。

啟動時的設定檔路徑解析順序：

1. `ZEROCLAW_WORKSPACE` 覆寫（若已設定）
2. 已持久化的 `~/.zeroclaw/active_workspace.toml` 標記（若存在）
3. 預設 `~/.zeroclaw/config.toml`

ZeroClaw 在啟動時以 `INFO` 層級記錄已解析的設定：

- `Config loaded` 包含欄位：`path`、`workspace`、`source`、`initialized`

結構描述匯出指令：

- `zeroclaw config schema`（將 JSON Schema draft 2020-12 輸出到 stdout）

## 核心鍵值

| 鍵 | 預設值 | 備註 |
|---|---|---|
| `default_provider` | `openrouter` | 供應商 ID 或別名 |
| `provider_api` | 未設定 | 可選的 API 模式，適用於 `custom:<url>` 供應商：`openai-chat-completions` 或 `openai-responses` |
| `default_model` | `anthropic/claude-sonnet-4-6` | 經由選定供應商路由的模型 |
| `default_temperature` | `0.7` | 模型溫度 |
| `model_support_vision` | 未設定（`None`） | 目前供應商 / 模型的視覺支援覆寫 |

備註：

- `model_support_vision = true` 強制啟用視覺支援（例如在 Ollama 上執行 `llava`）。
- `model_support_vision = false` 強制停用視覺支援。
- 未設定時保持供應商的內建預設。
- 環境變數覆寫：`ZEROCLAW_MODEL_SUPPORT_VISION` 或 `MODEL_SUPPORT_VISION`（值：`true`/`false`/`1`/`0`/`yes`/`no`/`on`/`off`）。

## `[observability]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `backend` | `none` | 可觀測性後端：`none`、`noop`、`log`、`prometheus`、`otel`、`opentelemetry` 或 `otlp` |
| `otel_endpoint` | `http://localhost:4318` | 後端為 `otel` 時使用的 OTLP HTTP 端點 |
| `otel_service_name` | `zeroclaw` | 發送到 OTLP 收集器的服務名稱 |
| `runtime_trace_mode` | `none` | 執行階段追蹤儲存模式：`none`、`rolling` 或 `full` |
| `runtime_trace_path` | `state/runtime-trace.jsonl` | 執行階段追蹤 JSONL 路徑（相對於工作區，除非為絕對路徑） |
| `runtime_trace_max_entries` | `200` | `runtime_trace_mode = "rolling"` 時保留的最大事件數 |

備註：

- `backend = "otel"` 使用 OTLP HTTP 匯出，搭配阻塞式匯出器客戶端，以便能從非 Tokio 環境安全地發送 span 和指標。
- 別名值 `opentelemetry` 和 `otlp` 對應相同的 OTel 後端。
- 執行階段追蹤用於除錯工具呼叫失敗和格式錯誤的模型工具載荷。它們可能包含模型輸出文字，因此在共用主機上建議預設停用。
- 查詢執行階段追蹤的指令：
  - `zeroclaw doctor traces --limit 20`
  - `zeroclaw doctor traces --event tool_call_result --contains \"error\"`
  - `zeroclaw doctor traces --id <trace-id>`

範例：

```toml
[observability]
backend = "otel"
otel_endpoint = "http://localhost:4318"
otel_service_name = "zeroclaw"
runtime_trace_mode = "rolling"
runtime_trace_path = "state/runtime-trace.jsonl"
runtime_trace_max_entries = 200
```

## 環境變數供應商覆寫

供應商選擇也可透過環境變數控制。優先順序為：

1. `ZEROCLAW_PROVIDER`（明確覆寫，非空時一律優先）
2. `PROVIDER`（舊式備援，僅在設定檔中的供應商未設定或仍為 `openrouter` 時套用）
3. `config.toml` 中的 `default_provider`

容器使用者注意事項：

- 若你的 `config.toml` 設定了明確的自訂供應商（如 `custom:https://.../v1`），來自 Docker / 容器環境的預設 `PROVIDER=openrouter` 將不再取代它。
- 當你有意讓執行階段環境覆寫非預設的已設定供應商時，請使用 `ZEROCLAW_PROVIDER`。
- OpenAI 相容 Responses 備援傳輸：
  - `ZEROCLAW_RESPONSES_WEBSOCKET=1` 強制 WebSocket 優先模式（`wss://.../responses`）適用於相容供應商。
  - `ZEROCLAW_RESPONSES_WEBSOCKET=0` 強制僅 HTTP 模式。
  - 未設定 = 自動（僅當端點主機為 `api.openai.com` 時才 WebSocket 優先，若 WebSocket 失敗則回退至 HTTP）。

## `[agent]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `compact_context` | `true` | 為 true 時：bootstrap_max_chars=6000，rag_chunk_limit=2。適用於 13B 或更小的模型 |
| `max_tool_iterations` | `20` | 每則使用者訊息的最大工具呼叫迴圈回合數，適用於 CLI、閘道和頻道 |
| `max_history_messages` | `50` | 每個工作階段保留的最大對話歷史訊息數 |
| `parallel_tools` | `false` | 在單次迭代中啟用並行工具執行 |
| `tool_dispatcher` | `auto` | 工具分派策略 |
| `loop_detection_no_progress_threshold` | `3` | 相同工具 + 引數產生相同輸出達此次數時觸發迴圈偵測。`0` 停用 |
| `loop_detection_ping_pong_cycles` | `2` | A→B→A→B 交替模式的循環次數門檻。`0` 停用 |
| `loop_detection_failure_streak` | `3` | 相同工具連續失敗次數門檻。`0` 停用 |

備註：

- 設定 `max_tool_iterations = 0` 會回退至安全預設值 `20`。
- 若頻道訊息超過此值，執行階段會回傳：`Agent exceeded maximum tool iterations (<value>)`。
- 在 CLI、閘道和頻道工具迴圈中，當待處理呼叫不需要核准閘門時，多個獨立的工具呼叫預設會同時執行；結果順序保持穩定。
- `parallel_tools` 適用於 `Agent::turn()` API 介面。它不影響 CLI、閘道或頻道處理器使用的執行階段迴圈。
- **迴圈偵測**會在 `max_tool_iterations` 耗盡前介入。首次偵測時，代理程式會收到自我修正提示；若迴圈持續則提前停止代理程式。偵測是結果感知的：重複呼叫但產生*不同*輸出（真正的進展）不會觸發。將任何門檻設為 `0` 即可停用該偵測器。

## `[security.otp]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `false` | 為敏感動作 / 網域啟用 OTP 閘門 |
| `method` | `totp` | OTP 方法（`totp`、`pairing`、`cli-prompt`） |
| `token_ttl_secs` | `30` | TOTP 時間步長視窗（秒） |
| `cache_valid_secs` | `300` | 最近驗證過的 OTP 碼快取視窗（秒） |
| `gated_actions` | `["shell","file_write","browser_open","browser","memory_forget"]` | 受 OTP 保護的工具動作 |
| `gated_domains` | `[]` | 需要 OTP 的明確網域模式（`*.example.com`、`login.example.com`） |
| `gated_domain_categories` | `[]` | 網域預設類別（`banking`、`medical`、`government`、`identity_providers`） |

備註：

- 網域模式支援萬用字元 `*`。
- 類別預設在驗證時會展開為策展過的網域集合。
- 無效的網域 glob 或未知的類別在啟動時會快速失敗。
- 當 `enabled = true` 且無 OTP 密鑰存在時，ZeroClaw 會產生一組並印出一次性的註冊 URI。

範例：

```toml
[security.otp]
enabled = true
method = "totp"
token_ttl_secs = 30
cache_valid_secs = 300
gated_actions = ["shell", "browser_open"]
gated_domains = ["*.chase.com", "accounts.google.com"]
gated_domain_categories = ["banking"]
```

## `[security.estop]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `false` | 啟用緊急停止狀態機與 CLI |
| `state_file` | `~/.zeroclaw/estop-state.json` | 持久化的 estop 狀態路徑 |
| `require_otp_to_resume` | `true` | 恢復操作前需要 OTP 驗證 |

備註：

- Estop 狀態以原子方式持久化，並在啟動時重新載入。
- 損壞 / 無法讀取的 estop 狀態會回退至失敗關閉的 `kill_all`。
- 使用 CLI 指令 `zeroclaw estop` 啟用，`zeroclaw estop resume` 清除層級。

## `[security.url_access]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `block_private_ip` | `true` | 預設阻擋本地 / 私有 / 鏈路本地 / 多播位址 |
| `allow_cidrs` | `[]` | 允許繞過私有 IP 阻擋的 CIDR 範圍（`100.64.0.0/10`、`198.18.0.0/15`） |
| `allow_domains` | `[]` | 在 DNS 檢查前繞過私有 IP 阻擋的網域模式（`internal.example`、`*.svc.local`） |
| `allow_loopback` | `false` | 允許迴路目標（`localhost`、`127.0.0.1`、`::1`） |
| `require_first_visit_approval` | `false` | 首次存取未見過的網域前需要人工明確確認 |
| `enforce_domain_allowlist` | `false` | 要求所有 URL 目標符合 `domain_allowlist`（除了工具層級的允許清單以外） |
| `domain_allowlist` | `[]` | 跨 URL 工具共用的全域可信網域允許清單 |
| `domain_blocklist` | `[]` | 跨 URL 工具共用的全域網域拒絕清單（最高優先級） |
| `approved_domains` | `[]` | 由人工操作者授予的持久化首次造訪核准 |

備註：

- 此政策由 `browser_open`、`http_request` 和 `web_fetch` 共用。
- `browser` 自動化（`action = "open"`）也遵循此政策。
- 工具層級的允許清單仍然適用。`allow_domains` / `allow_cidrs` 僅覆寫私有 / 本地阻擋。
- `domain_blocklist` 在允許清單之前評估；被阻擋的主機一律被拒絕。
- 當 `require_first_visit_approval = true` 時，未見過的網域會被拒絕，直到加入 `approved_domains`（或符合 `domain_allowlist`）。
- DNS 重新繫結保護保持啟用：解析到本地 / 私有 IP 的位址會被拒絕，除非明確允許。
- 代理程式可在執行階段透過 `web_access_config`（`action=get|set|check_url`）檢視 / 更新這些設定。
- 在受管理模式下，`web_access_config` 的變更仍需正常工具核准，除非已明確設為自動核准。

範例：

```toml
[security.url_access]
block_private_ip = true
allow_cidrs = ["100.64.0.0/10", "198.18.0.0/15"]
allow_domains = ["internal.example", "*.svc.local"]
allow_loopback = false
require_first_visit_approval = true
enforce_domain_allowlist = false
domain_allowlist = ["docs.rs", "github.com", "*.rust-lang.org"]
domain_blocklist = ["*.malware.test"]
approved_domains = ["example.com"]
```

執行階段工作流程（`web_access_config`）：

1. 啟動嚴格優先模式（在審查前拒絕未知網域）：

```json
{"action":"set","require_first_visit_approval":true,"enforce_domain_allowlist":false}
```

2. 在存取前先模擬測試目標 URL：

```json
{"action":"check_url","url":"https://docs.rs"}
```

3. 人工確認後，為後續執行持久化核准：

```json
{"action":"set","add_approved_domains":["docs.rs"]}
```

4. 升級為嚴格的僅允許清單模式（建議用於正式環境代理程式）：

```json
{"action":"set","enforce_domain_allowlist":true,"domain_allowlist":["docs.rs","github.com","*.rust-lang.org"]}
```

5. 跨所有 URL 工具緊急拒絕某網域：

```json
{"action":"set","add_domain_blocklist":["*.malware.test"]}
```

操作指引：

- 使用 `approved_domains` 進行漸進式新增和臨時核准。
- 使用 `domain_allowlist` 設定穩定的長期可信網域。
- 使用 `domain_blocklist` 進行即時全域拒絕；它一律覆寫允許規則。
- 將 `allow_domains` 專注於私有網路繞過場景（`internal.example`、`*.svc.local`）。

環境變數覆寫：

- `ZEROCLAW_URL_ACCESS_BLOCK_PRIVATE_IP` / `URL_ACCESS_BLOCK_PRIVATE_IP`
- `ZEROCLAW_URL_ACCESS_ALLOW_LOOPBACK` / `URL_ACCESS_ALLOW_LOOPBACK`
- `ZEROCLAW_URL_ACCESS_REQUIRE_FIRST_VISIT_APPROVAL` / `URL_ACCESS_REQUIRE_FIRST_VISIT_APPROVAL`
- `ZEROCLAW_URL_ACCESS_ENFORCE_DOMAIN_ALLOWLIST` / `URL_ACCESS_ENFORCE_DOMAIN_ALLOWLIST`
- `ZEROCLAW_URL_ACCESS_ALLOW_CIDRS` / `URL_ACCESS_ALLOW_CIDRS`（逗號分隔）
- `ZEROCLAW_URL_ACCESS_ALLOW_DOMAINS` / `URL_ACCESS_ALLOW_DOMAINS`（逗號分隔）
- `ZEROCLAW_URL_ACCESS_DOMAIN_ALLOWLIST` / `URL_ACCESS_DOMAIN_ALLOWLIST`（逗號分隔）
- `ZEROCLAW_URL_ACCESS_DOMAIN_BLOCKLIST` / `URL_ACCESS_DOMAIN_BLOCKLIST`（逗號分隔）
- `ZEROCLAW_URL_ACCESS_APPROVED_DOMAINS` / `URL_ACCESS_APPROVED_DOMAINS`（逗號分隔）

## `[security.syscall_anomaly]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `true` | 啟用基於指令輸出遙測的系統呼叫異常偵測 |
| `strict_mode` | `false` | 即使在基準線中，觀察到被拒絕的系統呼叫時仍發出異常 |
| `alert_on_unknown_syscall` | `true` | 對不在基準線中的系統呼叫名稱發出警告 |
| `max_denied_events_per_minute` | `5` | 被拒絕系統呼叫激增警告的門檻 |
| `max_total_events_per_minute` | `120` | 總系統呼叫事件激增警告的門檻 |
| `max_alerts_per_minute` | `30` | 每滾動分鐘的全域警告預算上限 |
| `alert_cooldown_secs` | `20` | 相同異常警告之間的冷卻時間 |
| `log_path` | `syscall-anomalies.log` | JSONL 異常日誌路徑 |
| `baseline_syscalls` | 內建允許清單 | 預期的系統呼叫設定檔；未知條目會觸發警告 |

備註：

- 偵測會消耗來自指令 `stdout`/`stderr` 的 seccomp/audit 提示。
- Linux audit 行中的數值系統呼叫 ID 在可用時會對應到常見的 x86_64 名稱。
- 警告預算和冷卻時間可在重複重試期間減少重複 / 雜訊事件。
- `max_denied_events_per_minute` 必須小於或等於 `max_total_events_per_minute`。

範例：

```toml
[security.syscall_anomaly]
enabled = true
strict_mode = false
alert_on_unknown_syscall = true
max_denied_events_per_minute = 5
max_total_events_per_minute = 120
max_alerts_per_minute = 30
alert_cooldown_secs = 20
log_path = "syscall-anomalies.log"
baseline_syscalls = ["read", "write", "openat", "close", "execve", "futex"]
```

## `[security.perplexity_filter]`

輕量級、選擇性啟用的對抗性後綴過濾器，在頻道和閘道訊息管線中的供應商呼叫前執行。

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enable_perplexity_filter` | `false` | 啟用 LLM 前的統計後綴異常阻擋 |
| `perplexity_threshold` | `18.0` | 字元類別 bigram 困惑度門檻 |
| `suffix_window_chars` | `64` | 用於異常評分的尾部字元視窗 |
| `min_prompt_chars` | `32` | 過濾器開始評估前的最小提示長度 |
| `symbol_ratio_threshold` | `0.20` | 後綴視窗中觸發阻擋的最低標點符號比例 |

備註：

- 此過濾器預設停用，以保持基準延遲 / 行為不受影響。
- 偵測器結合字元類別困惑度與 GCG 式 token 啟發式方法。
- 輸入僅在滿足異常條件時被阻擋；正常的自然語言提示不受影響。
- 典型的每訊息額外開銷在 debug 安全的本地測試中設計為低於 `50ms`，在 release 版本中大幅降低。

範例：

```toml
[security.perplexity_filter]
enable_perplexity_filter = true
perplexity_threshold = 16.5
suffix_window_chars = 72
min_prompt_chars = 40
symbol_ratio_threshold = 0.25
```

## `[security.outbound_leak_guard]`

控制工具輸出清理後頻道回覆中的出站憑證洩漏處理。

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `true` | 啟用頻道回應的出站憑證洩漏掃描 |
| `action` | `redact` | 洩漏處理模式：`redact`（遮蔽後送出）或 `block`（不送出原始內容） |
| `sensitivity` | `0.7` | 洩漏偵測器靈敏度（`0.0` 至 `1.0`，越高越積極） |

備註：

- 偵測使用與既有遮蔽防護相同的洩漏偵測器（API 金鑰、JWT、私密金鑰、高熵 Token 等）。
- `action = "redact"` 保持目前行為（安全預設的向後相容性）。
- `action = "block"` 更加嚴格，會回傳安全的備用訊息，而非可能含敏感內容的原始內容。
- 當此防護啟用時，`/v1/chat/completions` 串流回應會經過安全緩衝，在清理後才發出，以避免在最終掃描前洩漏原始 token delta。

範例：

```toml
[security.outbound_leak_guard]
enabled = true
action = "block"
sensitivity = 0.9
```

## `[agents.<name>]`

委派子代理程式設定。`[agents]` 下的每個鍵定義一個具名的子代理程式，主要代理程式可以委派工作給它。

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `provider` | _必要_ | 供應商名稱（例如 `"ollama"`、`"openrouter"`、`"anthropic"`） |
| `model` | _必要_ | 子代理程式的模型名稱 |
| `system_prompt` | 未設定 | 可選的系統提示覆寫 |
| `api_key` | 未設定 | 可選的 API 金鑰覆寫（當 `secrets.encrypt = true` 時加密儲存） |
| `temperature` | 未設定 | 子代理程式的溫度覆寫 |
| `max_depth` | `3` | 巢狀委派的最大遞迴深度 |
| `agentic` | `false` | 為子代理程式啟用多回合工具呼叫迴圈模式 |
| `allowed_tools` | `[]` | 代理模式的工具允許清單 |
| `max_iterations` | `10` | 代理模式的最大工具呼叫迭代次數 |

備註：

- `agentic = false` 保持既有的單次提示→回應委派行為。
- `agentic = true` 需要 `allowed_tools` 中至少有一個匹配項目。
- `delegate` 工具被排除於子代理程式允許清單之外，以防止重入式委派迴圈。

```toml
[agents.researcher]
provider = "openrouter"
model = "anthropic/claude-sonnet-4-6"
system_prompt = "You are a research assistant."
max_depth = 2
agentic = true
allowed_tools = ["web_search", "http_request", "file_read"]
max_iterations = 8

[agents.coder]
provider = "ollama"
model = "qwen2.5-coder:32b"
temperature = 0.2
```

## `[research]`

研究階段允許代理程式在產生主要回應前透過工具收集資訊。

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `false` | 啟用研究階段 |
| `trigger` | `never` | 研究觸發策略：`never`、`always`、`keywords`、`length`、`question` |
| `keywords` | `["find", "search", "check", "investigate"]` | 觸發研究的關鍵字（當 trigger = `keywords` 時） |
| `min_message_length` | `50` | 觸發研究的最小訊息長度（當 trigger = `length` 時） |
| `max_iterations` | `5` | 研究階段中的最大工具呼叫次數 |
| `show_progress` | `true` | 向使用者顯示研究進度 |

備註：

- 研究階段**預設停用**（`trigger = never`）。
- 啟用後，代理程式會先透過工具（grep、file_read、shell、記憶體搜尋）收集事實，然後使用收集到的脈絡進行回應。
- 研究在主要代理程式回合前執行，不計入 `agent.max_tool_iterations`。
- 觸發策略：
  - `never` -- 停用研究（預設）
  - `always` -- 每則使用者訊息都執行研究
  - `keywords` -- 訊息包含清單中任何關鍵字時執行研究
  - `length` -- 訊息長度超過 `min_message_length` 時執行研究
  - `question` -- 訊息包含 '?' 時執行研究

範例：

```toml
[research]
enabled = true
trigger = "keywords"
keywords = ["find", "show", "check", "how many"]
max_iterations = 3
show_progress = true
```

代理程式在回應以下類型的查詢前會先研究程式庫：
- "Find all TODO in src/"
- "Show contents of main.rs"
- "How many files in the project?"

## `[runtime]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `kind` | `native` | 執行階段後端：`native`、`docker` 或 `wasm` |
| `reasoning_enabled` | 未設定（`None`） | 全域推理 / 思考覆寫，適用於支援明確控制的供應商 |

備註：

- `reasoning_enabled = false` 明確停用支援供應商的供應商端推理（目前為 `ollama`，透過請求欄位 `think: false`）。
- `reasoning_enabled = true` 明確要求支援供應商的推理（`ollama` 上的 `think: true`）。
- 未設定時保持供應商預設。
- 已棄用的相容別名：`runtime.reasoning_level` 仍被接受，但應遷移至 `provider.reasoning_level`。
- `runtime.kind = "wasm"` 啟用受能力約束的模組執行，並停用 Shell / 程式風格的執行。

### `[runtime.wasm]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `tools_dir` | `"tools/wasm"` | 包含 `.wasm` 模組的工作區相對目錄 |
| `fuel_limit` | `1000000` | 每次模組呼叫的指令預算 |
| `memory_limit_mb` | `64` | 每模組記憶體上限（MB） |
| `max_module_size_mb` | `50` | 允許的最大 `.wasm` 檔案大小（MB） |
| `allow_workspace_read` | `false` | 允許 WASM 主機呼叫讀取工作區檔案（前瞻性功能） |
| `allow_workspace_write` | `false` | 允許 WASM 主機呼叫寫入工作區檔案（前瞻性功能） |
| `allowed_hosts` | `[]` | WASM 主機呼叫的明確網路主機允許清單（前瞻性功能） |

備註：

- `allowed_hosts` 項目必須為正規化的 `host` 或 `host:port` 字串；當 `runtime.wasm.security.strict_host_validation = true` 時，萬用字元、協定和路徑會被拒絕。
- 呼叫時的能力覆寫由 `runtime.wasm.security.capability_escalation_mode` 控制：
  - `deny`（預設）：拒絕超出執行階段基準線的提權。
  - `clamp`：將請求的能力降低至基準線。

### `[runtime.wasm.security]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `require_workspace_relative_tools_dir` | `true` | 要求 `runtime.wasm.tools_dir` 為工作區相對路徑，並拒絕 `..` 穿越 |
| `reject_symlink_modules` | `true` | 在執行時阻擋符號連結的 `.wasm` 模組檔案 |
| `reject_symlink_tools_dir` | `true` | 當 `runtime.wasm.tools_dir` 本身為符號連結時阻擋執行 |
| `strict_host_validation` | `true` | 對無效主機條目在設定 / 呼叫時失敗，而非靜默丟棄 |
| `capability_escalation_mode` | `"deny"` | 提權政策：`deny` 或 `clamp` |
| `module_hash_policy` | `"warn"` | 模組完整性政策：`disabled`、`warn` 或 `enforce` |
| `module_sha256` | `{}` | 可選的模組名稱對應固定 SHA-256 摘要的對應表 |

備註：

- `module_sha256` 的鍵必須與模組名稱匹配（不含 `.wasm`），且僅使用 `[A-Za-z0-9_-]`。
- `module_sha256` 的值必須為 64 字元的十六進位 SHA-256 字串。
- `module_hash_policy = "warn"` 允許執行，但會記錄缺少 / 不匹配的摘要。
- `module_hash_policy = "enforce"` 在缺少 / 不匹配摘要時阻擋執行，且需要至少一個固定值。

WASM 設定範本：

- `dev/config.wasm.dev.toml`
- `dev/config.wasm.staging.toml`
- `dev/config.wasm.prod.toml`

## `[provider]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `reasoning_level` | 未設定（`None`） | 推理力度 / 層級覆寫，適用於支援明確層級的供應商（目前為 OpenAI Codex `/responses`） |
| `transport` | 未設定（`None`） | 供應商傳輸覆寫（`auto`、`websocket`、`sse`） |

備註：

- 支援的值：`minimal`、`low`、`medium`、`high`、`xhigh`（不區分大小寫）。
- 設定後，會覆寫 OpenAI Codex 請求的 `ZEROCLAW_CODEX_REASONING_EFFORT`。
- 未設定時回退至 `ZEROCLAW_CODEX_REASONING_EFFORT`（若存在），否則預設為 `xhigh`。
- 若 `provider.reasoning_level` 和已棄用的 `runtime.reasoning_level` 同時設定，供應商層級的值優先。
- `provider.transport` 以不區分大小寫的方式正規化（`ws` 為 `websocket` 的別名；`http` 為 `sse` 的別名）。
- 對於 OpenAI Codex，預設傳輸模式為 `auto`（WebSocket 優先搭配 SSE 備援）。
- OpenAI Codex 的傳輸覆寫優先順序：
  1. `[[model_routes]].transport`（路由特定）
  2. `PROVIDER_TRANSPORT` / `ZEROCLAW_PROVIDER_TRANSPORT` / `ZEROCLAW_CODEX_TRANSPORT`
  3. `provider.transport`
  4. 舊式 `ZEROCLAW_RESPONSES_WEBSOCKET`（布林值）
- 環境變數覆寫設定後會取代已設定的 `provider.transport`。

## `[skills]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `open_skills_enabled` | `false` | 選擇性啟用社群 `open-skills` 儲存庫的載入 / 同步 |
| `open_skills_dir` | 未設定 | 可選的 `open-skills` 本地路徑（啟用時預設為 `$HOME/open-skills`） |
| `prompt_injection_mode` | `full` | 技能提示詳細度：`full`（行內指示 / 工具）或 `compact`（僅名稱 / 描述 / 位置） |
| `clawhub_token` | 未設定 | 可選的 Bearer Token，用於認證式 ClawhHub 技能下載 |

備註：

- 安全優先的預設：ZeroClaw **不會**複製或同步 `open-skills`，除非 `open_skills_enabled = true`。
- 環境變數覆寫：
  - `ZEROCLAW_OPEN_SKILLS_ENABLED` 接受 `1/0`、`true/false`、`yes/no`、`on/off`。
  - `ZEROCLAW_OPEN_SKILLS_DIR` 非空時覆寫儲存庫路徑。
  - `ZEROCLAW_SKILLS_PROMPT_MODE` 接受 `full` 或 `compact`。
- 啟用旗標的優先順序：`ZEROCLAW_OPEN_SKILLS_ENABLED` → `config.toml` 中的 `skills.open_skills_enabled` → 預設 `false`。
- `prompt_injection_mode = "compact"` 建議在低脈絡本地模型上使用，以減少啟動提示大小，同時保持技能檔案可按需取用。
- 技能載入和 `zeroclaw skills install` 都會套用靜態安全稽核。包含符號連結、腳本類檔案、高風險 Shell 載荷片段或不安全 Markdown 連結穿越的技能會被拒絕。
- `clawhub_token` 在從 ClawhHub 下載時以 `Authorization: Bearer <token>` 發送。請在 [https://clawhub.ai](https://clawhub.ai) 登入後取得 Token。若 API 對匿名請求回傳 429（速率限制）或 401（未授權），則需要此 Token。

**ClawhHub Token 範例：**

```toml
[skills]
clawhub_token = "your-token-here"
```

## `[composio]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `false` | 啟用 Composio 託管 OAuth 工具 |
| `api_key` | 未設定 | `composio` 工具使用的 Composio API 金鑰 |
| `entity_id` | `default` | connect/execute 呼叫時發送的預設 `user_id` |

備註：

- 向後相容性：舊式的 `enable = true` 被接受為 `enabled = true` 的別名。
- 若 `enabled = false` 或 `api_key` 缺失，`composio` 工具不會被註冊。
- ZeroClaw 以 `toolkit_versions=latest` 請求 Composio v3 工具，並以 `version="latest"` 執行工具，以避免過時的預設工具版本。
- 典型流程：呼叫 `connect`，完成瀏覽器 OAuth，然後對目標工具動作執行 `execute`。
- 若 Composio 回傳缺少已連線帳號參照的錯誤，請呼叫 `list_accounts`（可選帶 `app`），並將回傳的 `connected_account_id` 傳給 `execute`。

## `[cost]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `false` | 啟用費用追蹤 |
| `daily_limit_usd` | `10.00` | 每日支出上限（美元） |
| `monthly_limit_usd` | `100.00` | 每月支出上限（美元） |
| `warn_at_percent` | `80` | 支出達到此百分比時發出警告 |
| `allow_override` | `false` | 允許透過 `--override` 旗標超出預算的請求 |

備註：

- 當 `enabled = true` 時，執行階段會追蹤每請求的費用估算並強制執行日 / 月上限。
- 達到 `warn_at_percent` 門檻時會發出警告，但請求繼續執行。
- 達到上限時，除非 `allow_override = true` 且傳入 `--override` 旗標，否則請求會被拒絕。

## `[identity]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `format` | `openclaw` | 身分格式：`"openclaw"`（預設）或 `"aieos"` |
| `aieos_path` | 未設定 | AIEOS JSON 檔案路徑（相對於工作區） |
| `aieos_inline` | 未設定 | 行內 AIEOS JSON（檔案路徑的替代方案） |

備註：

- 使用 `format = "aieos"` 搭配 `aieos_path` 或 `aieos_inline` 來載入 AIEOS / OpenClaw 身分文件。
- `aieos_path` 和 `aieos_inline` 只應設定其一；`aieos_path` 優先。

## `[multimodal]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `max_images` | `4` | 每個請求接受的最大圖片標記數 |
| `max_image_size_mb` | `5` | Base64 編碼前的每張圖片大小上限 |
| `allow_remote_fetch` | `false` | 允許從標記中擷取 `http(s)` 圖片 URL |

備註：

- 執行階段接受使用者訊息中的圖片標記，語法為：``[IMAGE:<source>]``。
- 支援的來源：
  - 本地檔案路徑（例如 ``[IMAGE:/tmp/screenshot.png]``）
- Data URI（例如 ``[IMAGE:data:image/png;base64,...]``）
- 僅在 `allow_remote_fetch = true` 時支援遠端 URL
- 允許的 MIME 類型：`image/png`、`image/jpeg`、`image/webp`、`image/gif`、`image/bmp`。
- 當目前供應商不支援視覺時，請求會以結構化的能力錯誤（`capability=vision`）失敗，而非靜默丟棄圖片。

## `[browser]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `false` | 啟用瀏覽器工具（`browser_open` 和 `browser`） |
| `allowed_domains` | `[]` | `browser_open` 和 `browser` 的允許網域（精確 / 子網域匹配，或 `"*"` 允許所有公開網域） |
| `browser_open` | `default` | `browser_open` 使用的瀏覽器：`disable`、`brave`、`chrome`、`firefox`、`edge`（`msedge` 別名）、`default` |
| `session_name` | 未設定 | 瀏覽器工作階段名稱（用於代理程式瀏覽器自動化） |
| `backend` | `agent_browser` | 瀏覽器自動化後端：`"agent_browser"`、`"rust_native"`、`"computer_use"` 或 `"auto"` |
| `auto_backend_priority` | `[]` | `backend = "auto"` 的優先順序（例如 `["agent_browser","rust_native","computer_use"]`） |
| `agent_browser_command` | `agent-browser` | agent-browser CLI 的可執行檔 / 路徑 |
| `agent_browser_extra_args` | `[]` | 每個 agent-browser 指令前置的額外引數 |
| `agent_browser_timeout_ms` | `30000` | 每個 agent-browser 動作指令的逾時時間 |
| `native_headless` | `true` | rust-native 後端的無頭模式 |
| `native_webdriver_url` | `http://127.0.0.1:9515` | rust-native 後端的 WebDriver 端點 URL |
| `native_chrome_path` | 未設定 | 可選的 rust-native 後端 Chrome/Chromium 可執行檔路徑 |

### `[browser.computer_use]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `endpoint` | `http://127.0.0.1:8787/v1/actions` | computer-use 動作（OS 層級滑鼠 / 鍵盤 / 截圖）的 Sidecar 端點 |
| `api_key` | 未設定 | 可選的 computer-use Sidecar Bearer Token（加密儲存） |
| `timeout_ms` | `15000` | 每個動作請求的逾時時間（毫秒） |
| `allow_remote_endpoint` | `false` | 允許 computer-use Sidecar 使用遠端 / 公開端點 |
| `window_allowlist` | `[]` | 可選的視窗標題 / 程式允許清單，轉發給 Sidecar 政策 |
| `max_coordinate_x` | 未設定 | 可選的座標動作 X 軸邊界 |
| `max_coordinate_y` | 未設定 | 可選的座標動作 Y 軸邊界 |

備註：

- `browser_open` 是簡單的 URL 開啟器；`browser` 是完整的瀏覽器自動化（開啟 / 點擊 / 輸入 / 捲動 / 截圖）。
- 當 `backend = "computer_use"` 時，代理程式將瀏覽器動作委派給 `computer_use.endpoint` 的 Sidecar。
- `allow_remote_endpoint = false`（預設）拒絕任何非迴路端點，以防止意外的公開暴露。
- 使用 `window_allowlist` 限制 Sidecar 可互動的 OS 視窗。

## `[http_request]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `false` | 啟用 `http_request` 工具以進行 API 互動 |
| `allowed_domains` | `[]` | HTTP 請求的允許網域（精確 / 子網域匹配，或 `"*"` 允許所有公開網域） |
| `max_response_size` | `1000000` | 最大回應大小（位元組，預設：1 MB） |
| `timeout_secs` | `30` | 請求逾時時間（秒） |
| `user_agent` | `ZeroClaw/1.0` | 出站 HTTP 請求的 User-Agent 標頭 |
| `credential_profiles` | `{}` | 可選的具名環境變數認證設定檔，由工具引數 `credential_profile` 使用 |

備註：

- 預設拒絕：若 `allowed_domains` 為空，所有 HTTP 請求都會被拒絕。
- 使用精確網域或子網域匹配（例如 `"api.example.com"`、`"example.com"`），或 `"*"` 允許任何公開網域。
- 即使設定 `"*"`，本地 / 私有目標仍然被阻擋。
- Shell 的 `curl`/`wget` 被歸類為高風險，可能被自主性政策阻擋。建議使用 `http_request` 進行直接 HTTP 呼叫。
- `credential_profiles` 讓執行環境從環境變數注入認證標頭，使代理程式無需在工具引數中傳入原始 Token 即可呼叫需認證的 API。

範例：

```toml
[http_request]
enabled = true
allowed_domains = ["api.github.com", "api.linear.app"]

[http_request.credential_profiles.github]
header_name = "Authorization"
env_var = "GITHUB_TOKEN"
value_prefix = "Bearer "

[http_request.credential_profiles.linear]
header_name = "Authorization"
env_var = "LINEAR_API_KEY"
value_prefix = ""
```

然後以如下方式呼叫 `http_request`：

```json
{
  "url": "https://api.github.com/user",
  "credential_profile": "github"
}
```

使用 `credential_profile` 時，不要同時在 `args.headers` 中設定相同的標頭鍵（不區分大小寫），否則請求會因標頭衝突而被拒絕。

## `[web_fetch]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `false` | 啟用 `web_fetch` 頁面轉文字擷取 |
| `provider` | `fast_html2md` | 擷取 / 渲染後端：`fast_html2md`、`nanohtml2text`、`firecrawl` |
| `api_key` | 未設定 | 需要 API 金鑰的供應商後端（例如 `firecrawl`） |
| `api_url` | 未設定 | 可選的 API URL 覆寫（自架 / 替代端點） |
| `allowed_domains` | `["*"]` | 網域允許清單（`"*"` 允許所有公開網域） |
| `blocked_domains` | `[]` | 在允許清單之前套用的拒絕清單 |
| `max_response_size` | `500000` | 回傳載荷的最大大小（位元組） |
| `timeout_secs` | `30` | 請求逾時時間（秒） |
| `user_agent` | `ZeroClaw/1.0` | 擷取請求的 User-Agent 標頭 |

備註：

- `web_fetch` 針對網頁的摘要 / 資料擷取進行最佳化。
- 重導向目標會重新驗證允許 / 拒絕網域政策。
- 即使 `allowed_domains = ["*"]`，本地 / 私有網路目標仍然被阻擋。

## `[web_search]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `false` | 啟用 `web_search_tool` |
| `provider` | `duckduckgo` | 搜尋後端：`duckduckgo`（`ddg` 別名）、`brave`、`firecrawl`、`tavily`、`perplexity`、`exa`、`jina` |
| `fallback_providers` | `[]` | 主要供應商失敗後依序嘗試的備援供應商 |
| `retries_per_provider` | `0` | 切換到下一個供應商前的重試次數 |
| `retry_backoff_ms` | `250` | 重試之間的延遲（毫秒） |
| `api_key` | 未設定 | 通用供應商金鑰（用於 `firecrawl`/`tavily`，作為專用供應商金鑰的備援） |
| `api_url` | 未設定 | 可選的 API URL 覆寫 |
| `brave_api_key` | 未設定 | 專用的 Brave 金鑰（`provider = "brave"` 時必要，除非已設定 `api_key`） |
| `perplexity_api_key` | 未設定 | 專用的 Perplexity 金鑰 |
| `exa_api_key` | 未設定 | 專用的 Exa 金鑰 |
| `jina_api_key` | 未設定 | 可選的 Jina 金鑰 |
| `domain_filter` | `[]` | 可選的網域過濾器，轉發給支援的供應商 |
| `language_filter` | `[]` | 可選的語言過濾器，轉發給支援的供應商 |
| `country` | 未設定 | 可選的國家提示，用於支援的供應商 |
| `recency_filter` | 未設定 | 可選的時效過濾器，用於支援的供應商 |
| `max_tokens` | 未設定 | 可選的 Token 預算，用於支援的供應商（例如 Perplexity） |
| `max_tokens_per_page` | 未設定 | 可選的每頁 Token 預算，用於支援的供應商 |
| `exa_search_type` | `auto` | Exa 搜尋模式：`auto`、`keyword`、`neural` |
| `exa_include_text` | `false` | 在 Exa 回應中包含文字載荷 |
| `jina_site_filters` | `[]` | 可選的 Jina 搜尋網站過濾器 |
| `max_results` | `5` | 回傳的最大搜尋結果數（須為 1-10） |
| `timeout_secs` | `15` | 請求逾時時間（秒） |
| `user_agent` | `ZeroClaw/1.0` | 搜尋請求的 User-Agent 標頭 |

備註：

- 若 DuckDuckGo 在你的網路環境回傳 `403`/`429`，請切換供應商至 `brave`、`perplexity`、`exa`，或設定 `fallback_providers`。
- `web_search` 負責找到候選 URL；搭配 `web_fetch` 進行頁面內容擷取。
- 代理程式可在執行階段透過 `web_search_config` 工具（`action=get|set|list_providers`）修改這些設定。
- 在受管理模式下，`web_search_config` 的變更仍需正常工具核准，除非已明確設為自動核准。
- 無效的供應商名稱、`exa_search_type` 以及超出範圍的重試 / 結果數 / 逾時值在設定驗證時會被拒絕。

建議的韌性設定檔：

```toml
[web_search]
enabled = true
provider = "perplexity"
fallback_providers = ["exa", "jina", "duckduckgo"]
retries_per_provider = 1
retry_backoff_ms = 300
max_results = 5
timeout_secs = 20
```

執行階段工作流程（`web_search_config`）：

1. 檢視可用供應商和目前設定快照：

```json
{"action":"list_providers"}
```

```json
{"action":"get"}
```

2. 設定主要供應商和備援鏈：

```json
{"action":"set","provider":"perplexity","fallback_providers":["exa","jina","duckduckgo"]}
```

3. 調整供應商特定選項：

```json
{"action":"set","exa_search_type":"neural","exa_include_text":true}
```

```json
{"action":"set","jina_site_filters":["docs.rs","github.com"]}
```

4. 為區域感知查詢新增地理 / 語言 / 時效過濾器：

```json
{"action":"set","country":"US","language_filter":["en"],"recency_filter":"week"}
```

環境變數覆寫：

- `ZEROCLAW_WEB_SEARCH_ENABLED` / `WEB_SEARCH_ENABLED`
- `ZEROCLAW_WEB_SEARCH_PROVIDER` / `WEB_SEARCH_PROVIDER`
- `ZEROCLAW_WEB_SEARCH_MAX_RESULTS` / `WEB_SEARCH_MAX_RESULTS`
- `ZEROCLAW_WEB_SEARCH_TIMEOUT_SECS` / `WEB_SEARCH_TIMEOUT_SECS`
- `ZEROCLAW_WEB_SEARCH_FALLBACK_PROVIDERS` / `WEB_SEARCH_FALLBACK_PROVIDERS`（逗號分隔）
- `ZEROCLAW_WEB_SEARCH_RETRIES_PER_PROVIDER` / `WEB_SEARCH_RETRIES_PER_PROVIDER`
- `ZEROCLAW_WEB_SEARCH_RETRY_BACKOFF_MS` / `WEB_SEARCH_RETRY_BACKOFF_MS`
- `ZEROCLAW_WEB_SEARCH_DOMAIN_FILTER` / `WEB_SEARCH_DOMAIN_FILTER`（逗號分隔）
- `ZEROCLAW_WEB_SEARCH_LANGUAGE_FILTER` / `WEB_SEARCH_LANGUAGE_FILTER`（逗號分隔）
- `ZEROCLAW_WEB_SEARCH_COUNTRY` / `WEB_SEARCH_COUNTRY`
- `ZEROCLAW_WEB_SEARCH_RECENCY_FILTER` / `WEB_SEARCH_RECENCY_FILTER`
- `ZEROCLAW_WEB_SEARCH_MAX_TOKENS` / `WEB_SEARCH_MAX_TOKENS`
- `ZEROCLAW_WEB_SEARCH_MAX_TOKENS_PER_PAGE` / `WEB_SEARCH_MAX_TOKENS_PER_PAGE`
- `ZEROCLAW_WEB_SEARCH_EXA_SEARCH_TYPE` / `WEB_SEARCH_EXA_SEARCH_TYPE`
- `ZEROCLAW_WEB_SEARCH_EXA_INCLUDE_TEXT` / `WEB_SEARCH_EXA_INCLUDE_TEXT`
- `ZEROCLAW_WEB_SEARCH_JINA_SITE_FILTERS` / `WEB_SEARCH_JINA_SITE_FILTERS`（逗號分隔）
- `ZEROCLAW_BRAVE_API_KEY` / `BRAVE_API_KEY`
- `ZEROCLAW_PERPLEXITY_API_KEY` / `PERPLEXITY_API_KEY`
- `ZEROCLAW_EXA_API_KEY` / `EXA_API_KEY`
- `ZEROCLAW_JINA_API_KEY` / `JINA_API_KEY`

## `[gateway]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `host` | `127.0.0.1` | 繫結位址 |
| `port` | `42617` | 閘道監聽埠 |
| `require_pairing` | `true` | 在 Bearer 認證前要求配對 |
| `allow_public_bind` | `false` | 阻擋意外的公開暴露 |

## `[gateway.node_control]`（實驗性）

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `false` | 啟用節點控制骨架端點（`POST /api/node-control`） |
| `auth_token` | `null` | 可選的額外共用 Token，透過 `X-Node-Control-Token` 檢查 |
| `allowed_node_ids` | `[]` | `node.describe`/`node.invoke` 的允許清單（`[]` 接受任何） |

## `[autonomy]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `level` | `supervised` | `read_only`、`supervised` 或 `full` |
| `workspace_only` | `true` | 拒絕絕對路徑輸入，除非明確停用 |
| `allowed_commands` | _Shell 執行必要_ | 可執行名稱、明確可執行檔路徑或 `"*"` 的允許清單 |
| `forbidden_paths` | 內建受保護清單 | 明確路徑拒絕清單（預設為系統路徑 + 敏感點目錄） |
| `allowed_roots` | `[]` | 正規化後允許的工作區外額外根目錄 |
| `max_actions_per_hour` | `20` | 每政策動作預算 |
| `max_cost_per_day_cents` | `500` | 每政策支出防護欄 |
| `require_approval_for_medium_risk` | `true` | 中風險指令的核准閘門 |
| `block_high_risk_commands` | `true` | 高風險指令的硬性阻擋 |
| `allow_sensitive_file_reads` | `false` | 允許 `file_read` 讀取敏感檔案 / 目錄（例如 `.env`、`.aws/credentials`、私密金鑰） |
| `allow_sensitive_file_writes` | `false` | 允許 `file_write`/`file_edit` 寫入敏感檔案 / 目錄（例如 `.env`、`.aws/credentials`、私密金鑰） |
| `auto_approve` | `[]` | 一律自動核准的工具操作 |
| `always_ask` | `[]` | 一律需要核准的工具操作 |
| `non_cli_excluded_tools` | `[]` | 從非 CLI 頻道工具規格中隱藏的工具 |
| `non_cli_approval_approvers` | `[]` | 可選的允許清單，限制誰能執行非 CLI 核准管理指令 |
| `non_cli_natural_language_approval_mode` | `direct` | 核准管理指令的自然語言行為（`direct`、`request_confirm`、`disabled`） |
| `non_cli_natural_language_approval_mode_by_channel` | `{}` | 自然語言核准模式的逐頻道覆寫對應表 |

備註：

- `level = "full"` 跳過 Shell 執行的中風險核准閘門，但仍強制執行已設定的防護欄。
- 存取工作區外的路徑需要 `allowed_roots`，即使 `workspace_only = false`。
- `allowed_roots` 支援絕對路徑、`~/...` 和工作區相對路徑。
- `allowed_commands` 項目可以是指令名稱（例如 `"git"`）、明確可執行檔路徑（例如 `"/usr/bin/antigravity"`），或 `"*"` 以允許任何指令名稱 / 路徑（風險閘門仍然適用）。
- `file_read` 預設阻擋敏感的含密鑰檔案 / 目錄。僅在受控的除錯工作階段中設定 `allow_sensitive_file_reads = true`。
- `file_write` 和 `file_edit` 預設阻擋敏感的含密鑰檔案 / 目錄。僅在受控的緊急應變工作階段中設定 `allow_sensitive_file_writes = true`。
- `file_read`、`file_write` 和 `file_edit` 拒絕多重連結檔案（硬連結防護），以降低透過硬連結逃逸繞過工作區路徑的風險。
- Shell 分隔符 / 運算子的解析是引號感知的。引號引數內的 `;` 等字元被視為字面值，而非指令分隔符。
- 未引號的 Shell 串接 / 運算子仍受政策檢查強制執行（`;`、`|`、`&&`、`||`、背景串接和重導向）。
- 在非 CLI 頻道的受管理模式下，操作者可透過以下方式持久化人工核准的工具：
  - 一步流程：`/approve <tool>`。
  - 兩步流程：`/approve-request <tool>` 然後 `/approve-confirm <request-id>`（同一發送者 + 同一聊天 / 頻道）。
  兩種路徑都會寫入 `autonomy.auto_approve` 並從 `autonomy.always_ask` 移除該工具。
- `non_cli_natural_language_approval_mode` 控制自然語言核准意圖的嚴格程度：
  - `direct`（預設）：自然語言核准立即授權（適合私人聊天）。
  - `request_confirm`：自然語言核准建立待處理請求，需明確確認。
  - `disabled`：自然語言核准指令被拒絕；僅使用斜線指令。
- `non_cli_natural_language_approval_mode_by_channel` 可為特定頻道覆寫該模式（鍵為頻道名稱如 `telegram`、`discord`、`slack`）。
  - 範例：全域保持 `direct`，但強制 `discord = "request_confirm"` 用於團隊聊天。
- `non_cli_approval_approvers` 可限制誰能執行核准指令（`/approve*`、`/unapprove`、`/approvals`）：
  - `*` 允許所有頻道准入的發送者。
  - `alice` 允許任何頻道上的發送者 `alice`。
  - `telegram:alice` 僅允許該頻道 + 發送者組合。
  - `telegram:*` 允許 Telegram 上的任何發送者。
  - `*:alice` 允許 `alice` 在任何頻道上。
- 使用 `/unapprove <tool>` 從 `autonomy.auto_approve` 移除已持久化的核准。
- `/approve-pending` 列出目前發送者 + 聊天 / 頻道範圍的待處理請求。
- 若工具在核准後仍無法使用，請檢查 `autonomy.non_cli_excluded_tools`（執行階段的 `/approvals` 會顯示此清單）。頻道執行階段會自動從 `config.toml` 重新載入此清單。

```toml
[autonomy]
workspace_only = false
forbidden_paths = ["/etc", "/root", "/proc", "/sys", "~/.ssh", "~/.gnupg", "~/.aws"]
allowed_roots = ["~/Desktop/projects", "/opt/shared-repo"]
```

## `[memory]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `backend` | `sqlite` | `sqlite`、`lucid`、`markdown`、`none` |
| `auto_save` | `true` | 僅持久化使用者陳述的輸入（助理輸出被排除） |
| `embedding_provider` | `none` | `none`、`openai` 或自訂端點 |
| `embedding_model` | `text-embedding-3-small` | 嵌入模型 ID，或 `hint:<name>` 路由 |
| `embedding_dimensions` | `1536` | 所選嵌入模型的預期向量大小 |
| `vector_weight` | `0.7` | 混合排序的向量權重 |
| `keyword_weight` | `0.3` | 混合排序的關鍵字權重 |

備註：

- 記憶體脈絡注入會忽略舊式的 `assistant_resp*` 自動儲存鍵，以防止舊的模型生成摘要被當成事實處理。

## `[[model_routes]]` 和 `[[embedding_routes]]`

使用路由提示，讓整合可在模型 ID 演進時保持穩定名稱。

### `[[model_routes]]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `hint` | _必要_ | 任務提示名稱（例如 `"reasoning"`、`"fast"`、`"code"`、`"summarize"`） |
| `provider` | _必要_ | 路由的目標供應商（必須匹配已知供應商名稱） |
| `model` | _必要_ | 搭配該供應商使用的模型 |
| `max_tokens` | 未設定 | 可選的每路由輸出 Token 上限，轉發給供應商 API |
| `api_key` | 未設定 | 可選的此路由供應商 API 金鑰覆寫 |
| `transport` | 未設定 | 可選的每路由傳輸覆寫（`auto`、`websocket`、`sse`） |

### `[[embedding_routes]]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `hint` | _必要_ | 路由提示名稱（例如 `"semantic"`、`"archive"`、`"faq"`） |
| `provider` | _必要_ | 嵌入供應商（`"none"`、`"openai"` 或 `"custom:<url>"`） |
| `model` | _必要_ | 搭配該供應商使用的嵌入模型 |
| `dimensions` | 未設定 | 可選的此路由嵌入維度覆寫 |
| `api_key` | 未設定 | 可選的此路由供應商 API 金鑰覆寫 |

```toml
[memory]
embedding_model = "hint:semantic"

[[model_routes]]
hint = "reasoning"
provider = "openrouter"
model = "provider/model-id"
max_tokens = 8192

[[embedding_routes]]
hint = "semantic"
provider = "openai"
model = "text-embedding-3-small"
dimensions = 1536
```

升級策略：

1. 保持提示穩定（`hint:reasoning`、`hint:semantic`）。
2. 僅在路由項目中更新 `model = "...new-version..."`。
3. 在重新啟動 / 佈署前以 `zeroclaw doctor` 驗證。

自然語言設定路徑：

- 在正常的代理程式聊天中，要求助理以口語重新連接路由。
- 執行階段可透過工具 `model_routing_config`（預設值、情境和委派子代理程式）持久化這些更新，無需手動編輯 TOML。

範例請求：

- `Set conversation to provider kimi, model moonshot-v1-8k.`
- `Set coding to provider openai, model gpt-5.3-codex, and auto-route when message contains code blocks.`
- `Create a coder sub-agent using openai/gpt-5.3-codex with tools file_read,file_write,shell.`

## `[query_classification]`

自動模型提示路由 -- 根據內容模式將使用者訊息對應到 `[[model_routes]]` 提示。

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `false` | 啟用自動查詢分類 |
| `rules` | `[]` | 分類規則（依優先級順序評估） |

`rules` 中每條規則：

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `hint` | _必要_ | 必須匹配 `[[model_routes]]` 的 hint 值 |
| `keywords` | `[]` | 不區分大小寫的子字串匹配 |
| `patterns` | `[]` | 區分大小寫的字面匹配（適用於程式碼柵欄、關鍵字如 `"fn "`） |
| `min_length` | 未設定 | 僅在訊息長度 >= N 字元時匹配 |
| `max_length` | 未設定 | 僅在訊息長度 <= N 字元時匹配 |
| `priority` | `0` | 優先級較高的規則先被檢查 |

```toml
[query_classification]
enabled = true

[[query_classification.rules]]
hint = "reasoning"
keywords = ["explain", "analyze", "why"]
min_length = 200
priority = 10

[[query_classification.rules]]
hint = "fast"
keywords = ["hi", "hello", "thanks"]
max_length = 50
priority = 5
```

## `[channels_config]`

頂層頻道選項設定在 `channels_config` 下。

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `message_timeout_secs` | `300` | 頻道訊息處理的基礎逾時（秒）；執行階段會隨工具迴圈深度調整此值（最多 4 倍） |

範例：

- `[channels_config.telegram]`
- `[channels_config.discord]`
- `[channels_config.whatsapp]`
- `[channels_config.linq]`
- `[channels_config.nextcloud_talk]`
- `[channels_config.email]`
- `[channels_config.nostr]`

備註：

- 預設 `300s` 針對裝置端 LLM（Ollama）最佳化，其速度比雲端 API 慢。
- 執行階段逾時預算為 `message_timeout_secs * scale`，其中 `scale = min(max_tool_iterations, 4)` 且最小值為 `1`。
- 此縮放可避免在首次 LLM 回合較慢 / 重試時出現錯誤逾時，但後續工具迴圈回合仍需完成。
- 若使用雲端 API（OpenAI、Anthropic 等），可將此值降低至 `60` 或更低。
- 低於 `30` 的值會被限制為 `30`，以避免即時逾時抖動。
- 逾時發生時，使用者會收到：`⚠️ Request timed out while waiting for the model. Please try again.`
- Telegram 專用的中斷行為由 `channels_config.telegram.interrupt_on_new_message` 控制（預設 `false`）。
  啟用時，同一發送者在同一聊天中的較新訊息會取消進行中的請求，並保留被中斷的使用者脈絡。
- Telegram/Discord/Slack/Mattermost/Lark/Feishu 支援 `[channels_config.<channel>.group_reply]`：
  - `mode = "all_messages"` 或 `mode = "mention_only"`
  - `allowed_sender_ids = ["..."]` 可在群組中繞過提及閘門
  - `allowed_users` 允許清單檢查仍優先執行
- 舊式的 `mention_only` 旗標（Telegram/Discord/Mattermost/Lark）仍作為備援支援。
  若已設定 `group_reply.mode`，它優先於舊式 `mention_only`。
- 當 `zeroclaw channel start` 執行中時，`default_provider`、`default_model`、`default_temperature`、`api_key`、`api_url` 和 `reliability.*` 的更新會在下一則入站訊息時從 `config.toml` 即時套用。

### `[channels_config.nostr]`

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `private_key` | _必要_ | Nostr 私密金鑰（十六進位或 `nsec1…` bech32）；當 `secrets.encrypt = true` 時加密儲存 |
| `relays` | 見備註 | Relay WebSocket URL 清單；預設為 `relay.damus.io`、`nos.lol`、`relay.primal.net`、`relay.snort.social` |
| `allowed_pubkeys` | `[]`（拒絕全部） | 發送者允許清單（十六進位或 `npub1…`）；使用 `"*"` 允許所有發送者 |

備註：

- 同時支援 NIP-04（舊式加密 DM）和 NIP-17（禮物包裝私人訊息）。回覆會自動鏡射發送者的協定。
- `private_key` 為高價值密鑰；在正式環境中請保持 `secrets.encrypt = true`（預設值）。

詳細的頻道矩陣和允許清單行為請參見 [channels-reference.md](channels-reference.md)。

### `[channels_config.whatsapp]`

WhatsApp 在單一設定表下支援兩種後端。

Cloud API 模式（Meta Webhook）：

| 鍵 | 必要 | 用途 |
|---|---|---|
| `access_token` | 是 | Meta Cloud API Bearer Token |
| `phone_number_id` | 是 | Meta 電話號碼 ID |
| `verify_token` | 是 | Webhook 驗證 Token |
| `app_secret` | 可選 | 啟用 Webhook 簽章驗證（`X-Hub-Signature-256`） |
| `allowed_numbers` | 建議 | 允許的入站號碼（`[]` = 拒絕全部，`"*"` = 允許全部） |

WhatsApp Web 模式（原生客戶端）：

| 鍵 | 必要 | 用途 |
|---|---|---|
| `session_path` | 是 | 持久化 SQLite 工作階段路徑 |
| `pair_phone` | 可選 | 配對碼流程電話號碼（僅數字） |
| `pair_code` | 可選 | 自訂配對碼（否則自動產生） |
| `allowed_numbers` | 建議 | 允許的入站號碼（`[]` = 拒絕全部，`"*"` = 允許全部） |

備註：

- WhatsApp Web 需要建置旗標 `whatsapp-web`。
- 若同時存在 Cloud 和 Web 欄位，Cloud 模式優先（向後相容性）。

### `[channels_config.linq]`

Linq Partner V3 API 整合，適用於 iMessage、RCS 和 SMS。

| 鍵 | 必要 | 用途 |
|---|---|---|
| `api_token` | 是 | Linq Partner API Bearer Token |
| `from_phone` | 是 | 發送來源電話號碼（E.164 格式） |
| `signing_secret` | 可選 | Webhook 簽章密鑰，用於 HMAC-SHA256 簽章驗證 |
| `allowed_senders` | 建議 | 允許的入站電話號碼（`[]` = 拒絕全部，`"*"` = 允許全部） |

備註：

- Webhook 端點為 `POST /linq`。
- `ZEROCLAW_LINQ_SIGNING_SECRET` 設定後會覆寫 `signing_secret`。
- 簽章使用 `X-Webhook-Signature` 和 `X-Webhook-Timestamp` 標頭；過時的時間戳（>300s）會被拒絕。
- 完整設定範例請參見 [channels-reference.md](channels-reference.md)。

### `[channels_config.nextcloud_talk]`

原生 Nextcloud Talk 機器人整合（Webhook 接收 + OCS 發送 API）。

| 鍵 | 必要 | 用途 |
|---|---|---|
| `base_url` | 是 | Nextcloud 基礎 URL（例如 `https://cloud.example.com`） |
| `app_token` | 是 | 用於 OCS Bearer 認證的機器人應用程式 Token |
| `webhook_secret` | 可選 | 啟用 Webhook 簽章驗證 |
| `allowed_users` | 建議 | 允許的 Nextcloud 行為者 ID（`[]` = 拒絕全部，`"*"` = 允許全部） |

備註：

- Webhook 端點為 `POST /nextcloud-talk`。
- `ZEROCLAW_NEXTCLOUD_TALK_WEBHOOK_SECRET` 設定後會覆寫 `webhook_secret`。
- 設定與疑難排解請參見 [nextcloud-talk-setup.md](nextcloud-talk-setup.md)。

## `[hardware]`

硬體精靈設定，用於實體世界存取（STM32、探針、串列埠）。

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `false` | 是否啟用硬體存取 |
| `transport` | `none` | 傳輸模式：`"none"`、`"native"`、`"serial"` 或 `"probe"` |
| `serial_port` | 未設定 | 串列埠路徑（例如 `"/dev/ttyACM0"`） |
| `baud_rate` | `115200` | 串列鮑率 |
| `probe_target` | 未設定 | 探針目標晶片（例如 `"STM32F401RE"`） |
| `workspace_datasheets` | `false` | 啟用工作區資料手冊 RAG（索引 PDF 線路圖以供 AI 腳位查詢） |

備註：

- 使用 `transport = "serial"` 搭配 `serial_port` 進行 USB 串列連線。
- 使用 `transport = "probe"` 搭配 `probe_target` 進行除錯探針刷寫（例如 ST-Link）。
- 協定詳情請參見 [hardware-peripherals-design.md](hardware-peripherals-design.md)。

## `[peripherals]`

更高層級的週邊板設定。板子啟用後會成為代理程式工具。

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `false` | 啟用週邊支援（板子成為代理程式工具） |
| `boards` | `[]` | 板子設定 |
| `datasheet_dir` | 未設定 | 資料手冊文件路徑（相對於工作區），用於 RAG 檢索 |

`boards` 中每個項目：

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `board` | _必要_ | 板子類型：`"nucleo-f401re"`、`"rpi-gpio"`、`"esp32"` 等 |
| `transport` | `serial` | 傳輸：`"serial"`、`"native"`、`"websocket"` |
| `path` | 未設定 | 串列路徑：`"/dev/ttyACM0"`、`"/dev/ttyUSB0"` |
| `baud` | `115200` | 串列鮑率 |

```toml
[peripherals]
enabled = true
datasheet_dir = "docs/datasheets"

[[peripherals.boards]]
board = "nucleo-f401re"
transport = "serial"
path = "/dev/ttyACM0"
baud = 115200

[[peripherals.boards]]
board = "rpi-gpio"
transport = "native"
```

備註：

- 將以板子命名的 `.md`/`.txt` 資料手冊檔案（例如 `nucleo-f401re.md`、`rpi-gpio.md`）放置在 `datasheet_dir` 中以供 RAG 檢索。
- 板子協定與韌體注意事項請參見 [hardware-peripherals-design.md](hardware-peripherals-design.md)。

## `[agents_ipc]`

同一主機上獨立 ZeroClaw 代理程式的行程間通訊。

| 鍵 | 預設值 | 用途 |
|---|---|---|
| `enabled` | `false` | 啟用 IPC 工具（`agents_list`、`agents_send`、`agents_inbox`、`state_get`、`state_set`） |
| `db_path` | `~/.zeroclaw/agents.db` | 共用 SQLite 資料庫路徑（同一主機上所有代理程式共用一個檔案） |
| `staleness_secs` | `300` | 在此視窗內未出現的代理程式被視為離線（秒） |

備註：

- 當 `enabled = false`（預設）時，不會註冊 IPC 工具，也不會建立資料庫。
- 共用同一 `db_path` 的所有代理程式可以互相探索並交換訊息。
- 代理程式身分衍生自 `workspace_dir`（SHA-256 雜湊），而非使用者提供。

範例：

```toml
[agents_ipc]
enabled = true
db_path = "~/.zeroclaw/agents.db"
staleness_secs = 300
```

## 安全相關預設值

- 頻道允許清單預設為拒絕（`[]` 表示拒絕全部）
- 閘道預設要求配對
- 公開繫結預設停用

## 驗證指令

編輯設定後：

```bash
zeroclaw status
zeroclaw doctor
zeroclaw channel doctor
zeroclaw service restart
```

## 相關文件

- [channels-reference.md](channels-reference.md)
- [providers-reference.md](providers-reference.md)
- [operations-runbook.md](operations-runbook.md)
- [troubleshooting.md](troubleshooting.md)
