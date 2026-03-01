# ZeroClaw 指令參照（繁體中文）

本參照文件衍生自目前的 CLI 介面（`zeroclaw --help`）。

最後驗證日期：**2026 年 2 月 28 日**。

## 頂層指令

| 指令 | 用途 |
|---|---|
| `onboard` | 快速或互動式初始化工作區 / 設定檔 |
| `agent` | 執行互動式聊天或單則訊息模式 |
| `gateway` | 啟動 Webhook 與 WhatsApp HTTP 閘道 |
| `daemon` | 啟動受管理的常駐執行階段（閘道 + 頻道 + 選用心跳 / 排程器） |
| `service` | 管理使用者層級 OS 服務生命週期 |
| `doctor` | 執行診斷與新鮮度檢查 |
| `status` | 輸出目前設定與系統摘要 |
| `estop` | 啟用 / 恢復緊急停止層級並檢視 estop 狀態 |
| `cron` | 管理排程任務 |
| `models` | 重新整理供應商模型目錄 |
| `providers` | 列出供應商 ID、別名及使用中的供應商 |
| `channel` | 管理頻道與頻道健康檢查 |
| `integrations` | 檢視整合詳情 |
| `skills` | 列出 / 安裝 / 移除技能 |
| `migrate` | 從外部執行階段匯入（目前支援 OpenClaw） |
| `config` | 匯出機器可讀的設定結構描述 |
| `completions` | 產生 Shell 自動補全腳本至 stdout |
| `hardware` | 探索與內省 USB 硬體 |
| `peripheral` | 設定與刷寫週邊裝置 |

## 指令群組

### `onboard`

- `zeroclaw onboard`
- `zeroclaw onboard --interactive`
- `zeroclaw onboard --channels-only`
- `zeroclaw onboard --force`
- `zeroclaw onboard --api-key <KEY> --provider <ID> --memory <sqlite|lucid|markdown|none>`
- `zeroclaw onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none>`
- `zeroclaw onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none> --force`

`onboard` 安全行為：

- 若 `config.toml` 已存在且以 `--interactive` 執行，上線流程會提供兩種模式：
  - 完整上線（覆寫 `config.toml`）
  - 僅更新供應商（更新 provider / model / API key，同時保留既有的頻道、隧道、記憶體、Hook 及其他設定）
- 在非互動式環境下，若 `config.toml` 已存在，會安全拒絕執行，除非傳入 `--force`。
- 僅需輪替頻道 Token / 允許清單時，請使用 `zeroclaw onboard --channels-only`。

### `agent`

- `zeroclaw agent`
- `zeroclaw agent -m "Hello"`
- `zeroclaw agent --provider <ID> --model <MODEL> --temperature <0.0-2.0>`
- `zeroclaw agent --peripheral <board:path>`

提示：

- 在互動聊天中，可以用自然語言要求路由變更（例如「對話使用 kimi、寫程式使用 gpt-5.3-codex」）；助理可透過工具 `model_routing_config` 將此設定持久化。
- 在互動聊天中，你也可以要求：
  - 切換網頁搜尋供應商 / 備援鏈（`web_search_config`）
  - 檢視或更新網域存取政策（`web_access_config`）

### `gateway` / `daemon`

- `zeroclaw gateway [--host <HOST>] [--port <PORT>] [--new-pairing]`
- `zeroclaw daemon [--host <HOST>] [--port <PORT>]`

`--new-pairing` 會清除所有已儲存的配對 Token，並在閘道啟動時強制產生新的配對碼。

### `estop`

- `zeroclaw estop`（啟用 `kill-all`）
- `zeroclaw estop --level network-kill`
- `zeroclaw estop --level domain-block --domain "*.chase.com" [--domain "*.paypal.com"]`
- `zeroclaw estop --level tool-freeze --tool shell [--tool browser]`
- `zeroclaw estop status`
- `zeroclaw estop resume`
- `zeroclaw estop resume --network`
- `zeroclaw estop resume --domain "*.chase.com"`
- `zeroclaw estop resume --tool shell`
- `zeroclaw estop resume --otp <123456>`

注意事項：

- `estop` 指令需要 `[security.estop].enabled = true`。
- 當 `[security.estop].require_otp_to_resume = true` 時，`resume` 需要 OTP 驗證。
- 若省略 `--otp`，OTP 提示會自動出現。

### `service`

- `zeroclaw service install`
- `zeroclaw service start`
- `zeroclaw service stop`
- `zeroclaw service restart`
- `zeroclaw service status`
- `zeroclaw service uninstall`

### `cron`

