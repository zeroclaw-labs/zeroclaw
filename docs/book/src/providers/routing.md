# Routing

Routing happens at the **agent layer**. Each agent points at exactly one provider; channels point at agents.

Two layers of decisions:

1. **Per-call backend selection**: "use the cheap model unless this prompt looks like reasoning." Each routing target is its own `[agents.<alias>]` entry with its own `model_provider`. Channels are routed to whichever agent should handle their traffic.
2. **Provider reliability**: vendor-redundancy lives behind a single first-class provider. Configure OpenRouter (or an equivalent) as one provider and let it handle vendor fan-out at its endpoint.

## Per-agent dispatch

Define each routing target as its own agent, then point channels at the agent that should handle their traffic.

Each channel binds to one agent. Channels move between agents by editing `channels = [...]` on the agent that should pick them up; `Config::validate()` makes sure references resolve.

For ad-hoc multi-step routing inside a single conversation, the `spawn_subagent` tool lets an agent run an ephemeral child under its own identity. The child inherits the parent's permissions envelope (see `[risk_profiles.<alias>].allowed_tools`) and returns its final response to the parent's tool loop.

## Hint-based model routes

A narrower mechanism: `[[model_routes]]` lets an agent override the configured `model_provider` for prompts marked with a hint string. Useful when one agent should occasionally reach for a different model without spinning up a second agent.

Routes only fire when a prompt explicitly carries the matching hint. The default request path uses the agent's primary `model_provider`.

## Observability

Per-agent dispatch decisions are visible in tracing logs:

```
INFO channel=telegram.home routed to agent=fast
INFO agent=fast model_provider=anthropic.haiku turn_id=...
INFO model_provider=anthropic.haiku stream complete tokens={input=512, output=128}
```

For production deployments, wire the log output to Loki / Grafana. See [Operations → Logs & observability](../ops/observability.md).

## See also

- [Overview](./overview.md): provider model and per-agent dispatch
- [Configuration](./configuration.md): full `[providers.*]` schema
- [Provider catalog](./catalog.md): every canonical slot
