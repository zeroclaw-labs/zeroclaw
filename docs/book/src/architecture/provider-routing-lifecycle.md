# Provider routing lifecycle

Provider routing begins after ZeroClaw has selected the agent that owns a turn. It covers provider-profile and model selection, retries and fallback, stream recovery, and the attribution that explains which backend served the request. Channel-to-agent dispatch is a separate lifecycle; see [Channel runtime lifecycle](./channel-runtime-lifecycle.md).

Use this page when a change touches `model_routes`, session or in-turn model selection, provider fallback, retry classification, rate-limit cooldowns, stream completion, replay after a stream failure, or requested-versus-served provider attribution.

## Ownership map

| Concern | Current owner | Contract |
| --- | --- | --- |
| Provider profiles and fallback graph | `zeroclaw-config` provider schema and validation | A dotted `<family>.<alias>` identifies the endpoint, credentials, optional primary model, capabilities, and ordered fallback declarations for one profile. |
| Provider construction | `zeroclaw-providers` factory functions | `zeroclaw-providers::model::build_model` derives the selected model, tuning, connection and identity from config; runtime adds its tool dispatcher. Materialize each profile with its own settings, flatten configured fallback entries in order, and compose routing around reliability. |
| Hint-based selection | `RouterModelProvider` | Resolve `hint:<name>` to a configured provider target and route model. The primary target is pinned to the active/default model. A non-primary target is pinned when its resolved selection supplies a model, including a default or sole nested entry; otherwise its reliable entry remains unpinned and receives the route model. |
| Effort-based selection | Resolved runtime profile plus agent/channel turn entrypoints | Deterministically classify an opted-in turn, select one of two existing route hints, preserve explicit model choices, and constrain eligible provider attempts for the turn. |
| Retry and failover | `ReliableModelProvider` | Classify failures, retry with bounded backoff, honor rate-limit cooldowns, and advance through the materialized entries. |
| Provider stream termination | Concrete providers and `zeroclaw-providers/src/stream_guard.rs` | Translate each provider protocol's completion semantics into `StreamEvent::Final` or a truncation error. |
| Stream replay and partial-output commitment | `zeroclaw-runtime/src/agent/turn/provider_call.rs` and `stream_consume.rs` | Retry a failed stream as non-streaming only before immutable event output is committed. Never replay a cancelled or visibly partial response. |
| Per-call attribution | `ProviderDispatch` | Open attribution scopes around the provider call selected for each attempt. |
| Successful recovery record | `ReliableModelProvider` | Expose one task-local requested-versus-served record after a successful recovery. In event-instrumented turn paths, the `usage_by_provider` ledger records billable attempts through `TurnEvent::Usage` with serving identity and an `accepted` flag. |
| User-facing recovery notices | Runtime and channel consumers | Render the successful recovery record for their own output surface. Notice rules are not uniform across consumers. |

## Construction and selection

The runtime starts with an active provider reference and model from the selected agent, a session override, or an in-turn `model_switch`. Provider construction then composes two wrappers:

1. The factory builds a `ReliableModelProvider` for the active provider profile. An effective primary model comes from an explicit construction override or the resolved model selection, including an implicit `models.default` or sole nested entry. The same selection supplies the model-option overlay and the model pin. When a model exists, it and the profile's `fallback_models` become pinned entries. Without one, the profile contributes one unpinned entry and its `fallback_models` are not materialized. Recursively referenced `fallback` profiles are still walked. Each referenced profile keeps its own credentials, endpoint, headers, model, and capability overrides.
2. When `model_routes` are configured, the factory builds a separate reliable provider for the primary route and each unique route target, then wraps them in `RouterModelProvider`.
3. A recognized `hint:<name>` selects its configured target before the call enters that target's reliability policy. A normal model value uses the default route. An unknown hint logs a warning, stays in the default reliability domain, and preserves the literal `hint:<name>` as the requested model. A pinned default entry still serves its pin; an unpinned default entry forwards the literal value and the provider may reject it before normal fallback or error handling continues.

There are two current construction constraints:

- Route pinning is conditional. The primary target is pinned to the active/default model passed into provider construction, including when a recognized hint points back to the active primary profile; that hint's `model_routes[].model` value does not override the primary pin. A non-primary target is pinned to its resolved selection's model, including an implicit default or sole nested entry, so its route model does not override that pin either. A non-primary target whose resolved selection has no effective model is valid and remains unpinned; the route model reaches that provider, and that profile's `fallback_models` are not materialized even though its referenced fallback profiles are still walked. Keep each route model aligned with the target pin when one exists. An omitted profile `model` alone does not imply unpinned behavior when a nested entry supplies the model.
- Route targets are deduplicated by `model_provider`. If a route supplies `api_key`, the first matching route credential takes precedence when the shared target is constructed. Prefer credentials on the provider profile when several hints share one target.

