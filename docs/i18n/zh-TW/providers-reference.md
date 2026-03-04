# ZeroClaw Provider 參照手冊（繁體中文）

本文件列出所有 provider ID、別名，以及對應的認證環境變數。

最後驗證日期：**2026 年 2 月 28 日**。

## 如何列出所有 Provider

```bash
zeroclaw providers
```

## 憑證解析順序

執行階段的憑證解析順序如下：

1. 設定檔或 CLI 中明確指定的憑證
2. Provider 專屬環境變數
3. 通用備援環境變數：先找 `ZEROCLAW_API_KEY`，再找 `API_KEY`

對於彈性備援鏈（`reliability.fallback_providers`），每個備援 provider 會獨立解析各自的憑證。主要 provider 的明確憑證不會自動套用到備援 provider。

## Provider 目錄

| 正式 ID | 別名 | 本機 | Provider 專屬環境變數 |
|---|---|---:|---|
| `openrouter` | — | 否 | `OPENROUTER_API_KEY` |
| `anthropic` | — | 否 | `ANTHROPIC_OAUTH_TOKEN`, `ANTHROPIC_API_KEY` |
| `openai` | — | 否 | `OPENAI_API_KEY` |
| `ollama` | — | 是 | `OLLAMA_API_KEY`（選填） |
| `gemini` | `google`, `google-gemini` | 否 | `GEMINI_API_KEY`, `GOOGLE_API_KEY` |
| `venice` | — | 否 | `VENICE_API_KEY` |
| `vercel` | `vercel-ai` | 否 | `VERCEL_API_KEY` |
| `cloudflare` | `cloudflare-ai` | 否 | `CLOUDFLARE_API_KEY` |
| `moonshot` | `kimi` | 否 | `MOONSHOT_API_KEY` |
| `kimi-code` | `kimi_coding`, `kimi_for_coding` | 否 | `KIMI_CODE_API_KEY`, `MOONSHOT_API_KEY` |
| `synthetic` | — | 否 | `SYNTHETIC_API_KEY` |
| `opencode` | `opencode-zen` | 否 | `OPENCODE_API_KEY` |
| `zai` | `z.ai` | 否 | `ZAI_API_KEY` |
| `glm` | `zhipu` | 否 | `GLM_API_KEY` |
| `minimax` | `minimax-intl`, `minimax-io`, `minimax-global`, `minimax-cn`, `minimaxi`, `minimax-oauth`, `minimax-oauth-cn`, `minimax-portal`, `minimax-portal-cn` | 否 | `MINIMAX_OAUTH_TOKEN`, `MINIMAX_API_KEY` |
| `bedrock` | `aws-bedrock` | 否 | `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY`（選填：`AWS_REGION`） |
| `qianfan` | `baidu` | 否 | `QIANFAN_API_KEY` |
| `doubao` | `volcengine`, `ark`, `doubao-cn` | 否 | `ARK_API_KEY`, `DOUBAO_API_KEY` |
| `siliconflow` | `silicon-cloud`, `siliconcloud` | 否 | `SILICONFLOW_API_KEY` |
| `hunyuan` | `tencent` | 否 | `HUNYUAN_API_KEY` |
| `qwen` | `dashscope`, `qwen-intl`, `dashscope-intl`, `qwen-us`, `dashscope-us`, `qwen-code`, `qwen-oauth`, `qwen_oauth` | 否 | `QWEN_OAUTH_TOKEN`, `DASHSCOPE_API_KEY` |
| `groq` | — | 否 | `GROQ_API_KEY` |
| `mistral` | — | 否 | `MISTRAL_API_KEY` |
| `xai` | `grok` | 否 | `XAI_API_KEY` |
| `deepseek` | — | 否 | `DEEPSEEK_API_KEY` |
| `together` | `together-ai` | 否 | `TOGETHER_API_KEY` |
| `fireworks` | `fireworks-ai` | 否 | `FIREWORKS_API_KEY` |
| `novita` | — | 否 | `NOVITA_API_KEY` |
| `perplexity` | — | 否 | `PERPLEXITY_API_KEY` |
| `cohere` | — | 否 | `COHERE_API_KEY` |
| `copilot` | `github-copilot` | 否 | （使用設定檔 / `API_KEY` 備援搭配 GitHub 權杖） |
| `lmstudio` | `lm-studio` | 是 | （選填；預設為本機模式） |
| `llamacpp` | `llama.cpp` | 是 | `LLAMACPP_API_KEY`（選填；僅在伺服器啟用驗證時需要） |
| `sglang` | — | 是 | `SGLANG_API_KEY`（選填） |
| `vllm` | — | 是 | `VLLM_API_KEY`（選填） |
| `osaurus` | — | 是 | `OSAURUS_API_KEY`（選填；預設為 `"osaurus"`） |
| `nvidia` | `nvidia-nim`, `build.nvidia.com` | 否 | `NVIDIA_API_KEY` |

