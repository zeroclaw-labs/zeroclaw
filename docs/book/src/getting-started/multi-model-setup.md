# Multi-Model Setup

A walkthrough of the common patterns for using multiple model providers: per-agent dispatch, cost tiering, local-first with hosted backup, API key rotation, and rate-limit handling.

> **Reference material** for the provider system lives in:
> - [Model Providers → Overview](../providers/overview.md) — what providers are, configuration shape
> - [Model Providers → Fallback & routing](../providers/fallback-and-routing.md) — per-agent dispatch and OpenRouter
> - [Model Providers → Catalog](../providers/catalog.md) — every provider's config shape

## When to use multi-model setup

Multi-model configuration is useful for:

- **Cost tiering**: Cheap model handles high-volume channels; reasoning model handles complex requests
- **Capability routing**: Vision-capable model for image-bearing channels, reasoning model for research workflows
- **Local-first development**: Local Ollama for development; hosted endpoint for production
- **Per-team isolation**: Different teams use different agents with different model_providers and credentials
- **Rate-limit handling**: Rotate through API keys on `429` (rate limit) responses

## Core idea — per-agent dispatch

In V3 there is no in-process model fallback chain. Each `[agents.<alias>]` entry points at exactly one `[providers.models.<type>.<alias>]`. If the model goes down, the agent goes down — the operator picks how the channels above respond (typically by routing to a different agent). This is intentional: see [Fallback & routing](../providers/fallback-and-routing.md) for the rationale.

To run multiple models, run multiple agents:

```toml
[providers.models.anthropic.haiku]
model   = "claude-haiku-4-5-20251001"
api_key = "sk-ant-..."

[providers.models.anthropic.sonnet]
model   = "claude-sonnet-4-6"
api_key = "sk-ant-..."

[providers.models.deepseek.reasoner]
model   = "deepseek-reasoner"
api_key = "sk-..."

[agents.fast]
enabled        = true
model_provider = "anthropic.haiku"
risk_profile   = "default"
runtime_profile = "default"
channels        = ["telegram.default"]

[agents.deep]
enabled        = true
model_provider = "anthropic.sonnet"
risk_profile   = "default"
runtime_profile = "default"
channels        = ["slack.engineering"]

[agents.reasoner]
enabled        = true
model_provider = "deepseek.reasoner"
risk_profile   = "default"
runtime_profile = "default"
channels        = ["slack.research"]
```

Each channel binds to one agent at a time. To move a channel to a different agent, edit the `channels = [...]` list on the agent that should pick it up — `Config::validate()` makes sure references resolve at startup.

## Cross-vendor reliability — use OpenRouter

OpenRouter is treated as a single first-class provider. It handles vendor fan-out, fallback, and uptime behind one endpoint:

```toml
[providers.models.openrouter.default]
model   = "anthropic/claude-sonnet-4-20250514"
api_key = "sk-or-..."

[agents.default]
enabled        = true
model_provider = "openrouter.default"
risk_profile   = "default"
runtime_profile = "default"
```

If your goal is "one provider goes down, automatically use another", that's OpenRouter's job — not ZeroClaw's. The runtime sees one provider; OpenRouter does the cross-vendor work upstream.

## Same-vendor retry

For transient errors (network blip, 503, timeout) against the *same* provider, ZeroClaw retries with exponential backoff. This is configurable globally:

```toml
[reliability]
provider_retries     = 2          # retries per provider attempt before bailing
provider_backoff_ms  = 500        # initial backoff; doubles per retry
```

Defaults are 2 retries, 500 ms initial backoff. These are inside-one-provider retries — there is no in-process cross-provider fallback.

## API key rotation

For providers that frequently encounter rate limits, supply additional API keys that ZeroClaw will rotate through on `429` responses:

```toml
[reliability]
api_keys = ["sk-key-2", "sk-key-3", "sk-key-4"]
```

The primary `api_key` (configured on the provider entry) is always tried first; these extras are rotated on rate-limit errors. All keys must belong to the same provider account class — this is rate-limit smoothing, not multi-tenant key juggling.

## Local development with hosted backup

Use a local Ollama instance for an agent that handles your dev/test channel; bind a separate agent to a hosted provider for production channels.

