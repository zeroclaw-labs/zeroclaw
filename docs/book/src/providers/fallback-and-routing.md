# Fallback & Routing

ZeroClaw's earlier schema versions exposed two meta-providers — `reliable` (fallback chains) and `router` (task-hint routing). Both are gone in the current typed-family schema. Routing now happens at the **agent layer**, not the model-provider layer.

## Why the change

The old meta-providers tried to shoulder two different jobs at once:

- **Per-call backend selection** ("use the cheap model unless this prompt looks like reasoning").
- **Provider reliability** ("if Claude times out, fall back to OpenAI").

Both conflated request-time intent with infrastructure plumbing. The current model splits them cleanly:

- Per-call backend selection becomes per-agent dispatch — set up multiple agents with different `model_provider` references and route channel traffic accordingly.
- Provider reliability is OpenRouter's job, not ours. Use OpenRouter as a normal provider and let it handle vendor fan-out.

## Per-agent dispatch — the V3 way

Define each routing target as its own agent, then point channels at the agent that should handle their traffic.

```toml
[providers.models.anthropic.sonnet]
model   = "claude-sonnet-4-6"
api_key = "sk-ant-..."

[providers.models.anthropic.haiku]
model   = "claude-haiku-4-5-20251001"
api_key = "sk-ant-..."

[providers.models.deepseek.reasoner]
model   = "deepseek-reasoner"
api_key = "sk-..."

[providers.models.gemini.vision]
model   = "gemini-2.5-pro"
api_key = "..."

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

[agents.eyes]
enabled        = true
model_provider = "gemini.vision"
risk_profile   = "default"
runtime_profile = "default"
channels        = ["discord.media"]
```

Each channel binds to one agent. Channels can move between agents by editing `channels = [...]` on the agent that should pick them up; `Config::validate()` makes sure references resolve.

For ad-hoc multi-step routing inside a single conversation, use the `delegate` tool: an agent can hand off to another configured agent (also typed via `agent_<alias>` references).

## Reliability — use OpenRouter

OpenRouter is treated as a single first-class provider. Your runtime sees one endpoint; OpenRouter handles vendor fallback, model selection, and uptime behind that endpoint.

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

If OpenRouter is unavailable, that's an outage — there is no in-process fallback. Operators who need cross-vendor reliability run multiple ZeroClaw instances behind a load balancer or use OpenRouter's enterprise SLA.

## Why no in-process fallback?

A few practical reasons the V2 `reliable` chain didn't earn its keep:

- **Failure modes are vendor-specific.** "Provider returned 500" means different things for different vendors; a single retry-and-fall-through policy hid bugs more often than it caught them.
- **State across providers is hard.** A fallback chain that swaps providers mid-conversation has to reconcile message-format differences, tool-call IDs, and reasoning-token shapes. Doing it correctly is a lot of code; doing it incorrectly silently corrupts conversation state.
- **OpenRouter does it better.** Vendor fan-out is OpenRouter's whole product. We don't need to reimplement it.
- **Per-agent dispatch is more honest.** When two channels should use different models, naming two agents is clearer than encoding the routing rule inside a meta-provider.

## What does NOT exist in V3

- `kind = "reliable"` — no fallback meta-provider.
- `kind = "router"` — no task-hint router meta-provider.
- `fallback_providers = [...]` field — eradicated from the schema and runtime.
- `default_provider` / `default_model` global keys — eradicated.
- `provider_family_excludes()` runtime filter — gone (typed-family schema makes per-family fields self-describing).

The migration drops these keys at load time and emits a warning so operators upgrading from older configs see what's been removed.

## Observability

Per-agent dispatch decisions are visible in tracing logs:

```
INFO channel=telegram.default routed to agent=fast
INFO agent=fast model_provider=anthropic.haiku turn_id=...
INFO model_provider=anthropic.haiku stream complete tokens={input=512, output=128}
```

For production deployments, wire the log output to Loki / Grafana. See [Operations → Logs & observability](../ops/observability.md).

## See also

- [Overview](./overview.md) — provider model and per-agent dispatch
- [Configuration](./configuration.md) — full `[providers.*]` schema
- [Provider catalog](./catalog.md) — every canonical slot