### Vercel AI Gateway 備註

- Provider ID：`vercel`（別名：`vercel-ai`）
- 基底 API URL：`https://ai-gateway.vercel.sh/v1`
- 認證方式：`VERCEL_API_KEY`
- 使用 Vercel AI Gateway 不需要部署專案。
- 如果出現 `DEPLOYMENT_NOT_FOUND`，請確認 provider 指向的是上述 gateway 端點，而非 `https://api.vercel.ai`。

### Gemini 備註

- Provider ID：`gemini`（別名：`google`, `google-gemini`）
- 認證來源可為 `GEMINI_API_KEY`、`GOOGLE_API_KEY`，或 Gemini CLI OAuth 快取（`~/.gemini/oauth_creds.json`）
- API 金鑰請求使用 `generativelanguage.googleapis.com/v1beta`
- Gemini CLI OAuth 請求使用 `cloudcode-pa.googleapis.com/v1internal`，採用 Code Assist 請求封裝語意
- 支援思考模型（例如 `gemini-3-pro-preview`）— 內部推理部分會自動從回應中過濾

### Qwen（阿里雲）備註

- Provider ID：`qwen`、`qwen-code`（OAuth）、`qwen-oauth`、`dashscope`、`qwen-intl`、`qwen-us`
- **OAuth 免費方案**：使用 `qwen-code` 或在設定中指定 `api_key = "qwen-oauth"`
  - 端點：`portal.qwen.ai/v1`
  - 憑證：`~/.qwen/oauth_creds.json`（使用 `qwen login` 進行驗證）
  - 每日配額：1000 次請求
  - 可用模型：`qwen3-coder-plus`（於 2026-02-24 驗證）
  - 上下文視窗：約 32K 個 token
- **API 金鑰存取**：使用 `qwen` 或 `dashscope` provider 搭配 `DASHSCOPE_API_KEY`
  - 端點：`dashscope.aliyuncs.com/compatible-mode/v1`
  - 付費 API 金鑰可取得更高配額與更多模型
- **認證方式**：`QWEN_OAUTH_TOKEN`（OAuth 用）或 `DASHSCOPE_API_KEY`（API 金鑰用）
- **建議模型**：`qwen3-coder-plus` — 針對程式碼任務最佳化
- **配額追蹤**：`zeroclaw providers-quota --provider qwen-code` 顯示靜態配額資訊（`?/1000` — 剩餘未知，每日上限 1000）
  - Qwen OAuth API 不會回傳速率限制標頭
  - 實際請求計數需要本機計數器（尚未實作）
  - 可偵測並解析速率限制錯誤以進行重試退避
- **限制**：
  - OAuth 免費方案僅限 1 個模型與每日 1000 次請求
  - 詳見測試報告：`docs/qwen-provider-test-report.md`

### 火山引擎 ARK（豆包）備註

- 執行階段 provider ID：`doubao`（別名：`volcengine`, `ark`, `doubao-cn`）
- 上線精靈顯示/正式名稱：`volcengine`
- 基底 API URL：`https://ark.cn-beijing.volces.com/api/v3`
- 聊天端點：`/chat/completions`
- 模型探索端點：`/models`
- 認證方式：`ARK_API_KEY`（備援：`DOUBAO_API_KEY`）
- 預設模型：`doubao-1-5-pro-32k-250115`

最小設定範例：

```bash
export ARK_API_KEY="your-ark-api-key"
zeroclaw onboard --provider volcengine --api-key "$ARK_API_KEY" --model doubao-1-5-pro-32k-250115 --force
```