- `zeroclaw cron list`
- `zeroclaw cron add <expr> [--tz <IANA_TZ>] <command>`
- `zeroclaw cron add-at <rfc3339_timestamp> <command>`
- `zeroclaw cron add-every <every_ms> <command>`
- `zeroclaw cron once <delay> <command>`
- `zeroclaw cron remove <id>`
- `zeroclaw cron pause <id>`
- `zeroclaw cron resume <id>`

注意事項：

- 變更排程 / cron 動作需要 `cron.enabled = true`。
- 排程建立（`create` / `add` / `once`）中的 Shell 指令載荷會在工作持久化前先經過安全指令政策驗證。

### `models`

- `zeroclaw models refresh`
- `zeroclaw models refresh --provider <ID>`
- `zeroclaw models refresh --force`

`models refresh` 目前支援即時目錄重新整理的供應商 ID：`openrouter`、`openai`、`anthropic`、`groq`、`mistral`、`deepseek`、`xai`、`together-ai`、`gemini`、`ollama`、`llamacpp`、`sglang`、`vllm`、`astrai`、`venice`、`fireworks`、`cohere`、`moonshot`、`glm`、`zai`、`qwen`、`volcengine`（`doubao`/`ark` 別名）、`siliconflow` 和 `nvidia`。

### `doctor`

- `zeroclaw doctor`
- `zeroclaw doctor models [--provider <ID>] [--use-cache]`
- `zeroclaw doctor traces [--limit <N>] [--event <TYPE>] [--contains <TEXT>]`
- `zeroclaw doctor traces --id <TRACE_ID>`

供應商連線矩陣 CI / 本地輔助指令：

- `python3 scripts/ci/provider_connectivity_matrix.py --binary target/release-fast/zeroclaw --contract .github/connectivity/probe-contract.json`

`doctor traces` 從 `observability.runtime_trace_path` 讀取執行階段工具 / 模型診斷資料。

### `channel`

- `zeroclaw channel list`
- `zeroclaw channel start`
- `zeroclaw channel doctor`
- `zeroclaw channel bind-telegram <IDENTITY>`
- `zeroclaw channel add <type> <json>`
- `zeroclaw channel remove <name>`

頻道伺服器執行中可用的即時聊天指令：

- Telegram/Discord 發送者工作階段路由：
  - `/models`
  - `/models <provider>`
  - `/model`
  - `/model <model-id>`
  - `/new`
- 受管理的工具核准（所有非 CLI 頻道）：
  - `/approve-request <tool-name>`（建立待核准請求）
  - `/approve-confirm <request-id>`（確認待處理請求；限同一發送者 + 同一聊天 / 頻道）
  - `/approve-pending`（列出目前發送者 + 聊天 / 頻道範圍的待處理請求）
  - `/approve <tool-name>`（一步直接授權 + 持久化至 `autonomy.auto_approve`，相容路徑）
  - `/unapprove <tool-name>`（撤銷 + 從 `autonomy.auto_approve` 中移除）
  - `/approvals`（顯示執行階段 + 已持久化的核准狀態）
  - 自然語言核准行為由 `[autonomy].non_cli_natural_language_approval_mode` 控制：
    - `direct`（預設）：`授权工具 shell` / `approve tool shell` 立即授權
    - `request_confirm`：自然語言核准會建立待處理請求，然後以請求 ID 確認
    - `disabled`：自然語言核准指令被忽略（僅限斜線指令）
  - 可選的逐頻道覆寫：`[autonomy].non_cli_natural_language_approval_mode_by_channel`

核准安全行為：

- 執行階段核准指令會在頻道迴圈中 **早於** LLM 推理之前被解析和執行。
- 待處理請求的作用範圍為發送者 + 聊天 / 頻道，並會自動過期。
- 確認時需要與建立請求時相同的發送者和相同的聊天 / 頻道。
- 一旦核准並持久化，該工具在重新啟動後仍維持核准狀態，直到被撤銷。
- 可選政策閘門：`[autonomy].non_cli_approval_approvers` 可限制誰能執行核准管理指令。

多頻道啟動行為：
- `zeroclaw channel start` 在單一程序中啟動所有已設定的頻道。
- 若某個頻道初始化失敗，其他頻道仍會繼續啟動。
- 若所有已設定的頻道都初始化失敗，啟動時會回傳錯誤並結束。

頻道執行階段也會監看 `config.toml` 並即時套用以下更新：
- `default_provider`
- `default_model`
- `default_temperature`
- `api_key` / `api_url`（針對預設供應商）
- `reliability.*` 供應商重試設定

`add/remove` 目前會引導你回到受管理設定 / 手動設定路徑（尚非完整的宣告式修改器）。

