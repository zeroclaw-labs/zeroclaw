# Routing

ZeroClaw uses routing for two different decisions:

1. **Agent dispatch** selects which agent owns a channel or request. Each agent has its own provider profile and runtime policy.
2. **Provider and model routing** selects a configured provider profile and model for a call, then applies that profile's retry and fallback policy.

An external routing service such as OpenRouter can still perform vendor selection behind one provider profile. It is optional: ZeroClaw also supports first-party hint routes, same-profile model fallback, and fallback across provider profiles.

## Per-agent dispatch

Define each routing target as its own agent, then point channels at the agent that should handle their traffic.

Each channel binds to one agent. Channels move between agents by editing `channels = [...]` on the agent that should pick them up; `Config::validate()` makes sure references resolve.

For ad-hoc multi-step routing inside a single conversation, the `spawn_subagent` tool lets an agent run an ephemeral child under its own identity. The child inherits the parent's permissions envelope (see `[risk_profiles.<alias>].allowed_tools`) and returns its final response to the parent's tool loop.

## Hint-based model routes

A narrower mechanism: `[[model_routes]]` lets an agent override the configured `model_provider` for prompts marked with a hint string. Useful when one agent should occasionally reach for a different model without spinning up a second agent. Each route entry carries a `hint` (the string a prompt must declare to fire it), a `model_provider` (the dotted `<type>.<alias>` profile to switch to, e.g. `deepseek.reasoner`), and a `model` (the provider-local model id, e.g. `deepseek-reasoner`). Configure routes through the gateway, zerocode, or `zeroclaw config set`; see the [Config reference](../reference/config.md#model_routes) for the field schema.

Routes normally fire when a prompt explicitly carries the matching hint. An opted-in effort-routing runtime profile can also select two named routes automatically before provider execution.

An unknown `hint:<name>` logs a warning and stays in the default reliability domain while preserving the literal hint as the requested model. A pinned default entry still serves its active/default pin. An unpinned default entry forwards the literal value, which the provider may reject before normal fallback or error handling continues.

`model_provider` is always a provider profile reference in dotted `<type>.<alias>` form, such as `anthropic.sonnet` or `openai.default`. The profile carries the endpoint, credential reference, compatibility flavor, fallback chain, and optional default model. The `model` field is provider-local state under that profile.

> **Current limitation:** Route pinning depends on the target profile. The primary target is pinned to the active/default model used to construct the router, including when a recognized hint points back to the active primary profile; that hint's `model_routes[].model` value does not override the primary pin. A non-primary target with a configured profile model is pinned to that model, so its route model does not override the profile model either. A non-primary target without a configured model remains unpinned and receives the route model; its `fallback_models` are not materialized, although referenced fallback profiles are still walked. Keep the route model aligned with the target pin when one exists.

## Effort-based local/cloud routing

An `effort_routing` block on a runtime profile can select a local route for simple and ambiguous turns and a cloud route for complex turns. The feature is off unless the block is present. It reuses exact, case-sensitive `[[model_routes]].hint` values, so provider credentials and model IDs remain owned by the existing route and provider-profile configuration.

```toml
[[model_routes]]
hint = "local"
model_provider = "ollama.local"
model = "qwen3:8b"

[[model_routes]]
hint = "cloud"
model_provider = "anthropic.sonnet"
model = "claude-sonnet-4-5"

[runtime_profiles.local_first.effort_routing]
local_hint = "local"
cloud_hint = "cloud"
cloud_escalation = "auto"
```

The first implementation uses ZeroClaw's deterministic complexity estimator. It does not send the prompt to another model for classification. Simple and standard turns select `local_hint`. Complex turns select `cloud_hint` only when `cloud_escalation = "auto"`; the default `"never"` keeps every automatic decision local.

The routing decision is per turn. Logs include the selected target, complexity tier, escalation policy, and message length, but not the message text. A local decision restricts provider attempts to the configured local provider, including stream-to-nonstream recovery, so an availability fallback cannot silently disclose that turn to cloud. A cloud decision permits the configured local and cloud provider references.

Explicit choices take precedence over automatic effort routing. CLI `--provider` or `--model` overrides bypass it. A channel `/model <model-or-hint>` choice and an in-turn `model_switch` remain selected instead of being replaced on the next classifiable turn. Use `/model auto` to clear the sender-session choice and restore configured automatic routing; scoped `/model --user auto` and `/model --agent auto` clear those scoped choices. Structured session provider or model overrides remain explicit for that session.

This slice does not provide an ask-before-cloud mode, escalation-frequency limits, or a separate cost cap. Use `cloud_escalation = "never"` when cloud egress is not allowed, and use provider budgets and normal observability for cost controls.

Effort routing has a deliberately narrow relationship to adjacent work:

- [Issue #5287](https://github.com/zeroclaw-labs/zeroclaw/issues/5287) owns how a local or small model behaves after it is selected.
- [Issue #7539](https://github.com/zeroclaw-labs/zeroclaw/issues/7539) concerns local model-router and discovery work; effort routing consumes configured aliases instead of discovering models.
- [Issue #7431](https://github.com/zeroclaw-labs/zeroclaw/issues/7431) concerns delivery and tool-routing intent, not local-versus-cloud model selection.

## Reliability fallback

A provider profile can declare `fallback_models` for alternate models on the same endpoint and `fallback` for other dotted provider profiles. ZeroClaw materializes `fallback_models` only when the profile has an effective primary model; otherwise that profile contributes one unpinned entry. It then walks fallback profiles depth-first. Each fallback profile keeps its own endpoint, credentials, headers, optional model, and nested fallback declarations.

Effective execution can differ after a rate limit: entries from one profile share a cooldown key, so a `429` on the primary can skip that profile's remaining fallback models while the cooldown is active.

Configure the chain through the ZeroCode Config editor, the dashboard, or `zeroclaw config set`; see [Provider configuration](./configuration.md#fallback-on-failure). The [Provider routing lifecycle](../architecture/provider-routing-lifecycle.md) documents construction, retry classification, streaming recovery, no-replay boundaries, and attribution ownership.

## Runtime model switching

Runtime switches use the same provider-profile contract as config-backed routing:

- `/models <type>.<alias>` selects the active provider profile for the sender session. Channel runtimes can also accept a bare `<type>` shorthand when exactly one configured alias exists for that provider family.
- `/model <model-id>` selects a model within the active provider profile. If the value resolves through a `[[model_routes]]` entry, that route can select a different provider profile. A pinned target serves its effective pin rather than necessarily `model_routes[].model`; an unpinned target without a configured profile model receives the route model.
- The `model_switch` tool uses `model_provider = "<type>.<alias>"` plus `model = "<provider-local-model-id>"`.

Runtime switches are session/runtime state. They do not edit `config.toml`; persisted defaults require an explicit config write. For tool-driven switches, bare provider family names such as `openai` are not switch targets because they do not identify which configured profile, credential, endpoint, or compatibility mode should be used.

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
- [Provider routing lifecycle](../architecture/provider-routing-lifecycle.md): selection, retry, fallback, streaming recovery, and attribution ownership
- [Provider catalog](./catalog.md): every canonical slot
