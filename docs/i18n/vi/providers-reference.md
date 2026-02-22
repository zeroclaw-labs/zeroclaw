# Tài liệu tham khảo Providers — ZeroClaw

Tài liệu này liệt kê các provider ID, alias và biến môi trường chứa thông tin xác thực.

Cập nhật lần cuối: **2026-02-19**.

## Cách liệt kê các Provider

```bash
zeroclaw providers
```

## Thứ tự ưu tiên khi giải quyết thông tin xác thực

Thứ tự ưu tiên tại runtime:

1. Thông tin xác thực tường minh từ config/CLI
2. Biến môi trường dành riêng cho provider
3. Biến môi trường dự phòng chung: `ZEROCLAW_API_KEY`, sau đó là `API_KEY`

Với chuỗi provider dự phòng (`reliability.fallback_providers`), mỗi provider dự phòng tự giải quyết thông tin xác thực của mình độc lập. Key xác thực của provider chính không tự động dùng cho provider dự phòng.

## Danh mục Provider

| Canonical ID | Alias | Cục bộ | Biến môi trường dành riêng |
|---|---|---:|---|
| `openrouter` | — | Không | `OPENROUTER_API_KEY` |
| `anthropic` | — | Không | `ANTHROPIC_OAUTH_TOKEN`, `ANTHROPIC_API_KEY` |
| `openai` | — | Không | `OPENAI_API_KEY` |
| `ollama` | — | Có | `OLLAMA_API_KEY` (tùy chọn) |
| `gemini` | `google`, `google-gemini` | Không | `GEMINI_API_KEY`, `GOOGLE_API_KEY` |
| `venice` | — | Không | `VENICE_API_KEY` |
| `vercel` | `vercel-ai` | Không | `VERCEL_API_KEY` |
| `cloudflare` | `cloudflare-ai` | Không | `CLOUDFLARE_API_KEY` |
| `moonshot` | `kimi` | Không | `MOONSHOT_API_KEY` |
| `kimi-code` | `kimi_coding`, `kimi_for_coding` | Không | `KIMI_CODE_API_KEY`, `MOONSHOT_API_KEY` |
| `synthetic` | — | Không | `SYNTHETIC_API_KEY` |
| `opencode` | `opencode-zen` | Không | `OPENCODE_API_KEY` |
| `zai` | `z.ai` | Không | `ZAI_API_KEY` |
| `glm` | `zhipu` | Không | `GLM_API_KEY` |
| `minimax` | `minimax-intl`, `minimax-io`, `minimax-global`, `minimax-cn`, `minimaxi`, `minimax-oauth`, `minimax-oauth-cn`, `minimax-portal`, `minimax-portal-cn` | Không | `MINIMAX_OAUTH_TOKEN`, `MINIMAX_API_KEY` |
| `bedrock` | `aws-bedrock` | Không | `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` (tùy chọn: `AWS_REGION`) |
| `qianfan` | `baidu` | Không | `QIANFAN_API_KEY` |
| `qwen` | `dashscope`, `qwen-intl`, `dashscope-intl`, `qwen-us`, `dashscope-us`, `qwen-code`, `qwen-oauth`, `qwen_oauth` | Không | `QWEN_OAUTH_TOKEN`, `DASHSCOPE_API_KEY` |
| `groq` | — | Không | `GROQ_API_KEY` |
| `mistral` | — | Không | `MISTRAL_API_KEY` |
| `xai` | `grok` | Không | `XAI_API_KEY` |
| `deepseek` | — | Không | `DEEPSEEK_API_KEY` |
| `together` | `together-ai` | Không | `TOGETHER_API_KEY` |
| `fireworks` | `fireworks-ai` | Không | `FIREWORKS_API_KEY` |
| `perplexity` | — | Không | `PERPLEXITY_API_KEY` |
| `cohere` | — | Không | `COHERE_API_KEY` |
| `copilot` | `github-copilot` | Không | (dùng config/`API_KEY` fallback với GitHub token) |
| `lmstudio` | `lm-studio` | Có | (tùy chọn; mặc định là cục bộ) |
| `nvidia` | `nvidia-nim`, `build.nvidia.com` | Không | `NVIDIA_API_KEY` |

### Ghi chú về Gemini

