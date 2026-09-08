# Provider routing lifecycle

Provider routing begins after ZeroClaw has selected the agent that owns a turn. It covers provider-profile and model selection, retries and fallback, stream recovery, and the attribution that explains which backend served the request. Channel-to-agent dispatch is a separate lifecycle; see [Channel runtime lifecycle](./channel-runtime-lifecycle.md).

Use this page when a change touches `model_routes`, session or in-turn model selection, provider fallback, retry classification, rate-limit cooldowns, stream completion, replay after a stream failure, or requested-versus-served provider attribution.

## Ownership map

| Concern | Current owner | Contract |
| --- | --- | --- |
| Provider profiles and fallback graph | `zeroclaw-config` provider schema and validation | A dotted `<family>.<alias>` identifies the endpoint, credentials, optional primary model, capabilities, and ordered fallback declarations for one profile. |
| Provider construction | `zeroclaw-providers` factory functions | Materialize each profile with its own settings, flatten configured fallback entries in order, and compose routing around reliability. |
| Hint-based selection | `RouterModelProvider` | Resolve `hint:<name>` to a configured provider target and route model. The primary target is pinned to the active/default model. A non-primary target is pinned when its profile configures a model; otherwise its reliable entry remains unpinned and receives the route model. |
| Retry and failover | `ReliableModelProvider` | Classify failures, retry with bounded backoff, honor rate-limit cooldowns, and advance through the materialized entries. |
| Provider stream termination | Concrete providers and `zeroclaw-providers/src/stream_guard.rs` | Translate each provider protocol's completion semantics into `StreamEvent::Final` or a truncation error. |
| Stream replay and partial-output commitment | `zeroclaw-runtime/src/agent/turn/provider_call.rs` and `stream_consume.rs` | Retry a failed stream as non-streaming only before immutable event output is committed. Never replay a cancelled or visibly partial response. |
| Per-call attribution | `ProviderDispatch` | Open attribution scopes around the provider call selected for each attempt. |
| Successful recovery record | `ReliableModelProvider` | Expose one task-local requested-versus-served record after a successful recovery. This is not canonical per-attempt accounting. |
| User-facing recovery notices | Runtime and channel consumers | Render the successful recovery record for their own output surface. Notice rules are not uniform across consumers. |

## Construction and selection

The runtime starts with an active provider reference and model from the selected agent, a session override, or an in-turn `model_switch`. Provider construction then composes two wrappers:

1. The factory builds a `ReliableModelProvider` for the active provider profile. An effective primary model comes from an explicit construction override or the profile's configured `model`. When one exists, it and the profile's `fallback_models` become pinned entries. Without one, the profile contributes one unpinned entry and its `fallback_models` are not materialized. Recursively referenced `fallback` profiles are still walked. Each referenced profile keeps its own credentials, endpoint, headers, model, and capability overrides.
2. When `model_routes` are configured, the factory builds a separate reliable provider for the primary route and each unique route target, then wraps them in `RouterModelProvider`.
3. A recognized `hint:<name>` selects its configured target before the call enters that target's reliability policy. A normal model value uses the default route. An unknown hint logs a warning, stays in the default reliability domain, and preserves the literal `hint:<name>` as the requested model. A pinned default entry still serves its pin; an unpinned default entry forwards the literal value and the provider may reject it before normal fallback or error handling continues.

There are two current construction constraints:

- Route pinning is conditional. The primary target is pinned to the active/default model passed into provider construction, including when a recognized hint points back to the active primary profile; that hint's `model_routes[].model` value does not override the primary pin. A non-primary target with a configured profile model is pinned to that model, so its route model does not override the profile model either. A non-primary target without a configured model is valid and remains unpinned; the route model reaches that provider, and that profile's `fallback_models` are not materialized even though its referenced fallback profiles are still walked. Keep each route model aligned with the target pin when one exists, and account for the unpinned behavior when the target profile omits `model`.
- Route targets are deduplicated by `model_provider`. If a route supplies `api_key`, the first matching route credential takes precedence when the shared target is constructed. Prefer credentials on the provider profile when several hints share one target.

