# Multi-Model Setup

A walkthrough of the common patterns for using multiple model providers: per-agent dispatch, cost tiering, local-first with hosted backup, API key rotation, and rate-limit handling.

> **Reference material** for the provider system lives in:
> - [Model Providers → Overview](../providers/overview.md): what providers are, configuration shape
> - [Model Providers → Routing](../providers/routing.md): per-agent dispatch and OpenRouter
> - [Model Providers → Catalog](../providers/catalog.md): every provider's config shape

## When to use multi-model setup

Multi-model configuration is useful for:

1. **Cost tiering**: cheap model handles high-volume channels; reasoning model handles complex requests
2. **Capability routing**: vision-capable model for image-bearing channels, reasoning model for research workflows
3. **Local-first development**: local Ollama for development, hosted endpoint for production
4. **Per-team isolation**: different teams use different agents with different model_providers and credentials
5. **Rate-limit handling**: rotate through API keys on `429` (rate limit) responses

## Core idea: per-agent dispatch

Each `[agents.<alias>]` entry points at exactly one `[providers.models.<type>.<alias>]`. If the model goes down, the agent goes down; the operator routes affected channels to a different agent. See [Routing](../providers/routing.md) for the full pattern.

To run multiple models, run multiple agents, each binding to one model provider. Each channel binds to one agent at a time. To move a channel to a different agent, edit the `channels` list on the agent that should pick it up; `Config::validate()` makes sure references resolve at startup.

## Cross-vendor reliability: use OpenRouter

OpenRouter is treated as a single first-class provider. It handles vendor fan-out and uptime behind one endpoint. If your goal is "one provider goes down, automatically use another", that's OpenRouter's job, not ZeroClaw's. The runtime sees one provider; OpenRouter does the cross-vendor work upstream.

## Same-vendor retry

For transient errors (network blip, 503, timeout) against the *same* provider, ZeroClaw retries with exponential backoff, configurable globally under `reliability` (defaults: 2 retries, 500 ms initial backoff). These are inside-one-provider retries.

## API key rotation

For providers that frequently encounter rate limits, supply additional API keys on the provider entry that ZeroClaw rotates through on `429` responses. The primary `api_key` is always tried first; extras are rotated on rate-limit errors. All keys must belong to the same provider account class; this is rate-limit smoothing, not multi-tenant key juggling.

## Local development with hosted alternative

Run a local-Ollama agent and a hosted-provider agent side by side; route each channel to whichever you want it to use.

The `dev` agent runs from the CLI (no channel binding required, `zeroclaw agent -a dev` is enough). When Ollama is down, the dev agent fails fast and surfaces the error. The prod channels are unaffected.

## Local-small no-text-fallback profile

Small local models usually need a runtime profile, not a provider-specific mode. Keep the Ollama provider focused on connection details, then use `[runtime_profiles.<alias>]` to tighten the prompt/tool loop behavior.

```toml
[providers.models.ollama.local]
uri   = "http://localhost:11434"
model = "qwen2.5-coder:7b"

[agents.local]
model_provider  = "ollama.local"
risk_profile    = "supervised"
runtime_profile = "local_small"

[risk_profiles.supervised]
level                            = "supervised"
workspace_only                   = true
require_approval_for_medium_risk = true
block_high_risk_commands         = true

[runtime_profiles.local_small]
compact_context          = true
strict_tool_parsing      = true
max_tool_iterations      = 4
max_history_messages     = 20
max_context_tokens       = 8000
max_tool_result_chars    = 4000
keep_tool_context_turns  = 1
```

This profile composes existing primitives:

- `compact_context` keeps startup context small.
- `strict_tool_parsing` treats XML/JSON-looking fallback text as assistant text unless the provider returns native tool calls.
- `max_tool_iterations`, `max_context_tokens`, and `max_tool_result_chars` bound runaway loops and oversized tool context.