快速驗證：

```bash
zeroclaw models refresh --provider volcengine
zeroclaw agent --provider volcengine --model doubao-1-5-pro-32k-250115 -m "ping"
```

### SiliconFlow 備註

- Provider ID：`siliconflow`（別名：`silicon-cloud`, `siliconcloud`）
- 基底 API URL：`https://api.siliconflow.cn/v1`
- 聊天端點：`/chat/completions`
- 模型探索端點：`/models`
- 認證方式：`SILICONFLOW_API_KEY`
- 預設模型：`Pro/zai-org/GLM-4.7`

最小設定範例：

```bash
export SILICONFLOW_API_KEY="your-siliconflow-api-key"
zeroclaw onboard --provider siliconflow --api-key "$SILICONFLOW_API_KEY" --model Pro/zai-org/GLM-4.7 --force
```

快速驗證：

```bash
zeroclaw models refresh --provider siliconflow
zeroclaw agent --provider siliconflow --model Pro/zai-org/GLM-4.7 -m "ping"
```

### Ollama 視覺功能備註

- Provider ID：`ollama`
- 透過使用者訊息中的圖片標記 ``[IMAGE:<source>]`` 支援視覺輸入。
- 經過多模態正規化後，ZeroClaw 會透過 Ollama 原生的 `messages[].images` 欄位傳送圖片。
- 若選取的 provider 不支援視覺功能，ZeroClaw 會回傳結構化的能力錯誤，而非靜默忽略圖片。

### Ollama 雲端路由備註

- `:cloud` 模型後綴僅適用於遠端 Ollama 端點。
- 遠端端點應在 `api_url` 中設定（範例：`https://ollama.com`）。
- ZeroClaw 會自動正規化 `api_url` 中的尾端 `/api`。
- 若 `default_model` 以 `:cloud` 結尾但 `api_url` 為本機或未設定，設定驗證會提前失敗並提供可操作的錯誤訊息。
- 本機 Ollama 模型探索會刻意排除 `:cloud` 項目，避免在本機模式下選到僅限雲端的模型。

### 混元備註