This ordering matters: routing chooses a reliability domain; it does not bypass reliability. An external routing service such as OpenRouter can still perform server-side selection behind one ZeroClaw profile, but it is optional and does not replace ZeroClaw's first-party route and fallback contracts.

The operator-facing schema and examples live in [Provider configuration](../providers/configuration.md) and [Routing](../providers/routing.md). Keep field syntax there instead of duplicating it in architecture documents.

## Non-streaming attempt order

For a production alias, the factory flattens the configured graph depth-first. The effective order is:

1. The profile's effective primary model, or one unpinned entry when no effective primary model exists.
2. That profile's `fallback_models`, in order, only when an effective primary model exists.
3. Each `fallback` profile, in order, including that profile's own primary or unpinned entry, eligible fallback models, and nested fallback profiles.

For each materialized entry, `ReliableModelProvider` attempts the request up to `provider_retries + 1` times. A retryable error normally stays on the entry and applies bounded backoff. A retryable rate limit places that provider profile on an in-memory cooldown and advances when another entry exists. Most non-retryable errors advance immediately; context-window errors have method-specific handling and can return early for runtime recovery. A successful response ends the walk; if every entry fails, the wrapper returns an aggregated error with the attempt failures.

Materialization order and effective execution order can differ after a rate limit. A profile's primary and `fallback_models` entries share one cooldown key, so a `429` on the primary can cause the remaining same-profile models to be skipped while the cooldown is active.