This ordering matters: routing chooses a reliability domain; it does not bypass reliability. An external routing service such as OpenRouter can still perform server-side selection behind one ZeroClaw profile, but it is optional and does not replace ZeroClaw's first-party route and fallback contracts.

When a resolved runtime profile has `effort_routing`, the turn entrypoint runs the deterministic complexity estimator before provider execution. Simple and standard turns select the configured local hint. A complex turn selects the cloud hint only under the explicit `auto` escalation policy. No model call is made for classification, and decision telemetry excludes prompt text. Explicit operator CLI, session, and channel selections disable this automatic step at their applicable scope; channel `/model auto` restores it. A model-issued `model_switch` changes the active provider/model without changing whether the route is automatic or operator-owned, so automatic effort routing remains active after a model-issued switch unless an operator override already disabled it.

The provider boundary enforces the decision as well as selecting a model. Structured agent and channel paths install a task-local allowlist that every Reliable candidate loop checks, including non-streaming, streaming, and stream-recovery paths. Direct CLI and daemon entrypoints construct their turn router from only the allowed route targets. A local decision therefore cannot cross into the configured cloud provider through availability fallback. A cloud decision can use the configured local and cloud provider references.

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

A stream that completes with no final text or tool calls is a semantic-empty response, not a successful answer. When runtime marks that result replay-safe and `provider_retries` is nonzero, Reliable allows one non-streaming recovery call to the exact provider/model that produced the empty stream. That allowance is consumed once; a failed recovery advances to the remaining configured candidates with their normal retry budgets. With zero retries, the failed stream entry remains skipped, unless it is the only candidate, in which case one non-streaming recovery attempt of the same entry is allowed (recovery, not replay). That single-candidate allowance is also consumed once; when both allowances apply to the same entry they merge into one single attempt. Reasoning already shown stays visible once, but does not count as a final answer. This exception does not authorize replay after cancellation, interrupted visible output, or provider-executed tool work.

This division keeps transport recovery in the runtime, provider-specific framing in the adapter, and retry/fallback policy in the reliability wrapper. A provider implementation should not invent a second turn-level replay policy.

## Attribution and known gaps

`ProviderDispatch` opens attribution around each provider call. `ReliableModelProvider` records a requested-versus-served fallback only after a non-streaming call succeeds or a fallback stream completes without error. Runtime and channel code can consume that task-local record to tell a user that recovery occurred.

