# ZeroClaw Providers Reference

This document maps provider IDs, aliases, and credential environment variables.

Last verified: **February 21, 2026**.

## How to List Providers

```bash
zeroclaw providers
```

## Credential Resolution Order

Runtime resolution order is:

1. Explicit credential from config/CLI
2. Provider-specific env var(s)
3. Generic fallback env vars: `ZEROCLAW_API_KEY` then `API_KEY`

For resilient fallback chains (`reliability.fallback_providers`), each fallback
provider resolves credentials independently. The primary provider's explicit
credential is not reused for fallback providers.

## Provider Catalog

| Canonical ID | Aliases | Local | Provider-specific env var(s) |
|---|---|---:|---|
| `openrouter` | — | No | `OPENROUTER_API_KEY` |
| `anthropic` | — | No | `ANTHROPIC_OAUTH_TOKEN`, `ANTHROPIC_API_KEY` |
| `openai` | — | No | `OPENAI_API_KEY` |
| `ollama` | — | Yes | `OLLAMA_API_KEY` (optional) |
| `gemini` | `google`, `google-gemini` | No | `GEMINI_API_KEY`, `GOOGLE_API_KEY` |
| `venice` | — | No | `VENICE_API_KEY` |
| `vercel` | `vercel-ai` | No | `VERCEL_API_KEY` |
| `cloudflare` | `cloudflare-ai` | No | `CLOUDFLARE_API_KEY` |
| `moonshot` | `kimi` | No | `MOONSHOT_API_KEY` |
| `kimi-code` | `kimi_coding`, `kimi_for_coding` | No | `KIMI_CODE_API_KEY`, `MOONSHOT_API_KEY` |
| `synthetic` | — | No | `SYNTHETIC_API_KEY` |
| `opencode` | `opencode-zen` | No | `OPENCODE_API_KEY` |
| `zai` | `z.ai` | No | `ZAI_API_KEY` |
| `glm` | `zhipu` | No | `GLM_API_KEY` |
| `minimax` | `minimax-intl`, `minimax-io`, `minimax-global`, `minimax-cn`, `minimaxi`, `minimax-oauth`, `minimax-oauth-cn`, `minimax-portal`, `minimax-portal-cn` | No | `MINIMAX_OAUTH_TOKEN`, `MINIMAX_API_KEY` |
| `bedrock` | `aws-bedrock` | No | `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` (optional: `AWS_REGION`) |
| `qianfan` | `baidu` | No | `QIANFAN_API_KEY` |
| `doubao` | `volcengine`, `ark`, `doubao-cn` | No | `ARK_API_KEY`, `DOUBAO_API_KEY` |
| `qwen` | `dashscope`, `qwen-intl`, `dashscope-intl`, `qwen-us`, `dashscope-us`, `qwen-code`, `qwen-oauth`, `qwen_oauth` | No | `QWEN_OAUTH_TOKEN`, `DASHSCOPE_API_KEY` |
| `groq` | — | No | `GROQ_API_KEY` |
| `mistral` | — | No | `MISTRAL_API_KEY` |
| `xai` | `grok` | No | `XAI_API_KEY` |
| `deepseek` | — | No | `DEEPSEEK_API_KEY` |
| `together` | `together-ai` | No | `TOGETHER_API_KEY` |
| `fireworks` | `fireworks-ai` | No | `FIREWORKS_API_KEY` |
| `novita` | — | No | `NOVITA_API_KEY` |
| `perplexity` | — | No | `PERPLEXITY_API_KEY` |
| `cohere` | — | No | `COHERE_API_KEY` |
| `copilot` | `github-copilot` | No | (use config/`API_KEY` fallback with GitHub token) |
| `lmstudio` | `lm-studio` | Yes | (optional; local by default) |
| `llamacpp` | `llama.cpp` | Yes | `LLAMACPP_API_KEY` (optional; only if server auth is enabled) |
| `sglang` | — | Yes | `SGLANG_API_KEY` (optional) |
| `vllm` | — | Yes | `VLLM_API_KEY` (optional) |
| `osaurus` | — | Yes | `OSAURUS_API_KEY` (optional; defaults to `"osaurus"`) |
| `nvidia` | `nvidia-nim`, `build.nvidia.com` | No | `NVIDIA_API_KEY` |

### Vercel AI Gateway Notes

