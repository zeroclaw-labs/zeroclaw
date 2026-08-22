# Multi-Model Setup

A walkthrough of the common patterns for using multiple model providers: per-agent dispatch, hint routes, cost tiering, local-first with hosted backup, non-streaming provider fallback, rate-limit handling, and streaming recovery.

> **Reference material** for the provider system lives in:
> - [Model Providers → Overview](../providers/overview.md): what providers are, configuration shape
> - [Model Providers → Routing](../providers/routing.md): agent dispatch, hint routes, and provider fallback
> - [Model Providers → Catalog](../providers/catalog.md): every provider's config shape

## When to use multi-model setup

Multi-model configuration is useful for:

1. **Cost tiering**: cheap model handles high-volume channels; reasoning model handles complex requests
2. **Capability routing**: vision-capable model for image-bearing channels, reasoning model for research workflows
3. **Local-first development**: local Ollama for development, hosted endpoint for production
4. **Per-team isolation**: different teams use different agents with different model_providers and credentials
5. **Non-streaming rate-limit handling**: move to another configured provider profile after a retryable `429`

## Core idea: per-agent dispatch

Each `[agents.<alias>]` entry starts from one `[providers.models.<type>.<alias>]`. That provider profile can declare alternate models with `fallback_models` and other provider profiles with `fallback`. See [Routing](../providers/routing.md) for the full pattern.

To run multiple models, run multiple agents, each binding to one model provider. Each channel binds to one agent at a time. To move a channel to a different agent, edit the `channels` list on the agent that should pick it up; `Config::validate()` makes sure references resolve at startup.

## Cross-provider reliability

For non-streaming calls, ZeroClaw can walk an ordered fallback graph across provider profiles. Each fallback profile keeps its own endpoint, credentials, model, headers, capability overrides, and nested fallback declarations. The runtime retries or advances according to the error classification and profile cooldown state.

OpenRouter remains a first-class provider and can perform vendor selection behind one endpoint. It is an optional external routing layer, not a requirement for ZeroClaw's first-party fallback.

## Non-streaming retry and fallback

For transient errors such as a network failure, `503`, or timeout, a non-streaming call retries with bounded exponential backoff, configurable globally under `reliability` (defaults: 2 retries, 500 ms initial backoff). After an entry is exhausted, the reliable wrapper advances through the profile's `fallback_models` and fallback profiles.

## Streaming recovery boundary