The record is only a family/model recovery hint. Production entries use the provider family as `display_name`, so the record can lose the dotted profile alias. A same-family, same-model fallback between aliases may therefore be indistinguishable from the requested route. Runtime responses append a model/provider fallback notice when the record differs. Channel delivery preserves the cross-family footer and uses `channels.model_fallback_notice` for ordinary same-family fallback to a different model. The global modes are `off` by default, `redacted` without routing identifiers, and `detailed` with requested and served provider/model identifiers. See [Model fallback notices](../channels/overview.md#model-fallback-notices) for configuration and audience privacy tradeoffs.

The channel orchestrator resolves this policy from the current live `Config` at outbound delivery rather than snapshotting it into a long-lived channel handle. Existing config publication and reload paths apply changes to a running daemon. The notice decorates outbound content after the assistant response enters history, so it does not become prompt history or change the serving-attempt record. Safeguard notices retain their separate behavior.

In event-instrumented turn paths, per-attempt accounting lives in the `usage_by_provider` ledger, not in that record. Every billable attempt in those paths, accepted or rejected, emits `TurnEvent::Usage` carrying its serving `provider_ref`/`model` and an `accepted` flag; the gateway done frame derives `cost_usd` as the sum of the ledger. Standalone one-shot queries without an event sink, including skill reflection, may be cost-tracked without entering this ledger. This resolves [issue #9470](https://github.com/zeroclaw-labs/zeroclaw/issues/9470) (incorrect usage and cost attribution across rejected attempts and stale fallback notices after stream recovery). Do not infer per-attempt cost accuracy from the final fallback notice or requested provider identity alone; read the ledger.

Content refusal and safeguard fallback is also a separate proposed contract from transport reliability. [Tracker #9293](https://github.com/zeroclaw-labs/zeroclaw/issues/9293) coordinates that work across provider, configuration, channel, gateway, and web surfaces. The serving-identity and per-attempt ledger work in [PR #8966](https://github.com/zeroclaw-labs/zeroclaw/pull/8966) closes the Reliable cost-attribution gap; content-refusal fallback remains tracked separately.

## Change checklist

For provider-routing changes, answer these before reviewer sign-off:

- Does the change affect agent dispatch, hint selection, reliability fallback, or an external router? Name exactly one owner for each decision.
- If a hint targets any provider profile, does its model handling match the target construction? Compare the primary target with the active/default pin. For a non-primary target whose resolved selection supplies a model, including a default or sole nested entry, compare the route model with that pin. Only when that selection has no effective model should the route model flow through without materializing the profile's `fallback_models`.
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

### Named selections and capacity

A runtime keeps the existing `ModelIdentity` as the selected entry identity. Capacity lookups use its full reference, so entries sharing the same provider-local model ID can still have different context windows. Hint routes retain their configured references. Reliable dispatch may attribute the accepted primary to the shared profile for billing; that normalization does not replace the selected entry's capacity. A fallback to another profile or model resolves its own capacity. Accepted-call reports carry the construction selection separately from the physical billing identity, including on streams, so a cross-profile fallback does not infer an entry from a shared wire model ID. Capacity is resolved from the admitted Config generation at consumption time.

The kernel-only CLI also constructs through `zeroclaw_providers::model::build_model`; provider and model arguments are invocation inputs and do not mutate profile or nested-entry configuration. Dotted aliases use the same resolver as the full runtime. The legacy bare-family CLI override inherits the agent's configured model (including a selected nested entry), key and endpoint; an explicit model argument replaces only the model ID. Agent-aware capacity lookups resolve the inherited selection from Config when its model ID matches the served model, preserving the bare family's billing identity. A different explicit model ID uses the compatibility capacity. A dotted profile override instead resolves its own connection. Unknown named model entries fail construction rather than silently using a profile model; dangling fallback entries are pruned under the existing fallback contract. Delegated requests derive model and temperature together from the target's selected entry for both ordinary and agentic execution.

### Configuration and invocation sampling

`ModelSelection` is the borrowed config view for model defaults. Model construction returns an encapsulated `BuiltModel` binding; its model and profile names derive from `ModelIdentity`. `build_model` assembles routing and reliability, while `build_raw_provider` and `build_reliable_provider` preserve consumers that own their routing or make a single raw call. Their `ProviderOverrides` argument carries only genuine invocation overrides; consumers do not independently copy credentials or rebuild model-option overlays. Runtime alone adds the tool dispatcher. The model-option overlay destructures all model fields exhaustively, requiring an explicit inheritance decision when the schema grows.

Initial Agent construction accepts model overrides before building its provider, so an explicit model does not require a configured default. Session refresh builds a sealed `PreparedModelUpdate` from one admitted configuration generation and installs that generation together with the complete runtime binding. Refresh dependency checks traverse the canonical fallback graph and routed selections on demand, using both old and new configurations for changed or removed profiles. Shared construction inputs (provider retries, backoff and key pool; runtime reasoning defaults; credential-store encryption; and multimodal policy) rebuild all live RPC bindings through the same transaction before a config write is acknowledged. Existing turns keep their admitted generation until completion. No parallel dependency registry is retained. An equivalent in-turn model switch is a successful no-op; returning from a classified hint to the primary selection still resets the selector. Model-dependent tools resolve through the Agent's admitted configuration cell; independent live security policy keeps its own authority.

Configured Agent callers use `AgentBuildContext` for invocation and transport capabilities. The runner accepts an `AgentRunRequest` and an admitted `Arc<Config>`; its heap boundary is established before callers compose task-local scopes, and polling remains in the caller task. Compatibility overloads delegate to these entries. Delegate registration supplies the canonical configuration plus execution capabilities through `DelegateBuildContext`; it does not reconstruct parallel provider, agent, runtime, risk or skill maps. Delegated capacity comes from the same target generation as its provider and sampling.

Agent-scoped refresh excludes sessions owned by other agents before acquiring their update guards. Provider and shared scopes remain conservative until the installed active selection is read under the guard, because an in-turn switch can differ from session overrides. Channel reload uses the canonical configuration filename rather than reconstructing `config.toml` from provider options.

Model-routing mutations acquire the shared config write gate without waiting, then probe a privately hydrated candidate before saving it. A busy gate produces a retryable result: a running RPC turn already holds its session guard, and a config writer can hold the gate while waiting for that turn. Construction errors and permanent request failures reject the candidate; transient request failures permit saving with an explicit deferred outcome. Uncredentialed hosted profiles without an explicit endpoint or external authentication return an explicit skipped outcome. Catalog and pricing construction also use `build_nonblocking`; async listing remains on the normal async path. Live pricing covers each opted-in profile's implicit selected model, including `models.default` or a sole nested entry, rather than every declared nested model.

After an in-turn switch, refresh derives the active selection from the installed Agent identity and admitted configuration. Changing only a session's model or temperature preserves that active provider. Configured model defaults are resolved again during rebuilding. A switched model ID that differs from its admitted selection's default remains pinned; an equivalent switch is a no-op and continues to inherit default edits. Existing explicit session model overrides retain their own provenance. A new explicit provider selection in a `session/configure` request takes precedence over the active in-turn selection.

Long-lived channel tool registries resolve model configuration through an immutable turn-admission scope keyed by the owning agent. Concurrent turns retain separate scopes. Nested turns add their own admission while borrowed parent tools retain their parent's generation; spawned pipeline and bounded-delegate workers explicitly inherit this context. The scope carries existing admitted Config Arcs and does not publish or cache live configuration.

Invocation temperature and thinking adjustment travel separately from configured defaults. `zeroclaw-api::model_provider::CallSampling` is a configuration-independent explicit call input: `Explicit(None)` omits temperature, `Explicit(Some(value))` overrides it, and a per-selection resolver opts into configured defaults. The existing scalar trait methods always retain their omit/override meaning. Configured consumers use the corresponding `*_with_sampling` methods; no ambient task-local policy changes scalar calls. `Temperature::for_call` captures the admitted configuration Arc for the lifetime of a call or stream, and turn iterations share one admitted generation. Reliable retains each candidate's construction reference, including its nested model entry, and resolves sampling at the attempt boundary. Router, model-pin, vision and dispatch wrappers preserve this explicit input.

Per-call native tool-round policy travels alongside sampling through the combined chat and stream methods. Composite providers forward both controls; a physical attempt resolves its own sampling without losing the selected tool-round policy. Effort routes retain their full model reference for construction and capacity, while the attempt allowlist derives connection profiles through canonical config lookup. Nested selections and dotted aliases therefore share the owning profile's permission boundary. Classifier calls, visual fallback and model switches observe that same boundary.

Kernel-only CLI chat, channel classification and consolidation, skill reflection, delegation, direct vision calls, and graceful summaries use their own selected route and admitted configuration. A separately configured classifier with no temperature leaves sampling unset rather than inheriting the main agent's value. A derived entrance temperature never becomes an invocation override. Explicit invocation overrides continue across retry and fallback candidates and model switches. Reverting the integration restores the previous sampling behavior; no persisted config migration is involved.

SOP `llm.generate` holds a resolver over the canonical live-config handle rather than a provider or sampling snapshot. Each call reads one published Config, resolves the default agent's current selection, and constructs through `build_model`. The adapter uses `build_nonblocking` for construction and passes sampling from that same generation explicitly. This shared construction adapter also protects async Agent, channel, tool, gateway boot, doctor catalog and raw vision-route callers from synchronous OAuth exchanges that use a blocking HTTP client. Published updates to model identity, endpoint, credentials, and model options apply on the next SOP call without recreating the engine. Invalid selections fail the call rather than reusing the previous provider. Reverting this integration restores construction-time SOP settings; no persisted config migration is involved.

Hailo-Ollama materializes the selected model's context window through the common construction-option overlay for its existing local input budget. Rebuilding a binding rematerializes this projection; Config remains canonical and the window is not sent as a wire `num_ctx` override.

Deterministic response-cache keys include the full selected reference and a SHA-256 digest of the canonical model-provider configuration and shared construction options, resolved at use time. Entries with the same wire model ID but different tuning, and updates to the same entry, cannot reuse an earlier result. Connection credentials are never stored as plaintext in the cache key. Changes anywhere in model-provider configuration or shared request defaults conservatively start a new cache namespace. Reverting these fixes restores the earlier vision sampling, fallback capacity, Hailo budgeting and cache reuse behavior; no persisted configuration migration is required.