- Provider ID: `vercel` (alias: `vercel-ai`)
- Base API URL: `https://ai-gateway.vercel.sh/v1`
- Authentication: `VERCEL_API_KEY`
- Vercel AI Gateway usage does not require a project deployment.
- If you see `DEPLOYMENT_NOT_FOUND`, verify the provider is targeting the gateway endpoint above instead of `https://api.vercel.ai`.

### Gemini Notes

- Provider ID: `gemini` (aliases: `google`, `google-gemini`)
- Auth can come from `GEMINI_API_KEY`, `GOOGLE_API_KEY`, or Gemini CLI OAuth cache (`~/.gemini/oauth_creds.json`)
- API key requests use `generativelanguage.googleapis.com/v1beta`
- Gemini CLI OAuth requests use `cloudcode-pa.googleapis.com/v1internal` with Code Assist request envelope semantics
- Thinking models (e.g. `gemini-3-pro-preview`) are supported — internal reasoning parts are automatically filtered from the response

### Ollama Vision Notes

- Provider ID: `ollama`
- Vision input is supported through user message image markers: ``[IMAGE:<source>]``.
- After multimodal normalization, ZeroClaw sends image payloads through Ollama's native `messages[].images` field.
- If a non-vision provider is selected, ZeroClaw returns a structured capability error instead of silently ignoring images.

### Ollama Cloud Routing Notes

- Use `:cloud` model suffix only with a remote Ollama endpoint.
- Remote endpoint should be set in `api_url` (example: `https://ollama.com`).
- ZeroClaw normalizes a trailing `/api` in `api_url` automatically.
- If `default_model` ends with `:cloud` while `api_url` is local or unset, config validation fails early with an actionable error.
- Local Ollama model discovery intentionally excludes `:cloud` entries to avoid selecting cloud-only models in local mode.

### llama.cpp Server Notes

- Provider ID: `llamacpp` (alias: `llama.cpp`)
- Default endpoint: `http://localhost:8080/v1`
- API key is optional by default; set `LLAMACPP_API_KEY` only when `llama-server` is started with `--api-key`.
- Model discovery: `zeroclaw models refresh --provider llamacpp`

### SGLang Server Notes

- Provider ID: `sglang`
- Default endpoint: `http://localhost:30000/v1`
- API key is optional by default; set `SGLANG_API_KEY` only when the server requires authentication.
- Tool calling requires launching SGLang with `--tool-call-parser` (e.g. `hermes`, `llama3`, `qwen25`).
- Model discovery: `zeroclaw models refresh --provider sglang`

### vLLM Server Notes

- Provider ID: `vllm`
- Default endpoint: `http://localhost:8000/v1`
- API key is optional by default; set `VLLM_API_KEY` only when the server requires authentication.
- Model discovery: `zeroclaw models refresh --provider vllm`

### Osaurus Server Notes

