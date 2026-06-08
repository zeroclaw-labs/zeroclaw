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

[channels.telegram.home]
bot_token = "..."

[channels.slack.engineering]
bot_token = "..."

[channels.slack.research]
bot_token = "..."

[agents.fast]
model_provider  = "anthropic.haiku"
risk_profile    = "hardened"
runtime_profile = "tight"            # fewer iterations for snappy public replies
channels        = ["telegram.home"]

[agents.deep]
model_provider  = "anthropic.sonnet"
risk_profile    = "hardened"
runtime_profile = "deep"             # higher iteration cap for engineering tasks
channels        = ["slack.engineering"]

[agents.reasoner]
model_provider  = "deepseek.reasoner"
risk_profile    = "hardened"
runtime_profile = "deep"             # extended chains for research-style prompts
channels        = ["slack.research"]

# Shared `hardened` posture across the three public-facing agents,
# distinct `tight` / `deep` runtime profiles per per-agent throughput
# intent. `risk_profile` and `runtime_profile` are independent maps.

[risk_profiles.hardened]
level                            = "supervised"
workspace_only                   = true
require_approval_for_medium_risk = true
block_high_risk_commands         = true

[runtime_profiles.tight]
max_tool_iterations  = 5
max_actions_per_hour = 30

[runtime_profiles.deep]
max_tool_iterations  = 50
max_actions_per_hour = 200
```

Each channel binds to one agent at a time. To move a channel to a different agent, edit the `channels = [...]` list on the agent that should pick it up, `Config::validate()` makes sure references resolve at startup.

## Cross-vendor reliability: use OpenRouter

OpenRouter is treated as a single first-class provider. It handles vendor fan-out and uptime behind one endpoint:

```toml
[providers.models.openrouter.home]
model   = "anthropic/claude-sonnet-4-20250514"
api_key = "sk-or-..."

[agents.assistant]
model_provider = "openrouter.home"
risk_profile   = "hardened"
# runtime_profile omitted — uses runtime defaults

[risk_profiles.hardened]
level = "supervised"
```

If your goal is "one provider goes down, automatically use another", that's OpenRouter's job, not ZeroClaw's. The runtime sees one provider; OpenRouter does the cross-vendor work upstream.

## Same-vendor retry

For transient errors (network blip, 503, timeout) against the *same* provider, ZeroClaw retries with exponential backoff. This is configurable globally:

```toml
[reliability]
provider_retries    = 2          # retries per provider attempt before bailing
provider_backoff_ms = 500        # initial backoff; doubles per retry
```

Defaults are 2 retries, 500 ms initial backoff. These are inside-one-provider retries.

## API key rotation

For providers that frequently encounter rate limits, supply additional API keys that ZeroClaw will rotate through on `429` responses:

```toml
[reliability]
api_keys = ["sk-key-2", "sk-key-3", "sk-key-4"]
```

The primary `api_key` (configured on the provider entry) is always tried first; these extras are rotated on rate-limit errors. All keys must belong to the same provider account class, this is rate-limit smoothing, not multi-tenant key juggling.

## Local development with hosted alternative

Run a local-Ollama agent and a hosted-provider agent side by side; route each channel to whichever you want it to use.

```toml
[providers.models.ollama.local]
uri   = "http://localhost:11434"
model = "qwen3.6:35b-a3b"

[providers.models.openrouter.home]
model   = "anthropic/claude-haiku-4-5-20251001"
api_key = "sk-or-..."

[channels.telegram.production]
bot_token = "..."

[channels.slack.production]
bot_token = "..."

[agents.dev]
model_provider  = "ollama.local"
risk_profile    = "permissive"      # local dev box — looser gates
runtime_profile = "deep"            # plenty of iterations during iteration

[agents.prod]
model_provider  = "openrouter.home"
risk_profile    = "hardened"        # public channels — strict gates
runtime_profile = "tight"           # production discipline — short loops, low spend
channels        = ["telegram.production", "slack.production"]

[risk_profiles.permissive]
level          = "full"
workspace_only = false

[risk_profiles.hardened]
level                            = "supervised"
workspace_only                   = true
require_approval_for_medium_risk = true
block_high_risk_commands         = true

[runtime_profiles.deep]
max_tool_iterations  = 50
max_actions_per_hour = 200

[runtime_profiles.tight]
max_tool_iterations  = 5
max_actions_per_hour = 30
```

The `dev` agent runs from the CLI (no channel binding required, `zeroclaw agent -a dev` is enough). When Ollama is down, the dev agent fails fast and surfaces the error. The prod channels are unaffected.

## Cost tiering: heavy model when needed, fast model otherwise

Run two agents and route channels to the appropriate tier. The `delegate` tool lets one agent hand off to another mid-conversation. Delegation is gated: the caller's risk profile must set `delegation_policy mode = "allow"`, and **both agents must share the same risk profile** (delegation does not cross trust tiers). So the frontline and heavy agents below run on the *same* `trusted` risk profile, they differ in model and runtime profile (iteration budget), not in trust surface.

```toml
[providers.models.anthropic.opus]
model   = "claude-opus-4-7"
api_key = "sk-ant-..."
# (no temperature — claude-opus-4-7 rejects any temperature setting)

[providers.models.anthropic.haiku]
model   = "claude-haiku-4-5-20251001"
api_key = "sk-ant-..."

[channels.telegram.home]
bot_token = "..."

[agents.frontline]
model_provider  = "anthropic.haiku"
risk_profile    = "trusted"      # shared trust tier (delegation requires a match)
runtime_profile = "tight"        # low iteration cap, fast turn-around
channels        = ["telegram.home"]

[agents.heavy]
model_provider  = "anthropic.opus"
risk_profile    = "trusted"      # SAME profile as frontline — required to be delegable
runtime_profile = "deep"         # high iteration cap for chain-of-thought work
# No channels — invoked via the delegate tool from frontline

# runtime_profile references an independent alias map from risk_profile;
# the two agents share one risk profile but differ in runtime profile.

[risk_profiles.trusted]
level                            = "supervised"
workspace_only                   = true
require_approval_for_medium_risk = true
block_high_risk_commands         = true
# allow this profile's agents to delegate to each other; without this,
# delegation is forbidden by default.
delegation_policy                = { mode = "allow" }
allowed_tools                    = ["shell", "file_read", "memory_recall", "delegate"]

[runtime_profiles.tight]
max_tool_iterations  = 5
max_actions_per_hour = 30

[runtime_profiles.deep]
max_tool_iterations  = 50
max_actions_per_hour = 200
```

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

Persisted logs (`"rolling"` is the default) capture retry and key-rotation behaviour:

```toml
[observability]
log_persistence = "rolling"
log_persistence_path = "state/runtime-trace.jsonl"
```

Then query traces:

```bash
zeroclaw doctor traces --contains "retry"
zeroclaw doctor traces --contains "429"
zeroclaw doctor traces --contains "model_provider"
```

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
