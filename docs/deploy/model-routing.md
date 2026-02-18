# ZeroClaw Model Routing Guide

Last updated: February 18, 2026

## Overview

ZeroClaw supports **model routing** via hints in the configuration. This allows routing different task types to different providers/models - for example, using a fast model for quick tasks, a powerful model for reasoning, and a specialized model for coding.

## Configuration

Add model routes to `~/.zeroclaw/config.toml`:

```toml
# Default fallback (used when no hint matches)
default_provider = "anthropic"
default_model = "claude-sonnet-4-6"

# Route reasoning tasks to a powerful model
[[model_routes]]
hint = "reasoning"
provider = "anthropic"
model = "claude-opus-4-20250514"

# Route fast/quick tasks to Groq (super fast inference)
[[model_routes]]
hint = "fast"
provider = "groq"
model = "llama-3.3-70b-versatile"

# Route image/vision tasks to a vision-capable model
[[model_routes]]
hint = "vision"
provider = "openai"
model = "gpt-4o"

# Route coding tasks to a coding-specialized model
[[model_routes]]
hint = "coding"
provider = "deepseek"
model = "deepseek-coder"
```

## Usage

### CLI

```bash
# Use reasoning route
zeroclaw agent --model "hint:reasoning" "Analyze this complex problem..."

# Use fast route
zeroclaw agent --model "hint:fast" "Quick question: what is 2+2?"

# Use coding route
zeroclaw agent --model "hint:coding" "Write a Python function to sort a list"
```

### Gateway/API

When calling the gateway API, specify the hint model in the request:

```bash
curl -X POST http://localhost:3000/webhook \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"message": "Your query here", "model": "hint:reasoning"}'
```

## Supported Providers

| Provider ID | Aliases | Env Var | Notes |
|-------------|---------|---------|-------|
| `anthropic` | — | `ANTHROPIC_API_KEY` | Best for reasoning |
| `openai` | — | `OPENAI_API_KEY` | Vision, general purpose |
| `groq` | — | `GROQ_API_KEY` | **Fastest inference** |
| `deepseek` | — | `DEEPSEEK_API_KEY` | Coding, cheap |
| `xai` | `grok` | `XAI_API_KEY` | Grok models |
| `mistral` | — | `MISTRAL_API_KEY` | Code, general |
| `gemini` | `google` | `GEMINI_API_KEY` | Vision, long context |
| `moonshot` | `kimi` | `MOONSHOT_API_KEY` | Kimi models |
| `kimi-code` | — | `KIMI_CODE_API_KEY` | Kimi coding model |
| `ollama` | — | (local) | Free, local models |
| `qwen` | `dashscope` | `DASHSCOPE_API_KEY` | Alibaba Qwen |
| `glm` | `zhipu` | `GLM_API_KEY` | Zhipu GLM |
| `zai` | `z.ai` | `ZAI_API_KEY` | Z.ai GLM-5 |

## Custom Endpoints

For OpenAI-compatible APIs:

```toml
default_provider = "custom:https://your-api.com"
api_key = "your-api-key"
default_model = "your-model-name"
```

For Anthropic-compatible APIs:

```toml
default_provider = "anthropic-custom:https://your-api.com"
api_key = "your-api-key"
default_model = "claude-sonnet-4-6"
```

## Environment Variables

Multiple API keys can be set simultaneously. ZeroClaw resolves credentials in this order:

1. Explicit credential from config/CLI
2. Provider-specific env var (e.g., `ANTHROPIC_API_KEY`)
3. Generic fallback: `ZEROCLAW_API_KEY`, then `API_KEY`

Example `.env`:

```bash
# Multiple providers - set all keys, use routing
ANTHROPIC_API_KEY=sk-ant-...
OPENAI_API_KEY=sk-...
GROQ_API_KEY=gsk_...
DEEPSEEK_API_KEY=sk-...
MOONSHOT_API_KEY=sk-...
```

## Example: Multi-Provider Setup

```toml
# ~/.zeroclaw/config.toml

# Default: Kimi K2.5 via Moonshot
default_provider = "moonshot"
default_model = "kimi-k2.5"

# Reasoning: Claude Opus
[[model_routes]]
hint = "reasoning"
provider = "anthropic"
model = "claude-opus-4-20250514"

# Fast: Groq Llama
[[model_routes]]
hint = "fast"
provider = "groq"
model = "llama-3.3-70b-versatile"

# Coding: DeepSeek Coder
[[model_routes]]
hint = "coding"
provider = "deepseek"
model = "deepseek-coder"

# Vision: GPT-4o
[[model_routes]]
hint = "vision"
provider = "openai"
model = "gpt-4o"
```

## Verifying Configuration

```bash
# Check current status
zeroclaw status

# List available providers
zeroclaw providers

# Test a specific model route
zeroclaw agent --model "hint:fast" "Hello, are you working?"
```

## Related Docs

- [ZeroClaw Providers Reference](https://github.com/zeroclaw-labs/zeroclaw/blob/main/docs/providers-reference.md)
- [ZeroClaw Config Reference](https://github.com/zeroclaw-labs/zeroclaw/blob/main/docs/config-reference.md)
- [Custom Providers](https://github.com/zeroclaw-labs/zeroclaw/blob/main/docs/custom-providers.md)