- Provider ID: `osaurus`
- Default endpoint: `http://localhost:1337/v1`
- API key defaults to `"osaurus"` but is optional; set `OSAURUS_API_KEY` to override or leave unset for keyless access.
- Model discovery: `zeroclaw models refresh --provider osaurus`
- [Osaurus](https://github.com/dinoki-ai/osaurus) is a unified AI edge runtime for macOS (Apple Silicon) that combines local MLX inference with cloud provider proxying through a single endpoint.
- Supports multiple API formats simultaneously: OpenAI-compatible (`/v1/chat/completions`), Anthropic (`/messages`), Ollama (`/chat`), and Open Responses (`/v1/responses`).
- Built-in MCP (Model Context Protocol) support for tool and context server connectivity.
- Local models run via MLX (Llama, Qwen, Gemma, GLM, Phi, Nemotron, and others); cloud models are proxied transparently.

### Bedrock Notes

- Provider ID: `bedrock` (alias: `aws-bedrock`)
- API: [Converse API](https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_Converse.html)
- Authentication: AWS AKSK (not a single API key). Set `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` environment variables.
- Optional: `AWS_SESSION_TOKEN` for temporary/STS credentials, `AWS_REGION` or `AWS_DEFAULT_REGION` (default: `us-east-1`).
- Default onboarding model: `anthropic.claude-sonnet-4-5-20250929-v1:0`
- Supports native tool calling and prompt caching (`cachePoint`).
- Cross-region inference profiles supported (e.g., `us.anthropic.claude-*`).
- Model IDs use Bedrock format: `anthropic.claude-sonnet-4-6`, `anthropic.claude-opus-4-6-v1`, etc.

### Ollama Reasoning Toggle

You can control Ollama reasoning/thinking behavior from `config.toml`:

```toml
[runtime]
reasoning_enabled = false
```

Behavior:

- `false`: sends `think: false` to Ollama `/api/chat` requests.
- `true`: sends `think: true`.
- Unset: omits `think` and keeps Ollama/model defaults.

### Kimi Code Notes

- Provider ID: `kimi-code`
- Endpoint: `https://api.kimi.com/coding/v1`
- Default onboarding model: `kimi-for-coding` (alternative: `kimi-k2.5`)
- Runtime auto-adds `User-Agent: KimiCLI/0.77` for compatibility.

### NVIDIA NIM Notes

- Canonical provider ID: `nvidia`
- Aliases: `nvidia-nim`, `build.nvidia.com`
- Base API URL: `https://integrate.api.nvidia.com/v1`
- Model discovery: `zeroclaw models refresh --provider nvidia`

Recommended starter model IDs (verified against NVIDIA API catalog on February 18, 2026):

- `meta/llama-3.3-70b-instruct`
- `deepseek-ai/deepseek-v3.2`
- `nvidia/llama-3.3-nemotron-super-49b-v1.5`
- `nvidia/llama-3.1-nemotron-ultra-253b-v1`

## Custom Endpoints

- OpenAI-compatible endpoint:

```toml
default_provider = "custom:https://your-api.example.com"
```

- Anthropic-compatible endpoint:

```toml
default_provider = "anthropic-custom:https://your-api.example.com"
```

## MiniMax OAuth Setup (config.toml)

Set the MiniMax provider and OAuth placeholder in config:

```toml
default_provider = "minimax-oauth"
api_key = "minimax-oauth"
```

Then provide one of the following credentials via environment variables:

- `MINIMAX_OAUTH_TOKEN` (preferred, direct access token)
- `MINIMAX_API_KEY` (legacy/static token)
- `MINIMAX_OAUTH_REFRESH_TOKEN` (auto-refreshes access token at startup)

Optional:

- `MINIMAX_OAUTH_REGION=global` or `cn` (defaults by provider alias)
- `MINIMAX_OAUTH_CLIENT_ID` to override the default OAuth client id

Channel compatibility note:

- For MiniMax-backed channel conversations, runtime history is normalized to keep valid `user`/`assistant` turn order.
- Channel-specific delivery guidance (for example Telegram attachment markers) is merged into the leading system prompt instead of being appended as a trailing `system` turn.

## Qwen Code OAuth Setup (config.toml)

Set Qwen Code OAuth mode in config:

```toml
default_provider = "qwen-code"
api_key = "qwen-oauth"
```

Credential resolution for `qwen-code`:

1. Explicit `api_key` value (if not the placeholder `qwen-oauth`)
2. `QWEN_OAUTH_TOKEN`
3. `~/.qwen/oauth_creds.json` (reuses Qwen Code cached OAuth credentials)
4. Optional refresh via `QWEN_OAUTH_REFRESH_TOKEN` (or cached refresh token)
5. If no OAuth placeholder is used, `DASHSCOPE_API_KEY` can still be used as fallback

Optional endpoint override:

- `QWEN_OAUTH_RESOURCE_URL` (normalized to `https://.../v1` if needed)
- If unset, `resource_url` from cached OAuth credentials is used when available

## Model Routing (`hint:<name>`)

You can route model calls by hint using `[[model_routes]]`:

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

Then call with a hint model name (for example from tool or integration paths):

```text
hint:reasoning
```

## Embedding Routing (`hint:<name>`)

You can route embedding calls with the same hint pattern using `[[embedding_routes]]`.
Set `[memory].embedding_model` to a `hint:<name>` value to activate routing.

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

Supported embedding providers:

- `none`
- `openai`
- `custom:<url>` (OpenAI-compatible embeddings endpoint)

Optional per-route key override:

```toml
[[embedding_routes]]
hint = "semantic"
provider = "openai"
model = "text-embedding-3-small"
api_key = "sk-route-specific"
```

## Upgrading Models Safely

Use stable hints and update only route targets when providers deprecate model IDs.

Recommended workflow:

1. Keep call sites stable (`hint:reasoning`, `hint:semantic`).
2. Change only the target model under `[[model_routes]]` or `[[embedding_routes]]`.
3. Run:
   - `zeroclaw doctor`
   - `zeroclaw status`
4. Smoke test one representative flow (chat + memory retrieval) before rollout.

This minimizes breakage because integrations and prompts do not need to change when model IDs are upgraded.