```toml
[providers.models.ollama.default]
uri   = "http://localhost:11434"
model = "qwen3.6:35b-a3b"

[providers.models.openrouter.default]
model   = "anthropic/claude-haiku-4-5-20251001"
api_key = "sk-or-..."

[agents.dev]
enabled        = true
model_provider = "ollama.default"
risk_profile   = "default"
runtime_profile = "default"
channels        = ["cli.default"]

[agents.prod]
enabled        = true
model_provider = "openrouter.default"
risk_profile   = "default"
runtime_profile = "default"
channels        = ["telegram.production", "slack.production"]
```

When Ollama is down, the dev channel fails fast and surfaces the error. The prod channels are unaffected.

## Cost tiering — heavy model when needed, fast model otherwise

Run two agents and route channels to the appropriate tier. The `delegate` tool lets one agent hand off to another mid-conversation:

```toml
[providers.models.anthropic.opus]
model   = "claude-opus-4-7"
api_key = "sk-ant-..."

[providers.models.anthropic.haiku]
model   = "claude-haiku-4-5-20251001"
api_key = "sk-ant-..."

[agents.frontline]
enabled        = true
model_provider = "anthropic.haiku"
risk_profile   = "default"
runtime_profile = "default"
channels        = ["telegram.default"]

[agents.heavy]
enabled        = true
model_provider = "anthropic.opus"
risk_profile   = "default"
runtime_profile = "default"
# No channels — invoked via the delegate tool from frontline
```

The frontline agent handles every inbound message on Haiku. When it needs deeper reasoning, it calls the `delegate` tool with `agent = "heavy"` and the heavier agent picks up the sub-task.

## Hot reload

The `[reliability]` section is hot-reloadable — updates to `config.toml` take effect on the next inbound message without a restart. Per-agent `model_provider` references are also hot-reloadable in the same way.

## Error handling

Inside-one-provider retries trigger on:

- **Timeout**: provider did not respond within the configured timeout
- **Connection error**: network or DNS failure
- **Rate limit (429)**: triggers API key rotation first; if all keys exhausted, fails up to the channel
- **Service unavailable (503)**: temporary service issue

Retries are NOT triggered by:

- **Invalid request (400)**: malformed input; retrying won't help
- **Permanent auth failure**: invalid API key format
- **Model output errors**: the model responded but returned an error payload

When all retries are exhausted on a single provider, the failure surfaces to the calling channel. There is no automatic cross-provider retry — that's the point of using OpenRouter or splitting traffic across multiple agents.

## Debugging

Enable runtime traces to see retry and key-rotation behavior:

```toml
[observability]
runtime_trace_mode = "rolling"
runtime_trace_path = "state/runtime-trace.jsonl"
```

Then query traces:

```bash
zeroclaw doctor traces --contains "retry"
zeroclaw doctor traces --contains "429"
zeroclaw doctor traces --contains "model_provider"
```

## Best practices

1. **One agent per routing intent.** If two channels need different model behavior, name two agents.
2. **Use OpenRouter for cross-vendor reliability.** Don't try to encode "if Claude fails, try OpenAI" in your config — it doesn't exist anymore. OpenRouter does this better than any in-process fallback could.
3. **Keep API key rotation pools homogeneous.** All keys in `[reliability] api_keys` should be from the same provider account — this is rate-limit smoothing, not multi-tenancy.
4. **Test each agent in isolation.** `zeroclaw chat --agent <alias>` smoke-tests an agent without channel plumbing in the way.
5. **Document agent intent.** Add `# comment` lines explaining which channels each agent serves and why.
6. **Use environment variables for secrets.** Store API keys in env or the secrets store, not inline in config.
7. **Separate dev and prod agents.** Don't share a `default` agent between local dev and production channels — bind them explicitly.

## Credential resolution

Each provider entry resolves credentials independently using the standard order:

1. **Inline `api_key`** on the provider entry
2. **Secrets store** at `~/.zeroclaw/secrets`
3. **Provider-specific env var** — `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GROQ_API_KEY`, etc.
4. **Generic fallback** — `ZEROCLAW_API_KEY`, `API_KEY`

Credentials are not shared between providers. Set them per provider:

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
export OPENAI_API_KEY="sk-..."
export GROQ_API_KEY="gsk-..."
```

## Related Documentation

- [Model Providers → Overview](../providers/overview.md)
- [Model Providers → Fallback & routing](../providers/fallback-and-routing.md)
- [Config reference](../reference/config.md) — generated config field index