The global `reliability.api_keys` pool is not a working failover mechanism today. The wrapper selects and logs an alternate key after a retryable rate limit, but the `ModelProvider` trait cannot apply that key to the constructed provider, so the retry still uses the original credential. [Issue #9190](https://github.com/zeroclaw-labs/zeroclaw/issues/9190) tracks the repair. Use distinct fallback profiles or an external routing service when credential-level failover is required.

Empty completions receive the same bounded retry treatment instead of immediately becoming a blank assistant turn.

Invalid fallback declarations have two different boundaries. Dangling references, cycles, over-depth edges, blank model IDs, and duplicate primary models are reported and pruned as described in [Provider configuration](../providers/configuration.md#misconfiguration). A fallback profile that resolves but cannot supply its required credential or cannot be constructed fails provider initialization rather than silently changing the route.

## Streaming and replay boundary

Streaming deliberately has a narrower retry contract than non-streaming calls:

1. `ReliableModelProvider` chooses the first ordered entry that supports the requested stream capabilities and is not cooling down.
2. It opens that stream once. It does not switch entries after the stream has started.
3. The concrete provider parser translates its protocol's completion semantics into `Final` or an error. Most guarded SSE parsers require their configured completion signal. Anthropic currently also treats EOF after a non-empty `message_delta.stop_reason` as complete even if `message_stop` was not observed; [PR #9447](https://github.com/zeroclaw-labs/zeroclaw/pull/9447) proposes requiring `message_stop`, but that change is not landed.
4. The runtime consumes and sanitizes stream events. If the stream fails before immutable event output is visible, the runtime retries the whole call through the non-streaming path, which re-enters the full reliability walk.
5. If text, reasoning, or pre-executed tool events have already reached an immutable event sink, interruption becomes `StreamInterruptedAfterOutput`. The runtime does not replay the request. Only text already forwarded to the consumer becomes persisted partial assistant text.
6. Cancellation never becomes an automatic provider retry. Cancellation before forwarded text aborts the turn. Cancellation after forwarded text preserves that partial assistant text; reasoning-only or pre-executed-tool output does not by itself become persisted partial assistant text on cancellation.

Draft-update sinks are mutable. A pre-commit fallback may replace a draft without duplicating immutable output, while event sinks define the no-replay boundary.

A stream that completes with no final text or tool calls is a semantic-empty response, not a successful answer. When runtime marks that result replay-safe and `provider_retries` is nonzero, Reliable allows one non-streaming recovery call to the exact provider/model that produced the empty stream. That allowance is consumed once; a failed recovery advances to the remaining configured candidates with their normal retry budgets. With zero retries, the failed stream entry remains skipped. Reasoning already shown stays visible once, but does not count as a final answer. This exception does not authorize replay after cancellation, interrupted visible output, or provider-executed tool work.

This division keeps transport recovery in the runtime, provider-specific framing in the adapter, and retry/fallback policy in the reliability wrapper. A provider implementation should not invent a second turn-level replay policy.

## Attribution and known gaps

`ProviderDispatch` opens attribution around each provider call. `ReliableModelProvider` records a requested-versus-served fallback only after a non-streaming call succeeds or a fallback stream completes without error. Runtime and channel code can consume that task-local record to tell a user that recovery occurred.

The record is only a family/model recovery hint. Production entries use the provider family as `display_name`, so the record can lose the dotted profile alias. A same-family, same-model fallback between aliases may therefore be indistinguishable from the requested route. Runtime responses append a model/provider fallback notice when the record differs. Channel delivery adds a footer only for a cross-family change; [issue #7883](https://github.com/zeroclaw-labs/zeroclaw/issues/7883) tracks intra-family notices.

That record is a success notice, not a canonical ledger of every attempt. [Issue #9470](https://github.com/zeroclaw-labs/zeroclaw/issues/9470) tracks incorrect usage and cost attribution across rejected attempts and stale fallback notices after stream recovery. Until that issue is resolved, do not infer per-attempt cost accuracy from the final fallback notice or requested provider identity.

Content refusal and safeguard fallback is also a separate proposed contract from transport reliability. [Tracker #9293](https://github.com/zeroclaw-labs/zeroclaw/issues/9293) coordinates that work across provider, configuration, channel, gateway, and web surfaces. Adjacent serving-identity work proposed in [PR #8966](https://github.com/zeroclaw-labs/zeroclaw/pull/8966) does not by itself close the Reliable attribution gap.

## Change checklist

For provider-routing changes, answer these before reviewer sign-off:

- Does the change affect agent dispatch, hint selection, reliability fallback, or an external router? Name exactly one owner for each decision.
- If a hint targets any provider profile, does its model handling match the target construction? Compare the primary target with the active/default pin. For a non-primary target with a configured model, compare the route model with that pin. If the profile omits `model`, confirm that the route model should flow through and that the profile's `fallback_models` will not be materialized.
- Does every fallback profile retain its own endpoint, credentials, model, headers, and capability overrides?
- What is retryable, what advances immediately, and what error is returned after exhaustion?
- Can a request be replayed after any output that a user or immutable consumer already observed?
- What exact signal does each provider parser accept as completion? Does EOF before that signal fail as truncated?
- Are requested and served provider/model identities preserved separately?
- Are usage, cost, logs, and user notices derived from the same serving attempt, or is the limitation explicitly tracked?
- Do streaming and non-streaming tests cover the same failure boundary where the behavior is intended to match?

## Source pointers

- Provider trait and stream events: `crates/zeroclaw-api/src/model_provider.rs`
- Route selection: `crates/zeroclaw-providers/src/router.rs`
- Profile model pinning: `crates/zeroclaw-providers/src/model_pin.rs`
- Retry, cooldown, fallback, and fallback notices: `crates/zeroclaw-providers/src/reliable.rs`
- Provider construction and fallback-graph materialization: `crates/zeroclaw-providers/src/lib.rs`, `crates/zeroclaw-providers/src/factory.rs`
- Provider stream completion guard: `crates/zeroclaw-providers/src/stream_guard.rs`
- Runtime stream replay and partial-output handling: `crates/zeroclaw-runtime/src/agent/turn/provider_call.rs`, `crates/zeroclaw-runtime/src/agent/turn/stream_consume.rs`
- Operator guides: [Provider configuration](../providers/configuration.md), [Routing](../providers/routing.md), [Streaming](../providers/streaming.md)
