# ZeroClaw Providers Reference

This document maps provider IDs, aliases, and credential environment variables.

Last verified: **February 19, 2026**.

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
| `qwen` | `dashscope`, `qwen-intl`, `dashscope-intl`, `qwen-us`, `dashscope-us` | No | `DASHSCOPE_API_KEY` |
| `groq` | — | No | `GROQ_API_KEY` |
| `mistral` | — | No | `MISTRAL_API_KEY` |
| `xai` | `grok` | No | `XAI_API_KEY` |
| `deepseek` | — | No | `DEEPSEEK_API_KEY` |
| `together` | `together-ai` | No | `TOGETHER_API_KEY` |
| `fireworks` | `fireworks-ai` | No | `FIREWORKS_API_KEY` |
| `perplexity` | — | No | `PERPLEXITY_API_KEY` |
| `cohere` | — | No | `COHERE_API_KEY` |
| `copilot` | `github-copilot` | No | (use config/`API_KEY` fallback with GitHub token) |
| `lmstudio` | `lm-studio` | Yes | (optional; local by default) |
| `nvidia` | `nvidia-nim`, `build.nvidia.com` | No | `NVIDIA_API_KEY` |

### Bedrock Notes

- Provider ID: `bedrock` (alias: `aws-bedrock`)
- API: [Converse API](https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_Converse.html)
- Authentication: AWS AKSK (not a single API key). Set `AWS_ACCESS_KEY_ID` + `AWS_SECRET_ACCESS_KEY` environment variables.
- Optional: `AWS_SESSION_TOKEN` for temporary/STS credentials, `AWS_REGION` or `AWS_DEFAULT_REGION` (default: `us-east-1`).
- Default onboarding model: `anthropic.claude-sonnet-4-5-20250929-v1:0`
- Supports native tool calling and prompt caching (`cachePoint`).
- Cross-region inference profiles supported (e.g., `us.anthropic.claude-*`).
- Model IDs use Bedrock format: `anthropic.claude-sonnet-4-6`, `anthropic.claude-opus-4-6-v1`, etc.

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
