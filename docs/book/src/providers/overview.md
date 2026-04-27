# Model Providers — Overview

Model providers are ZeroClaw's abstraction over any LLM endpoint the agent can call. Every chat-completion request goes through a `Provider` trait implementation (`zeroclaw-api::Provider`), whether the target is a remote API, a self-hosted inference server, or a local Ollama model.

Why "model" provider? We use the phrase "model provider" consistently — there may be other kinds of providers in the future (memory providers, tool providers, etc.) and keeping the word specific avoids ambiguity.

## Supported providers

Three categories:

### Native

One-off implementations that talk to a provider's native API:

- **Anthropic** — Claude models. Supports OAuth (`sk-ant-oat*`) and API keys.
- **OpenAI** — GPT and o-series models.
- **Ollama** — local inference. Native `/api/chat` endpoint with schema-based structured output.
- **Bedrock** — AWS foundation models (Claude, Llama, Titan, etc.). IAM-authenticated.
- **Gemini** — Google's API. Separate `gemini_cli` provider for CLI-based auth.
- **Azure OpenAI** — Microsoft's Azure-hosted OpenAI endpoint.
- **Copilot** — GitHub Copilot as a chat model. OAuth flow built in.
- **Claude Code** — Anthropic MCP proxy that lets the agent delegate to a Claude Code session.
- **Telnyx** — voice AI over Telnyx's platform.
- **KiloCLI** — local inference via KiloCLI.

### OpenAI-compatible (single implementation, 20+ endpoints)

One Rust implementation (`compatible.rs`) handles every OpenAI-compatible endpoint. A partial list:

- Groq, Mistral, xAI, DeepSeek
- OpenRouter (also exposes provider-routing semantics — see [Fallback & routing](./fallback-and-routing.md))
- Venice, Moonshot, Synthetic, OpenCode, Z.AI, GLM, MiniMax, Qianfan
- Vercel Gateway, Cloudflare Gateway
- Any SaaS or self-hosted endpoint that speaks OpenAI's chat-completions API

To add one of these, you don't implement a new provider — you add a config entry pointing at its base URL.

### Meta providers

- **`reliable`** — wraps any provider with a fallback chain. First failure falls to the next provider in the list.
- **`router`** — a multi-provider router that picks a backend based on per-request hints (`hint:reasoning`, `hint:cheap`, `hint:vision`, etc.).

## Configuration shape

Every provider is configured under `[providers.models.<name>]`:

```toml
[providers.models.claude]
kind = "anthropic"
model = "claude-haiku-4-5-20251001"
api_key = "sk-ant-..."

[providers.models.local]
kind = "ollama"
base_url = "http://localhost:11434"
model = "qwen3.6:35b-a3b"

[providers.models.groq]
kind = "openai-compatible"
base_url = "https://api.groq.com/openai"
model = "llama-3.3-70b-versatile"
api_key = "gsk_..."
```

See [Configuration](./configuration.md) for the full schema.

## Selecting a provider

Two mechanisms:

1. **`default_model`** in the top-level config picks which provider the agent loop uses by default.
2. **Channels and tools can override.** A channel config can specify `provider = "claude"` to use Claude for that channel's responses while `default_model` stays set to a cheaper option for the rest.

## Why provider-agnostic matters

Two practical reasons:

- **Vendor lock-in is a liability.** If OpenAI changes their pricing, throttles you, or deprecates a model, you swap `kind = "openai"` for `kind = "anthropic"` in one file. The rest of the system doesn't care.
- **Cost/performance routing.** A fallback chain of `[sonnet, haiku, local]` means you use the best model when it's available, fall back to cheaper hosted when it's not, and keep running on local Ollama during an API outage.

## What's next

- [Configuration](./configuration.md) — the full `[providers.*]` schema
- [Streaming](./streaming.md) — how tokens, tool calls, and reasoning deltas flow
- [Fallback & routing](./fallback-and-routing.md) — reliable and router meta-providers
- [Provider catalog](./catalog.md) — every supported provider, its config shape, and notes
- [Custom providers](./custom.md) — implementing a new one
