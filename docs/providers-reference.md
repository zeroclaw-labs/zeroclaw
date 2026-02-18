# ZeroClaw Providers Reference

This document maps provider IDs, aliases, and credential environment variables.

Last verified: **February 18, 2026**.

## How to List Providers

```bash
zeroclaw providers
```

## Credential Resolution Order

Runtime resolution order is:

1. Explicit credential from config/CLI
2. Provider-specific env var(s)
3. Generic fallback env vars: `ZEROCLAW_API_KEY` then `API_KEY`

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
| `synthetic` | — | No | `SYNTHETIC_API_KEY` |
| `opencode` | `opencode-zen` | No | `OPENCODE_API_KEY` |
| `zai` | `z.ai` | No | `ZAI_API_KEY` |
| `glm` | `zhipu` | No | `GLM_API_KEY` |
| `minimax` | — | No | `MINIMAX_API_KEY` |
| `bedrock` | `aws-bedrock` | No | (use config/`API_KEY` fallback) |
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

## Custom Endpoints

- OpenAI-compatible endpoint:

```toml
default_provider = "custom:https://your-api.example.com"
```

- Anthropic-compatible endpoint:

```toml
default_provider = "anthropic-custom:https://your-api.example.com"
```

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