### `integrations`

- `zeroclaw integrations info <name>`

### `skills`

- `zeroclaw skills list`
- `zeroclaw skills audit <source_or_name>`
- `zeroclaw skills install <source>`
- `zeroclaw skills remove <name>`

`<source>` 接受的格式：

| 格式 | 範例 | 備註 |
|---|---|---|
| **ClawhHub 個人檔案 URL** | `https://clawhub.ai/steipete/summarize` | 以網域自動偵測；從 ClawhHub API 下載 zip |
| **ClawhHub 簡短前綴** | `clawhub:summarize` | 簡短格式；slug 即為 ClawhHub 上的技能名稱 |
| **直接 zip URL** | `zip:https://example.com/skill.zip` | 任何回傳 zip 壓縮檔的 HTTPS URL |
| **本地 zip 檔案** | `/path/to/skill.zip` | 已下載至本地磁碟的 zip 檔案 |
| **Registry 套件** | `namespace/name` 或 `namespace/name@version` | 從已設定的 Registry 取得（預設為 ZeroMarket） |
| **Git 遠端** | `https://github.com/…`、`git@host:owner/repo.git` | 使用 `git clone --depth 1` 複製 |
| **本地檔案系統路徑** | `./my-skill` 或 `/abs/path/skill` | 複製目錄並進行稽核 |

**ClawhHub 安裝範例：**

```bash
# 透過個人檔案 URL 安裝（slug 從最後一段路徑擷取）
zeroclaw skill install https://clawhub.ai/steipete/summarize

# 使用簡短前綴安裝
zeroclaw skill install clawhub:summarize

# 從已下載的本地 zip 安裝
zeroclaw skill install ~/Downloads/summarize-1.0.0.zip
```

若 ClawhHub API 回傳 429（速率限制）或需要認證，請在 `[skills]` 設定中設置 `clawhub_token`（參見[設定參照](config-reference.md#skills)）。

**Zip 安裝行為：**
- 若 zip 包含 `_meta.json`（OpenClaw 慣例），名稱 / 版本 / 作者會從中讀取。
- 若 zip 中沒有 `SKILL.toml` 也沒有 `SKILL.md`，會自動產生最小的 `SKILL.toml`。

Registry 套件會安裝到 `~/.zeroclaw/workspace/skills/<name>/`。

`skills install` 在接受技能之前一律會執行內建的靜態安全稽核。稽核會阻擋：
- 技能套件內的符號連結
- 腳本類檔案（`.sh`、`.bash`、`.zsh`、`.ps1`、`.bat`、`.cmd`）
- 高風險指令片段（例如管線導向 Shell 的載荷）
- 逃逸出技能根目錄、指向遠端 Markdown 或目標為腳本檔案的 Markdown 連結

> **注意：** 安全稽核適用於目錄型安裝（本地路徑、Git 遠端）。Zip 型安裝（ClawhHub、直接 zip URL、本地 zip 檔案）在解壓縮時會進行路徑穿越安全檢查，但不會執行完整的靜態稽核 -- 對於不受信任的來源，請手動審查 zip 內容。

使用 `skills audit` 可手動驗證候選技能目錄（或以名稱指定已安裝的技能），再決定是否分享。

技能清單檔（`SKILL.toml`）支援 `prompts` 和 `[[tools]]`；兩者都會在執行階段注入到代理程式的系統提示中，使模型無需手動讀取技能檔案即可遵循技能指示。

### `migrate`

- `zeroclaw migrate openclaw [--source <path>] [--dry-run]`

### `config`

- `zeroclaw config schema`

`config schema` 會將完整 `config.toml` 合約的 JSON Schema（draft 2020-12）輸出到 stdout。

### `completions`

- `zeroclaw completions bash`
- `zeroclaw completions fish`
- `zeroclaw completions zsh`
- `zeroclaw completions powershell`
- `zeroclaw completions elvish`

`completions` 設計為僅輸出至 stdout，以便腳本可直接 source 而不受日誌 / 警告訊息干擾。

### `hardware`

- `zeroclaw hardware discover`
- `zeroclaw hardware introspect <path>`
- `zeroclaw hardware info [--chip <chip_name>]`

### `peripheral`

- `zeroclaw peripheral list`
- `zeroclaw peripheral add <board> <path>`
- `zeroclaw peripheral flash [--port <serial_port>]`
- `zeroclaw peripheral setup-uno-q [--host <ip_or_host>]`
- `zeroclaw peripheral flash-nucleo`

## 驗證提示

快速比對文件與目前執行檔的方式：

```bash
zeroclaw --help
zeroclaw <command> --help
```