With Ollama, this is a no-text-fallback profile: authorized tools remain configured in `risk_profile`, but text-form tool markup from the model is not executed. Use it for chat-first local agents, or for providers that return native/structured tool calls. If a local model must use ZeroClaw's text fallback tool syntax, set `strict_tool_parsing = false` and keep the other small-model limits.

## Cost tiering: heavy model when needed, fast model otherwise

Run two agents and route channels to the appropriate tier. The `delegate` tool lets one agent hand off to another mid-conversation. [Delegation](../agents/delegation.md) is gated: the caller's risk profile must set `delegation_policy mode = "allow"`, and the target must be reachable from the caller (a same-profile peer, or an explicit entry in the caller's `delegates` list). The frontline and heavy agents below run on the *same* `trusted` risk profile, so they reach each other as same-profile peers; they differ in model and runtime profile (iteration budget), not in trust surface.

The frontline agent handles every inbound message on Haiku. When it needs deeper reasoning, it calls the `delegate` tool with `agent = "heavy"`; because both agents share the `trusted` risk profile and that profile allows delegation, the heavier agent picks up the sub-task on Opus.

## Error handling

Inside-one-provider retries trigger on:

1. **Timeout**: provider did not respond within the configured timeout
2. **Connection error**: network or DNS failure
3. **Rate limit (429)**: triggers API key rotation first; if all keys exhausted, fails up to the channel
4. **Service unavailable (503)**: temporary service issue

Retries are NOT triggered by:

1. **Invalid request (400)**: malformed input; retrying won't help
2. **Permanent auth failure**: invalid API key format
3. **Model output errors**: the model responded but returned an error payload

When all retries are exhausted on a single provider, the failure surfaces to the calling channel. There is no automatic cross-provider retry, that's the point of using OpenRouter or splitting traffic across multiple agents.

## Debugging

Persisted logs (`"rolling"` is the default) capture retry and key-rotation behaviour. Then query traces:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw doctor traces --contains "retry"
zeroclaw doctor traces --contains "429"
zeroclaw doctor traces --contains "model_provider"
```

</div>

## Best practices

1. **One agent per routing intent.** If two channels need different model behavior, name two agents.
2. **Use OpenRouter for cross-vendor reliability.** Cross-vendor "if Claude fails, try OpenAI" is OpenRouter's job; configure it as one provider and let its endpoint handle the fan-out.
3. **Keep API key rotation pools homogeneous.** All keys in `[reliability] api_keys` should be from the same provider account, this is rate-limit smoothing, not multi-tenancy.
4. **Smoke-test each agent in isolation.** `zeroclaw agent -a <alias>` runs an agent without channel plumbing in the way.
5. **Document agent intent.** Add `# comment` lines explaining which channels each agent serves and why.
6. **Inject secrets via env, not inline.** `ZEROCLAW_providers__models__<type>__<alias>__api_key=...` sets `api_key` at startup; see [Environment variables](../reference/env-vars.md).
7. **Separate dev and prod agents.** Each environment gets its own `[agents.<alias>]` entry bound to its own channels.

## Credential resolution

Each provider entry resolves credentials in this order:

1. **Inline `api_key`** on the provider entry.
2. **Secrets store** at `~/.zeroclaw/secrets`.
3. **Generic env override**: `ZEROCLAW_providers__models__<type>__<alias>__api_key=...` at startup. See [Environment variables](../reference/env-vars.md) for the full grammar.
4. **Per-vendor env var** when the family supports it (e.g. `ANTHROPIC_API_KEY` / `ANTHROPIC_OAUTH_TOKEN` for Anthropic; `OPENROUTER_API_KEY` for OpenRouter).

Credentials are not shared between providers, set them per provider entry.

## Related Documentation

- [Model Providers → Overview](../providers/overview.md)
- [Model Providers → Routing](../providers/routing.md)
- [Environment variables](../reference/env-vars.md)