A streaming call selects the first eligible, non-cooling entry that supports the required stream capabilities. It does not advance to another entry after that stream starts. If the stream fails before visible output reaches an immutable consumer, the runtime retries the whole call through the non-streaming path, which can walk the fallback graph. Once visible output exists, the runtime preserves the partial response and does not replay the request or switch providers. See [Provider routing lifecycle](../architecture/provider-routing-lifecycle.md#streaming-and-replay-boundary) for the complete contract.

## API key rotation limitation

Do not rely on `reliability.api_keys` for credential failover. On a retryable rate limit, the reliable wrapper selects and logs an alternate key, but the `ModelProvider` trait cannot apply it to the already constructed provider. The retry still uses the original credential. [Issue #9190](https://github.com/zeroclaw-labs/zeroclaw/issues/9190) tracks this limitation.

Use separate provider profiles with their own credentials, or an external routing service, when credential-level failover is required.

## Local development with hosted alternative

Run a local-Ollama agent and a hosted-provider agent side by side; route each channel to whichever you want it to use.

The `dev` agent runs from the CLI (no channel binding required, `zeroclaw agent -a dev` is enough). When Ollama is down, the dev agent fails fast and surfaces the error. The prod channels are unaffected.

## Local-small no-text-fallback profile

Small local models usually need a runtime profile, not a provider-specific mode. Keep the Ollama provider focused on connection details, then use `[runtime_profiles.<alias>]` to tighten the prompt/tool loop behavior. ZeroClaw exposes a built-in `local_small` runtime preset for code paths that install runtime presets directly. If you edit config by hand, use this equivalent block:

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
agentic                 = true
compact_context          = true
strict_tool_parsing      = true
max_tool_iterations      = 4
max_actions_per_hour     = 10
max_cost_per_day_cents   = 100
shell_timeout_secs       = 30
max_delegation_depth     = 1
delegation_timeout_secs  = 60
agentic_timeout_secs     = 120
max_history_messages     = 20
max_context_tokens       = 8000
parallel_tools           = false
max_system_prompt_chars  = 4000
max_tool_result_chars    = 4000
keep_tool_context_turns  = 1
memory_recall_limit      = 3
```

This profile composes existing primitives:

- `compact_context` keeps startup context small.
- `strict_tool_parsing` treats XML/JSON-looking fallback text as assistant text unless the provider returns native tool calls.
- `max_tool_iterations`, `max_context_tokens`, `max_system_prompt_chars`, and `max_tool_result_chars` bound runaway loops and oversized prompt/tool context.
- `max_actions_per_hour`, `max_cost_per_day_cents`, and the timeout/delegation fields keep local runs on the same budget shape as the built-in preset.
- `parallel_tools = false` and `keep_tool_context_turns = 1` keep local runs sequential and limit retained tool context.

With Ollama, this is a no-text-fallback profile: authorized tools remain configured in `risk_profile`, but text-form tool markup from the model is not executed. Use it for chat-first local agents, or for providers that return native/structured tool calls. If a local model must use ZeroClaw's text fallback tool syntax, set `strict_tool_parsing = false` and keep the other small-model limits.

## Cost tiering: heavy model when needed, fast model otherwise

Run two agents and route channels to the appropriate tier. The `delegate` tool lets one agent hand off to another mid-conversation. [Delegation](../agents/delegation.md) is gated: the caller's risk profile must set `delegation_policy mode = "allow"`, and the target must be reachable from the caller (a same-profile peer, or an explicit entry in the caller's `delegates` list). The frontline and heavy agents below run on the *same* `trusted` risk profile, so they reach each other as same-profile peers; they differ in model and runtime profile (iteration budget), not in trust surface.

The frontline agent handles every inbound message on Haiku. When it needs deeper reasoning, it calls the `delegate` tool with `agent = "heavy"`; because both agents share the `trusted` risk profile and that profile allows delegation, the heavier agent picks up the sub-task on Opus.

## Non-streaming error handling

For non-streaming calls, retryable failures include:

1. **Timeout**: provider did not respond within the configured timeout
2. **Connection error**: network or DNS failure
3. **Rate limit (429)**: places the provider profile on a temporary in-memory cooldown and advances when another entry exists
4. **Service unavailable (503)**: temporary service issue

Retries are NOT triggered by:

1. **Invalid request (400)**: malformed input; retrying won't help
2. **Permanent auth failure**: invalid API key format
3. **Model output errors**: the model responded but returned an error payload

When every materialized entry is exhausted or cooling down, the failure surfaces to the calling channel with the collected attempt failures.

## Debugging

Persisted logs (`"rolling"` is the default) capture retry, cooldown, and fallback behavior. Then query traces:

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
2. **Give fallback profiles explicit ownership.** Keep each endpoint, credential, model, and capability override on the profile that serves it.
3. **Treat OpenRouter as an optional routing layer.** Use it when server-side vendor selection is useful; use ZeroClaw fallback profiles when the runtime should own the order.
4. **Do not rely on `reliability.api_keys`.** Use separately constructed profiles until [issue #9190](https://github.com/zeroclaw-labs/zeroclaw/issues/9190) is fixed.
5. **Smoke-test each agent in isolation.** `zeroclaw agent -a <alias>` runs an agent without channel plumbing in the way.
6. **Document agent intent.** Add `# comment` lines explaining which channels each agent serves and why.
7. **Inject secrets via env, not inline.** `ZEROCLAW_providers__models__<type>__<alias>__api_key=...` sets `api_key` at startup; see [Environment variables](../reference/env-vars.md).
8. **Separate dev and prod agents.** Each environment gets its own `[agents.<alias>]` entry bound to its own channels.

## Credential resolution

Each provider entry resolves credentials in this order:

1. **Inline `api_key`** on the provider entry.
2. **Secrets store** at `~/.zeroclaw/secrets`.
3. **Generic env override**: `ZEROCLAW_providers__models__<type>__<alias>__api_key=...` at startup. If your shell already exports `ANTHROPIC_API_KEY`, `OPENROUTER_API_KEY`, or a similar vendor-default name, bridge it into this schema-mirror variable before startup unless the provider family explicitly documents a native runtime env bridge. See [Environment variables](../reference/env-vars.md) for the full grammar and bridge examples.

Credentials are not shared between provider profiles; set them per profile. A route-level `model_routes[].api_key` is a higher-precedence override when its routed target is constructed. Route targets are deduplicated by `model_provider`, so the first matching route credential can construct the provider shared by several hints. Prefer profile-owned credentials when routes share a target.

## Related Documentation

- [Model Providers → Overview](../providers/overview.md)
- [Model Providers → Routing](../providers/routing.md)
- [Environment variables](../reference/env-vars.md)
