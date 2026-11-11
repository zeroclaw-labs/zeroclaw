# Routing

ZeroClaw uses routing for two different decisions:

1. **Agent dispatch** selects which agent owns a channel or request. Each agent has its own provider profile and runtime policy.
2. **Provider and model routing** selects an exact-key model for a call, then applies its retry policy and the `[providers.fallback]` graph.

An external routing service such as OpenRouter can still perform vendor selection behind one provider connection. It is optional: ZeroClaw also supports first-party hint routes and the exact-key `[providers.fallback]` graph.

## Per-agent dispatch

Define each routing target as its own agent, then point channels at the agent that should handle their traffic.

Each channel binds to one agent. Channels move between agents by editing `channels = [...]` on the agent that should pick them up; `Config::validate()` makes sure references resolve.

For ad-hoc multi-step routing inside a single conversation, the `spawn_subagent` tool lets an agent run an ephemeral child under its own identity. The child inherits the parent's permissions envelope (see `[risk_profiles.<alias>].allowed_tools`) and returns its final response to the parent's tool loop.

## Hint-based model routes

A narrower mechanism: `[[model_routes]]` maps a task hint to one canonical model reference. Each entry carries only `hint` and exact `model_ref`; the referenced model owns its connection, credentials, upstream model ID, and options. Configure routes through the gateway, zerocode, or `zeroclaw config set`; see the [Config reference](../reference/config.md#model_routes) for the field schema.

Routes normally fire when a prompt explicitly carries the matching hint. An opted-in effort-routing runtime profile can also select two named routes automatically before provider execution.

An unknown `hint:<name>` logs a warning and stays in the default reliability domain while preserving the literal hint as the requested model. Each route target is materialized from its complete canonical model definition, so a switch or fallback receives the target model's own connection, credentials, options, and fallback policy.

`model_ref` is an opaque exact key under `[providers.models]`. Dots are part of the TOML key and are never split into family, alias, or nested model segments.

## Effort-based local/cloud routing

An `effort_routing` block on a runtime profile can select a local route for simple and ambiguous turns and a cloud route for complex turns. The feature is off unless the block is present. It reuses exact, case-sensitive `[[model_routes]].hint` values, so provider credentials and model IDs remain owned by the existing route and provider-profile configuration.

```toml
[[model_routes]]
hint = "local"
model_ref = "chat.local"

[[model_routes]]
hint = "cloud"
model_ref = "chat.cloud"

[runtime_profiles.local_first.effort_routing]
local_hint = "local"
cloud_hint = "cloud"
cloud_escalation = "auto"
```

The first implementation uses ZeroClaw's deterministic complexity estimator. It does not send the prompt to another model for classification. The estimator counts UTF-8 bytes: text under 50 bytes with no reasoning keyword is simple, text over 200 bytes, containing a code fence, or containing at least two English reasoning keywords is complex, and the remainder is standard. Simple and standard turns select `local_hint`. Complex turns select `cloud_hint` only when `cloud_escalation = "auto"`; the default `"never"` keeps every automatic decision local. Channel turns classify the user's pre-enrichment text with attachment markers removed, so fetched link summaries and encoded media bytes do not trigger cloud escalation.

The routing decision is per turn. Logs include the selected target, complexity tier, escalation policy, and message length, but not the message text. A local decision restricts provider attempts to the configured local provider, including stream-to-nonstream recovery, so an availability fallback cannot silently disclose that turn to cloud. A cloud decision permits the configured local and cloud provider references.

Operator choices take precedence over automatic effort routing. CLI `--provider` or `--model` overrides bypass it. A channel `/model <model-or-hint>` choice remains selected instead of being replaced on the next classifiable turn. Use `/model auto` to clear the sender-session choice and restore configured automatic routing; scoped `/model --user auto` and `/model --agent auto` clear those scoped choices. Structured session provider or model overrides remain explicit for that session. A model-issued `model_switch` remains subject to the active effort policy and can select only a provider profile allowed for that turn.

`effort_routing` replaces `auto_classify` and channel `query_classification` for that agent. Configuration validation rejects a runtime profile that sets both `effort_routing` and `auto_classify`. For an agent with `effort_routing`, channel commands reserve `auto` for restoring automatic routing. Agents without `effort_routing` retain the existing model and route selection behavior, including a model or route hint named `auto`.

This slice does not provide an ask-before-cloud mode, escalation-frequency limits, or a separate cost cap. Use `cloud_escalation = "never"` to keep effort-routed model-provider attempts local, and use provider budgets and normal observability for cost controls. This setting does not govern separate network or provider calls made by unrelated tools or media preprocessing.

When the selected provider lacks vision and an image turn needs the configured `vision_model_provider`, that provider must resolve to one of the local or cloud provider references allowed for the turn. The turn fails closed when the dedicated vision provider falls outside that set.

Effort routing has a deliberately narrow relationship to adjacent work:

- [Issue #5287](https://github.com/zeroclaw-labs/zeroclaw/issues/5287) owns how a local or small model behaves after it is selected.
- [Issue #7539](https://github.com/zeroclaw-labs/zeroclaw/issues/7539) concerns local model-router and discovery work; effort routing consumes configured aliases instead of discovering models.
- [Issue #7431](https://github.com/zeroclaw-labs/zeroclaw/issues/7431) concerns delivery and tool-routing intent, not local-versus-cloud model selection.

## Reliability fallback

Fallbacks are declared once in `[providers.fallback]` as an exact-key graph, for example `"chat.primary" = ["chat.backup"]`. Each target is resolved and fully materialized from its own model definition, connection, presets, options, metadata, credentials, and capability policy. The immutable connection registry may be reused during one build, while every candidate receives a fresh model-aware adapter. The graph is walked depth-first with cycle and depth protection.

Effective execution can differ after a rate limit: each exact-key candidate is materialized independently, while cooldown state remains attached to the admitted reliability domain.

Configure the chain through the ZeroCode Config editor, the dashboard, or `zeroclaw config set`; see [Provider configuration](./configuration.md#fallback-on-failure). The [Provider routing lifecycle](../architecture/provider-routing-lifecycle.md) documents construction, retry classification, streaming recovery, no-replay boundaries, and attribution ownership.

## Runtime model switching

Runtime switches use the same provider-profile contract as config-backed routing:

- `/models <type>.<alias>` selects the active provider profile for the sender session. Channel runtimes can also accept a bare `<type>` shorthand when exactly one configured alias exists for that provider family.
- `/model <model-or-hint>` selects an active model or a declared route hint. A route resolves through its exact `model_ref`, so the selected connection, credentials, upstream model ID, and options are taken from that canonical model definition.
- The `model_switch` tool keeps the legacy provider/model arguments for runtime session overrides. Persisted `[[model_routes]]` entries use only `hint` plus exact `model_ref`; they do not duplicate provider credentials or an upstream model ID.

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
