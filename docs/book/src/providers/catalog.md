# Provider Catalog

Every provider ZeroClaw ships with. For each: what it talks to, config shape, and notes.

See [Configuration](./configuration.md) for universal fields.

---

## Native

### Anthropic

```toml
[providers.models.claude]
kind = "anthropic"
model = "claude-haiku-4-5-20251001"     # or claude-sonnet-4-6, claude-opus-4-7
api_key = "sk-ant-..."                   # or "sk-ant-oat-..." for OAuth
```

Supports OAuth tokens (`sk-ant-oat*`) from Claude Pro/Team subscriptions — no separate API billing. Streaming, tool calls, vision, and reasoning all supported.

### OpenAI

```toml
[providers.models.gpt]
kind = "openai"
model = "gpt-4o-mini"
api_key = "sk-..."
```

GPT-4o, GPT-5, o-series reasoning models. Reasoning tokens surfaced as `ReasoningDelta` events; see [Streaming](./streaming.md).

### Ollama

```toml
[providers.models.local]
kind = "ollama"
base_url = "http://localhost:11434"
model = "qwen3.6:35b-a3b"
think = false                     # disable chain-of-thought on reasoning models
```

Local inference. Uses Ollama's native `/api/chat`. Schema-based structured output via `format` parameter (reliability varies by model). No API key needed.

### Bedrock

```toml
[providers.models.bedrock]
kind = "bedrock"
region = "us-east-1"
model = "anthropic.claude-3-5-sonnet-20241022-v2:0"
# uses AWS credential chain — IAM role, env vars, ~/.aws/credentials
```

AWS-hosted Claude, Llama, Titan, and others. Auth via the standard AWS credentials chain — no explicit key in config needed if your environment is set up for AWS.

### Gemini

```toml
[providers.models.gemini]
kind = "gemini"
model = "gemini-2.5-pro"
api_key = "..."
```

Google's Gemini API. Supports vision and pre-executed grounded search (see [Streaming](./streaming.md) for `PreExecutedToolCall` events).

### Gemini CLI

```toml
[providers.models.gemini-cli]
kind = "gemini-cli"
model = "gemini-2.5-pro"
```

Shells out to the `gemini` CLI. No API key in config — uses whatever auth the CLI has.

### Azure OpenAI

```toml
[providers.models.azure]
kind = "azure-openai"
base_url = "https://my-resource.openai.azure.com"
deployment = "gpt-4o"
api_version = "2024-10-01-preview"
api_key = "..."
```

### Copilot

```toml
[providers.models.copilot]
kind = "copilot"
model = "gpt-4o"
# authenticate once via `zeroclaw onboard` — the wizard handles the OAuth flow
# and stores a token at ~/.config/zeroclaw/copilot.json
```

Uses a GitHub Copilot subscription for agent inference. OAuth-managed.

### Claude Code

```toml
[providers.models.cc]
kind = "claude-code"
```

Delegates turns to a Claude Code session over MCP. Useful for code-heavy workflows; inherits Claude Code's tool allow-lists and project context.

### Telnyx

```toml
[providers.models.voice-brain]
kind = "telnyx"
model = "..."
api_key = "..."
```

Voice-oriented AI endpoint. Pair with the `clawdtalk` channel for real-time SIP calls.

### KiloCLI

```toml
[providers.models.kilo]
kind = "kilocli"
model = "..."
```

Local inference via KiloCLI.

---

## OpenAI-compatible (`compatible.rs`, ~20+ endpoints)

One Rust impl reused for every endpoint that speaks OpenAI chat completions. Pattern:

```toml
[providers.models.<name>]
kind = "openai-compatible"
base_url = "<endpoint>"
model = "<model-id>"
api_key = "<key>"
```

Verified endpoints:

| Provider | `base_url` | Typical `model` |
|---|---|---|
| Groq | `https://api.groq.com/openai` | `llama-3.3-70b-versatile` |
| Mistral | `https://api.mistral.ai` | `mistral-large-latest` |
| xAI / Grok | `https://api.x.ai` | `grok-2-latest` |
| DeepSeek | `https://api.deepseek.com` | `deepseek-chat`, `deepseek-reasoner` |
| Moonshot | `https://api.moonshot.cn/v1` | `moonshot-v1-32k` |
| Z.AI / GLM | `https://open.bigmodel.cn/api/paas` | `glm-4-plus` |
| MiniMax | `https://api.minimax.chat` | `abab6.5s-chat` |
| Qianfan | `https://qianfan.baidubce.com/v2` | per model |
| Venice | `https://api.venice.ai/api` | per model |
| Vercel AI Gateway | `https://gateway.ai.vercel.app` | per model |
| Cloudflare Gateway | `https://gateway.ai.cloudflare.com/v1/.../.../chat/completions` | per model |
| OpenCode | `https://api.opencode.ai` | per model |
| Manifest | `https://app.manifest.build/v1` | `auto` |
| Synthetic | `https://api.synthetic.ai` | per model |

Any endpoint that claims OpenAI chat-completions compatibility should work — if it doesn't, file an issue with a minimal reproducer.

---

## Meta

### OpenRouter

```toml
[providers.models.openrouter]
kind = "openrouter"
model = "anthropic/claude-sonnet-4-20250514"   # openrouter's vendor/model form
api_key = "sk-or-..."
```

Routes through OpenRouter's fan-out layer. Use when you want one billing relationship across many models.

### Manifest

```toml
[providers.models.manifest]
kind = "manifest"
model = "auto"
api_key = "mnfst_..."
```

[Manifest](https://manifest.build) is an open-source LLM router that cuts inference costs through smart routing across 16+ providers. You get full control over which model handles each request. Route by complexity tier, task-specificity (coding, web browsing, etc.) and custom tiers. Manifest can also be self-hosted with Docker for fully private inference, override `base_url` to point to your local instance (e.g. `http://localhost:2099/v1`).

### Reliable (fallback chain)

```toml
[providers.models.main]
kind = "reliable"
fallback_providers = ["claude", "openrouter", "local"]
```

See [Fallback & routing](./fallback-and-routing.md).

### Router (task-hint)

```toml
[providers.models.brain]
kind = "router"
default = "haiku"
routes = [
    { hint = "reasoning", provider = "deepseek-r1" },
    { hint = "vision",    provider = "gemini" },
]
```

See [Fallback & routing](./fallback-and-routing.md).

---

## Something missing?

If the endpoint you want isn't listed, it's probably OpenAI-compatible — try `kind = "openai-compatible"` with the appropriate `base_url`. If it's not OpenAI-compatible and needs its own implementation, see [Custom providers](./custom.md).