- Provider ID：`hunyuan`（別名：`tencent`）
- 基底 API URL：`https://api.hunyuan.cloud.tencent.com/v1`
- 認證方式：`HUNYUAN_API_KEY`（從[騰訊雲控制台](https://console.cloud.tencent.com/hunyuan)取得）
- 建議模型：`hunyuan-t1-latest`（深度推理）、`hunyuan-turbo-latest`（快速）、`hunyuan-pro`（高品質）

### llama.cpp 伺服器備註

- Provider ID：`llamacpp`（別名：`llama.cpp`）
- 預設端點：`http://localhost:8080/v1`
- API 金鑰預設為選填；僅在 `llama-server` 以 `--api-key` 啟動時才需設定 `LLAMACPP_API_KEY`。
- 模型探索：`zeroclaw models refresh --provider llamacpp`

### SGLang 伺服器備註

- Provider ID：`sglang`
- 預設端點：`http://localhost:30000/v1`
- API 金鑰預設為選填；僅在伺服器要求認證時才需設定 `SGLANG_API_KEY`。
- 工具呼叫需要以 `--tool-call-parser` 啟動 SGLang（例如 `hermes`、`llama3`、`qwen25`）。
- 模型探索：`zeroclaw models refresh --provider sglang`

### vLLM 伺服器備註

- Provider ID：`vllm`
- 預設端點：`http://localhost:8000/v1`
- API 金鑰預設為選填；僅在伺服器要求認證時才需設定 `VLLM_API_KEY`。
- 模型探索：`zeroclaw models refresh --provider vllm`

### Osaurus 伺服器備註

- Provider ID：`osaurus`
- 預設端點：`http://localhost:1337/v1`
- API 金鑰預設為 `"osaurus"`，但為選填；設定 `OSAURUS_API_KEY` 可覆寫，或留空以使用免金鑰存取。
- 模型探索：`zeroclaw models refresh --provider osaurus`
- [Osaurus](https://github.com/dinoki-ai/osaurus) 是一個統一的 AI 邊緣執行環境，專為 macOS（Apple Silicon）設計，透過單一端點結合本機 MLX 推理與雲端 provider 代理。
- 同時支援多種 API 格式：OpenAI 相容（`/v1/chat/completions`）、Anthropic（`/messages`）、Ollama（`/chat`）、以及 Open Responses（`/v1/responses`）。
- 內建 MCP（Model Context Protocol）支援，可連接工具與上下文伺服器。
- 本機模型透過 MLX 執行（Llama、Qwen、Gemma、GLM、Phi、Nemotron 等）；雲端模型則透明代理。

### Bedrock 備註

- Provider ID：`bedrock`（別名：`aws-bedrock`）
- API：[Converse API](https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_Converse.html)
- 認證方式：AWS AKSK（非單一 API 金鑰）。設定 `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` 環境變數。
- 選填：`AWS_SESSION_TOKEN`（臨時 / STS 憑證用）、`AWS_REGION` 或 `AWS_DEFAULT_REGION`（預設：`us-east-1`）。
- 預設上線模型：`anthropic.claude-sonnet-4-5-20250929-v1:0`
- 支援原生工具呼叫與提示快取（`cachePoint`）。
- 支援跨區域推理設定檔（例如 `us.anthropic.claude-*`）。
- 模型 ID 採用 Bedrock 格式：`anthropic.claude-sonnet-4-6`、`anthropic.claude-opus-4-6-v1` 等。

### Ollama 推理開關

可透過 `config.toml` 控制 Ollama 的推理/思考行為：

```toml
[runtime]
reasoning_enabled = false
```

行為說明：

- `false`：向 Ollama `/api/chat` 請求傳送 `think: false`。
- `true`：傳送 `think: true`。
- 未設定：省略 `think`，維持 Ollama / 模型的預設行為。

### Ollama 視覺覆寫

部分 Ollama 模型支援視覺功能（例如 `llava`、`llama3.2-vision`），但部分不支援。由於 ZeroClaw 無法自動偵測此項能力，可在 `config.toml` 中手動覆寫：

```toml
default_provider = "ollama"
default_model = "llava"
model_support_vision = true
```

行為說明：

- `true`：在 agent 迴圈中啟用圖片附件處理。
- `false`：即使 provider 回報支援視覺功能也停用。
- 未設定：使用 provider 的內建預設值。

環境變數覆寫：`ZEROCLAW_MODEL_SUPPORT_VISION=true`

### OpenAI Codex 推理等級

可透過 `config.toml` 控制 OpenAI Codex 的推理力度：

```toml
[provider]
reasoning_level = "high"
```

行為說明：

- 支援的值：`minimal`、`low`、`medium`、`high`、`xhigh`（不分大小寫）。
- 設定後會覆寫 `ZEROCLAW_CODEX_REASONING_EFFORT`。
- 未設定時會備援至 `ZEROCLAW_CODEX_REASONING_EFFORT`（若存在），否則預設為 `xhigh`。
- 向下相容：`runtime.reasoning_level` 仍可使用但已棄用；建議改用 `provider.reasoning_level`。
- 若同時設定了 `provider.reasoning_level` 與 `runtime.reasoning_level`，以 provider 層級的值為準。

### Kimi Code 備註

- Provider ID：`kimi-code`
- 端點：`https://api.kimi.com/coding/v1`
- 預設上線模型：`kimi-for-coding`（替代方案：`kimi-k2.5`）
- 執行階段會自動加入 `User-Agent: KimiCLI/0.77` 以確保相容性。

### NVIDIA NIM 備註

- 正式 provider ID：`nvidia`
- 別名：`nvidia-nim`, `build.nvidia.com`
- 基底 API URL：`https://integrate.api.nvidia.com/v1`
- 模型探索：`zeroclaw models refresh --provider nvidia`

建議入門模型 ID（已於 2026 年 2 月 18 日對照 NVIDIA API 目錄驗證）：

- `meta/llama-3.3-70b-instruct`
- `deepseek-ai/deepseek-v3.2`
- `nvidia/llama-3.3-nemotron-super-49b-v1.5`
- `nvidia/llama-3.1-nemotron-ultra-253b-v1`

## 自訂端點

- OpenAI 相容端點：

```toml
default_provider = "custom:https://your-api.example.com"
```

- Anthropic 相容端點：

```toml
default_provider = "anthropic-custom:https://your-api.example.com"
```

## MiniMax OAuth 設定（config.toml）

在設定檔中設定 MiniMax provider 與 OAuth 佔位符：

```toml
default_provider = "minimax-oauth"
api_key = "minimax-oauth"
```

接著透過環境變數提供以下其中一組憑證：

- `MINIMAX_OAUTH_TOKEN`（建議使用，直接存取權杖）
- `MINIMAX_API_KEY`（舊版 / 靜態權杖）
- `MINIMAX_OAUTH_REFRESH_TOKEN`（啟動時自動更新存取權杖）

選填項目：

- `MINIMAX_OAUTH_REGION=global` 或 `cn`（依 provider 別名預設）
- `MINIMAX_OAUTH_CLIENT_ID` 覆寫預設的 OAuth 用戶端 ID

頻道相容性備註：

- 對於 MiniMax 驅動的頻道對話，執行階段歷史記錄會正規化，以維持有效的 `user`/`assistant` 輪次順序。
- 頻道專屬的傳送指引（例如 Telegram 附件標記）會合併至開頭的 system prompt，而非附加為尾端的 `system` 輪次。

## Qwen Code OAuth 設定（config.toml）

在設定中啟用 Qwen Code OAuth 模式：

```toml
default_provider = "qwen-code"
api_key = "qwen-oauth"
```

`qwen-code` 的憑證解析順序：

1. 明確的 `api_key` 值（若非佔位符 `qwen-oauth`）
2. `QWEN_OAUTH_TOKEN`
3. `~/.qwen/oauth_creds.json`（重複使用 Qwen Code 快取的 OAuth 憑證）
4. 透過 `QWEN_OAUTH_REFRESH_TOKEN`（或快取的 refresh token）進行選擇性更新
5. 若未使用 OAuth 佔位符，`DASHSCOPE_API_KEY` 仍可作為備援

選填端點覆寫：

- `QWEN_OAUTH_RESOURCE_URL`（如有需要會自動正規化為 `https://.../v1`）
- 若未設定，有可用的快取 OAuth 憑證時會使用其中的 `resource_url`

## 模型路由（`hint:<name>`）

可使用 `[[model_routes]]` 透過 hint 路由模型呼叫：

```toml
[[model_routes]]
hint = "reasoning"
provider = "openrouter"
model = "anthropic/claude-opus-4-20250514"
max_tokens = 8192

[[model_routes]]
hint = "fast"
provider = "groq"
model = "llama-3.3-70b-versatile"
```

接著以 hint 模型名稱呼叫（例如從工具或整合路徑）：

```text
hint:reasoning
```

## 嵌入路由（`hint:<name>`）

可使用相同的 hint 模式搭配 `[[embedding_routes]]` 路由嵌入呼叫。
將 `[memory].embedding_model` 設定為 `hint:<name>` 值即可啟用路由。

```toml
[memory]
embedding_model = "hint:semantic"

[[embedding_routes]]
hint = "semantic"
provider = "openai"
model = "text-embedding-3-small"
dimensions = 1536

[[embedding_routes]]
hint = "archive"
provider = "custom:https://embed.example.com/v1"
model = "your-embedding-model-id"
dimensions = 1024
```

支援的嵌入 provider：

- `none`
- `openai`
- `custom:<url>`（OpenAI 相容嵌入端點）

每條路由可選填金鑰覆寫：

```toml
[[embedding_routes]]
hint = "semantic"
provider = "openai"
model = "text-embedding-3-small"
api_key = "sk-route-specific"
```

## 安全升級模型

使用穩定的 hint，僅在 provider 棄用模型 ID 時更新路由目標。

建議工作流程：

1. 保持呼叫點穩定（`hint:reasoning`、`hint:semantic`）。
2. 僅修改 `[[model_routes]]` 或 `[[embedding_routes]]` 下的目標模型。
3. 執行：
   - `zeroclaw doctor`
   - `zeroclaw status`
4. 在全面上線前，對一個代表性流程進行冒煙測試（聊天 + 記憶體檢索）。

這種方式能將影響降到最低，因為整合端與 prompt 在模型 ID 升級時無需變更。