- Provider ID: `gemini` (alias: `google`, `google-gemini`)
- Xác thực có thể dùng `GEMINI_API_KEY`, `GOOGLE_API_KEY`, hoặc Gemini CLI OAuth cache (`~/.gemini/oauth_creds.json`)
- Request bằng API key dùng endpoint `generativelanguage.googleapis.com/v1beta`
- Request OAuth qua Gemini CLI dùng endpoint `cloudcode-pa.googleapis.com/v1internal` theo chuẩn Code Assist request envelope

### Ghi chú về Ollama Vision

- Provider ID: `ollama`
- Hỗ trợ đầu vào hình ảnh qua marker nội tuyến trong tin nhắn: ``[IMAGE:<source>]``
- Sau khi chuẩn hóa multimodal, ZeroClaw gửi payload hình ảnh qua trường `messages[].images` gốc của Ollama.
- Nếu chọn provider không hỗ trợ vision, ZeroClaw trả về lỗi rõ ràng thay vì âm thầm bỏ qua hình ảnh.

### Ghi chú về Bedrock

- Provider ID: `bedrock` (alias: `aws-bedrock`)
- API: [Converse API](https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_Converse.html)
- Xác thực: AWS AKSK (không phải một API key đơn lẻ). Cần đặt biến môi trường `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY`.
- Tùy chọn: `AWS_SESSION_TOKEN` cho thông tin xác thực tạm thời/STS, `AWS_REGION` hoặc `AWS_DEFAULT_REGION` (mặc định: `us-east-1`).
- Model mặc định khi khởi tạo: `anthropic.claude-sonnet-4-5-20250929-v1:0`
- Hỗ trợ native tool calling và prompt caching (`cachePoint`).
- Hỗ trợ cross-region inference profiles (ví dụ: `us.anthropic.claude-*`).
- Model ID dùng định dạng Bedrock: `anthropic.claude-sonnet-4-6`, `anthropic.claude-opus-4-6-v1`, v.v.

### Bật/tắt tính năng Reasoning của Ollama

Bạn có thể kiểm soát hành vi reasoning/thinking của Ollama từ `config.toml`:

```toml
[runtime]
reasoning_enabled = false
```

Hành vi:

- `false`: gửi `think: false` đến các yêu cầu Ollama `/api/chat`.
- `true`: gửi `think: true`.
- Không đặt: bỏ qua `think` và giữ nguyên mặc định của Ollama/model.

### Ghi đè Vision cho Ollama

Một số model Ollama hỗ trợ vision (ví dụ `llava`, `llama3.2-vision`) trong khi các model khác thì không.
Vì ZeroClaw không thể tự động phát hiện, bạn có thể ghi đè trong `config.toml`:

```toml
default_provider = "ollama"
default_model = "llava"
model_support_vision = true
```

Hành vi:

- `true`: bật xử lý hình ảnh đính kèm trong vòng lặp agent.
- `false`: tắt vision ngay cả khi provider báo hỗ trợ.
- Không đặt: dùng mặc định của provider.

Biến môi trường: `ZEROCLAW_MODEL_SUPPORT_VISION=true`

### Ghi chú về Kimi Code

- Provider ID: `kimi-code`
- Endpoint: `https://api.kimi.com/coding/v1`
- Model mặc định khi khởi tạo: `kimi-for-coding` (thay thế: `kimi-k2.5`)
- Runtime tự động thêm `User-Agent: KimiCLI/0.77` để đảm bảo tương thích.

### Ghi chú về NVIDIA NIM

- Canonical provider ID: `nvidia`
- Alias: `nvidia-nim`, `build.nvidia.com`
- Base API URL: `https://integrate.api.nvidia.com/v1`
- Khám phá model: `zeroclaw models refresh --provider nvidia`

Các model ID khởi đầu được khuyến nghị (đã xác minh với danh mục NVIDIA API ngày 2026-02-18):

- `meta/llama-3.3-70b-instruct`
- `deepseek-ai/deepseek-v3.2`
- `nvidia/llama-3.3-nemotron-super-49b-v1.5`
- `nvidia/llama-3.1-nemotron-ultra-253b-v1`

## Endpoint Tùy chỉnh

- Endpoint tương thích OpenAI:

```toml
default_provider = "custom:https://your-api.example.com"
```

- Endpoint tương thích Anthropic:

```toml
default_provider = "anthropic-custom:https://your-api.example.com"
```

