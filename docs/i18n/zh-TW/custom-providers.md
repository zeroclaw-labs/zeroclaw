# 自訂 Provider 設定（繁體中文）

ZeroClaw 支援自訂 API 端點，適用於 OpenAI 相容及 Anthropic 相容的 provider。

## Provider 類型

### OpenAI 相容端點（`custom:`）

適用於實作 OpenAI API 格式的服務：

```toml
default_provider = "custom:https://your-api.com"
api_key = "your-api-key"
default_model = "your-model-name"
```

選用的 API 模式：

```toml
# 預設（優先使用 chat-completions，可用時回退至 responses）
provider_api = "openai-chat-completions"

# Responses 優先模式（直接呼叫 /responses）
provider_api = "openai-responses"
```

`provider_api` 僅在 `default_provider` 使用 `custom:<url>` 時有效。

OpenAI 相容端點支援 Responses API WebSocket 模式：

- 自動模式：當您的 `custom:` 端點解析至 `api.openai.com` 時，ZeroClaw 會先嘗試 WebSocket 模式（`wss://.../responses`），若 websocket 握手或串流失敗則自動回退至 HTTP。
- 手動覆蓋：
  - `ZEROCLAW_RESPONSES_WEBSOCKET=1` 強制任何 `custom:` 端點優先使用 websocket 模式。
  - `ZEROCLAW_RESPONSES_WEBSOCKET=0` 停用 websocket 模式，僅使用 HTTP。

### Anthropic 相容端點（`anthropic-custom:`）

適用於實作 Anthropic API 格式的服務：

```toml
default_provider = "anthropic-custom:https://your-api.com"
api_key = "your-api-key"
default_model = "your-model-name"
```

## 設定方式

### 設定檔

編輯 `~/.zeroclaw/config.toml`：

```toml
api_key = "your-api-key"
default_provider = "anthropic-custom:https://api.example.com"
default_model = "claude-sonnet-4-6"
```

### 環境變數

`custom:` 及 `anthropic-custom:` provider 使用通用的 API key 環境變數：

```bash
export API_KEY="your-api-key"
# 或：export ZEROCLAW_API_KEY="your-api-key"
zeroclaw agent
```

## Hunyuan（騰訊）

ZeroClaw 內建 [騰訊混元](https://hunyuan.tencent.com/) 的一級 provider 支援：

- Provider ID：`hunyuan`（別名：`tencent`）
- 基礎 API URL：`https://api.hunyuan.cloud.tencent.com/v1`

設定 ZeroClaw：

```toml
default_provider = "hunyuan"
default_model = "hunyuan-t1-latest"
default_temperature = 0.7
```

設定 API key：

```bash
export HUNYUAN_API_KEY="your-api-key"
zeroclaw agent -m "hello"
```

## llama.cpp Server（建議的本機設定）

ZeroClaw 內建 `llama-server` 的一級本機 provider 支援：

- Provider ID：`llamacpp`（別名：`llama.cpp`）
- 預設端點：`http://localhost:8080/v1`
- 除非 `llama-server` 以 `--api-key` 啟動，否則 API key 為選填

啟動本機伺服器（範例）：

```bash
llama-server -hf ggml-org/gpt-oss-20b-GGUF --jinja -c 133000 --host 127.0.0.1 --port 8033
```

然後設定 ZeroClaw：

```toml
default_provider = "llamacpp"
api_url = "http://127.0.0.1:8033/v1"
default_model = "ggml-org/gpt-oss-20b-GGUF"
default_temperature = 0.7
```

快速驗證：

```bash
zeroclaw models refresh --provider llamacpp
zeroclaw agent -m "hello"
```

此流程不需要匯出 `ZEROCLAW_API_KEY=dummy`。

## SGLang Server

ZeroClaw 內建 [SGLang](https://github.com/sgl-project/sglang) 的一級本機 provider 支援：

- Provider ID：`sglang`
- 預設端點：`http://localhost:30000/v1`
- 除非伺服器要求驗證，否則 API key 為選填

啟動本機伺服器（範例）：

```bash
python -m sglang.launch_server --model meta-llama/Llama-3.1-8B-Instruct --port 30000
```

然後設定 ZeroClaw：

```toml
default_provider = "sglang"
default_model = "meta-llama/Llama-3.1-8B-Instruct"
default_temperature = 0.7
```

快速驗證：

```bash
zeroclaw models refresh --provider sglang
zeroclaw agent -m "hello"
```

此流程不需要匯出 `ZEROCLAW_API_KEY=dummy`。

## vLLM Server

ZeroClaw 內建 [vLLM](https://docs.vllm.ai/) 的一級本機 provider 支援：

- Provider ID：`vllm`
- 預設端點：`http://localhost:8000/v1`
- 除非伺服器要求驗證，否則 API key 為選填

啟動本機伺服器（範例）：

```bash
vllm serve meta-llama/Llama-3.1-8B-Instruct
```

然後設定 ZeroClaw：

```toml
default_provider = "vllm"
default_model = "meta-llama/Llama-3.1-8B-Instruct"
default_temperature = 0.7
```

快速驗證：

```bash
zeroclaw models refresh --provider vllm
zeroclaw agent -m "hello"
```

此流程不需要匯出 `ZEROCLAW_API_KEY=dummy`。

## 測試設定

驗證您的自訂端點：

```bash
# 互動模式
zeroclaw agent

# 單一訊息測試
zeroclaw agent -m "test message"
```

## 疑難排解

### 驗證錯誤

- 確認 API key 是否正確
- 檢查端點 URL 格式（必須包含 `http://` 或 `https://`）
- 確保端點從您的網路可存取

### 找不到模型

- 確認模型名稱是否與 provider 可用的模型相符
- 查閱 provider 文件以取得正確的模型識別碼
- 確保端點與模型家族相符。部分自訂閘道僅公開部分模型。
- 使用您設定的端點與金鑰驗證可用模型：

```bash
curl -sS https://your-api.com/models \
  -H "Authorization: Bearer $API_KEY"
```

- 若閘道未實作 `/models`，可傳送最簡化的 chat 請求，並檢查 provider 回傳的模型錯誤訊息。

### 連線問題

- 測試端點可達性：`curl -I https://your-api.com`
- 確認防火牆/代理伺服器設定
- 查看 provider 狀態頁面

## 範例

### 本機 LLM 伺服器（通用自訂端點）

```toml
default_provider = "custom:http://localhost:8080/v1"
api_key = "your-api-key-if-required"
default_model = "local-model"
```

### 企業代理伺服器

```toml
default_provider = "anthropic-custom:https://llm-proxy.corp.example.com"
api_key = "internal-token"
```

### 雲端 Provider 閘道

```toml
default_provider = "custom:https://gateway.cloud-provider.com/v1"
api_key = "gateway-api-key"
default_model = "gpt-4"
```