## Cấu hình MiniMax OAuth (`config.toml`)

Đặt provider MiniMax và OAuth placeholder trong config:

```toml
default_provider = "minimax-oauth"
api_key = "minimax-oauth"
```

Sau đó cung cấp một trong các thông tin xác thực sau qua biến môi trường:

- `MINIMAX_OAUTH_TOKEN` (ưu tiên, access token trực tiếp)
- `MINIMAX_API_KEY` (token tĩnh/cũ)
- `MINIMAX_OAUTH_REFRESH_TOKEN` (tự động làm mới access token khi khởi động)

Tùy chọn:

- `MINIMAX_OAUTH_REGION=global` hoặc `cn` (mặc định theo alias của provider)
- `MINIMAX_OAUTH_CLIENT_ID` để ghi đè OAuth client id mặc định

Lưu ý về tương thích channel:

- Đối với các cuộc trò chuyện channel được hỗ trợ bởi MiniMax, lịch sử runtime được chuẩn hóa để duy trì thứ tự lượt hợp lệ `user`/`assistant`.
- Hướng dẫn phân phối đặc thù của channel (ví dụ: marker đính kèm Telegram) được hợp nhất vào system prompt đầu tiên thay vì được thêm vào như một lượt `system` cuối cùng.

## Cấu hình Qwen Code OAuth (`config.toml`)

Đặt chế độ Qwen Code OAuth trong config:

```toml
default_provider = "qwen-code"
api_key = "qwen-oauth"
```

Thứ tự ưu tiên giải quyết thông tin xác thực cho `qwen-code`:

1. Giá trị `api_key` tường minh (nếu không phải placeholder `qwen-oauth`)
2. `QWEN_OAUTH_TOKEN`
3. `~/.qwen/oauth_creds.json` (tái sử dụng thông tin xác thực OAuth đã cache của Qwen Code)
4. Tùy chọn làm mới qua `QWEN_OAUTH_REFRESH_TOKEN` (hoặc refresh token đã cache)
5. Nếu không dùng OAuth placeholder, `DASHSCOPE_API_KEY` vẫn có thể được dùng làm dự phòng

Tùy chọn ghi đè endpoint:

- `QWEN_OAUTH_RESOURCE_URL` (được chuẩn hóa thành `https://.../v1` nếu cần)
- Nếu không đặt, `resource_url` từ thông tin xác thực OAuth đã cache sẽ được dùng khi có

## Định tuyến Model (`hint:<name>`)

Bạn có thể định tuyến các lời gọi model theo hint bằng cách sử dụng `[[model_routes]]`:

```toml
[[model_routes]]
hint = "reasoning"
provider = "openrouter"
model = "anthropic/claude-opus-4-20250514"

[[model_routes]]
hint = "fast"
provider = "groq"
model = "llama-3.3-70b-versatile"
```

Sau đó gọi với tên model hint (ví dụ từ tool hoặc các đường dẫn tích hợp):

```text
hint:reasoning
```

## Định tuyến Embedding (`hint:<name>`)

Bạn có thể định tuyến các lời gọi embedding theo cùng mẫu hint bằng `[[embedding_routes]]`.
Đặt `[memory].embedding_model` thành giá trị `hint:<name>` để kích hoạt định tuyến.

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

Các embedding provider được hỗ trợ:

- `none`
- `openai`
- `custom:<url>` (endpoint embeddings tương thích OpenAI)

Tùy chọn ghi đè key theo từng route:

```toml
[[embedding_routes]]
hint = "semantic"
provider = "openai"
model = "text-embedding-3-small"
api_key = "sk-route-specific"
```

## Nâng cấp Model An toàn

Sử dụng các hint ổn định và chỉ cập nhật target route khi provider ngừng hỗ trợ model ID cũ.

Quy trình được khuyến nghị:

1. Giữ nguyên các call site (`hint:reasoning`, `hint:semantic`).
2. Chỉ thay đổi model đích trong `[[model_routes]]` hoặc `[[embedding_routes]]`.
3. Chạy:
   - `zeroclaw doctor`
   - `zeroclaw status`
4. Smoke test một luồng đại diện (chat + memory retrieval) trước khi triển khai.

Cách này giảm thiểu rủi ro phá vỡ vì các tích hợp và prompt không cần thay đổi khi nâng cấp model ID.
