# Runtime composition contract (proposal)

**Status:** proposed, not implemented. This page is the composition-API proposal that [#10993](https://github.com/zeroclaw-labs/zeroclaw/issues/10993) asks for before any caller moves. It delivers the first acceptance item of that issue and the design half of [#7432](https://github.com/zeroclaw-labs/zeroclaw/issues/7432) R1. The only code that accompanies it is the type skeleton in `crates/zeroclaw-runtime/src/composition.rs`, which nothing consumes yet.

The goal is the one RFC [#5574](https://github.com/zeroclaw-labs/zeroclaw/issues/5574) set for Phase 2 D1: an agent runtime that an embedder can run with capabilities it supplies, where the runtime "has no knowledge of Telegram, Discord, Anthropic, or any specific tool implementation", and where the binary becomes a thin wiring layer.

## Where the runtime stands today

Measured on master `c44909b1ed` with `cargo metadata` and a production-only grep that excludes `#[cfg(test)]` modules and test files.

### Crate edges

| Crate | Depends on (workspace crates) |
| --- | --- |
| `zeroclaw-runtime` | api, commands, config, infra, log, macros, **memory**, **providers**, relay-proto, sop-graph, spawn, tls, tool-call-parser, **tools**; plugins (optional) |
| `zeroclaw-channels` | api, config, infra, log, memory, providers, **runtime**, spawn, tool-call-parser; tools (optional) |
| `zeroclaw-gateway` | api, **channels**, config, hardware, infra, log, memory, providers, **runtime**, spawn, tls, tools; plugins (optional) |

The runtime links every concrete provider, memory backend, and optional tool unconditionally. The channels crate sits above the runtime, which is the inversion [#6864](https://github.com/zeroclaw-labs/zeroclaw/issues/6864) describes.

### Where concrete capabilities are constructed

Inside the runtime, all capability construction funnels through a small number of entry functions:

| Function | Builds |
| --- | --- |
| `agent::loop_::run` | provider, memory, observer, tool registry |
| `agent::loop_::process_message_shared` | provider, memory, observer, tool registry |
| `Agent::from_config_with_session_cwd_and_mcp_approval_mode` | provider, memory, observer |
| `Agent::try_apply_model_switch`, `build_session_model_provider` | provider |
| `agent::turn::assemble_owned_execution` | memory, tool registry |
| `tools::delegate` (`build_target_provider`, `memory_for_target_agent`, `independent_agentic_tools_for_target`) | provider, memory, tool registry for a delegate target |
| `daemon::run`, `daemon::run_heartbeat_worker` | memory, observer |
| `cron::scheduler::run_agent_job` | memory |
| `tools::all_tools_with_runtime` and `default_tools` | the tool registry itself |

| Capability | Production construction sites | Files |
| --- | --- | --- |
| Provider | 13 | 6 |
| Memory | 14 | 8 |
| Observer | 8 | 4 |
| Tool registry assembly | 10 | 6 |
| `zeroclaw_tools::` uses | 98 (78 in `tools/mod.rs`) | 10 |

Construction also happens *outside* the runtime, in parallel with it. That is a second source of truth, not only a dependency problem:

- **Gateway** (`zeroclaw-gateway/src/lib.rs`) builds its own resilient provider, memory backend, observer, and two tool registries, then calls `agent::process_message`.
- **Channels orchestrator** keeps a per-sender provider cache (`get_or_create_provider`) built with `create_resilient_model_provider_from_ref`, and calls `all_tools_with_runtime` directly.
- **ACP server** builds agents through `Agent::from_config_with_session_cwd_and_mcp_backchannel*`.
- **`src/main.rs`** constructs providers for several commands and memory for the SOP engine, then calls `agent::run` and `daemon::run`.

### What `DaemonRegistry` already does

`daemon::DaemonRegistry` (landed in [#7430](https://github.com/zeroclaw-labs/zeroclaw/pull/7430)) registers **subsystem starters**: gateway, channels orchestrator, local socket, WSS, relay, enrollment, and MQTT. It also carries the shared SOP engine and audit logger. It does not register providers, memory, tools, or observers. It solves process composition, not capability composition.

## Proposal

### Two registries, because there are two lifetimes

RFC #5574 sketched one `Registry` with `register_channel`, `register_tool`, `set_provider`, `set_memory`, and `set_observer`. The current code has two things with different lifetimes, and folding them into one type would hide that:

| | Capabilities | Subsystems |
| --- | --- | --- |
| What | Providers, memory, tools, outbound channels, observer | Gateway, channels orchestrator, socket, WSS, relay, enrollment, MQTT |
| Type | `RuntimeCapabilities` (new) | `DaemonRegistry` (existing) |
| Lifetime | One config generation | One daemon run or reload iteration |
| Consumed by | Every agent turn | `daemon::run` supervision |

The runtime holds both. Subsystem starters receive the generation's capabilities instead of constructing their own, which is what removes the parallel construction in the channels orchestrator. The gateway is the exception: its starter is deleted at the v0.9.0 gateway split, so it is never migrated onto this contract (step 4).

### Sources, not instances

The RFC sketch passes finished instances (`set_provider(Arc<dyn Provider>)`). That cannot serve the current runtime:

- Providers and memory resolve **per agent** under the multi-agent model ([ADR-011](./decisions/ADR-011-multi-agent-runtime-boundaries.md)).
- A session can **switch model** mid-conversation (`try_apply_model_switch`), and a delegate resolves a **different agent's** provider and memory.
- A live config apply produces a new **generation** ([ADR-012](./decisions/ADR-012-generation-scoped-live-config-apply.md)), and turns already running must keep the capabilities they started with.

So the runtime asks *sources* for what it needs, naming the agent and the generation. A source decides how to build and cache. The runtime decides when to ask, for which agent, and under which resolved policy.

### Public types

These are in the skeleton, `zeroclaw_runtime::composition`:

```rust
#[derive(Clone)]
pub struct RuntimeCapabilities {
    pub providers: Arc<dyn ProviderSource>,
    pub memory: Arc<dyn MemorySource>,
    pub tools: Arc<dyn ToolSource>,
    pub channels: Arc<dyn ChannelSource>,
    pub observer: Arc<dyn Observer>,
}

pub trait ProviderSource: Send + Sync {
    fn model_provider(&self, request: &ProviderRequest<'_>)
        -> anyhow::Result<Arc<dyn ModelProvider>>;
}
pub trait MemorySource: Send + Sync {
    fn memory(&self, request: &MemoryRequest<'_>) -> anyhow::Result<Arc<dyn Memory>>;
}
pub trait ToolSource: Send + Sync {
    fn tools(&self, request: &ToolRequest<'_>) -> anyhow::Result<Vec<Box<dyn Tool>>>;
}
pub trait ChannelSource: Send + Sync {
    fn channel(&self, alias: &str) -> Option<Arc<dyn Channel>>;
}
```

`ProviderRequest` carries the config, the agent alias, and an optional explicit provider reference. `MemoryRequest` carries the config and agent alias. `ToolRequest` carries the config, agent alias, resolved `SecurityPolicy`, selected `RuntimeAdapter`, and the agent's memory handle.

The runtime entry point these feed is proposed as follows. It is **not** in the skeleton, because it cannot exist without an implementation:

```rust
pub struct Runtime { /* config, Arc<RuntimeCapabilities>, DaemonRegistry */ }

impl Runtime {
    pub fn new(config: Config, capabilities: RuntimeCapabilities) -> Self;
    pub fn with_daemon(self, subsystems: DaemonRegistry) -> Self;
    /// One agent turn with no daemon: the independent-consumer path.
    pub async fn run_turn(&self, agent_alias: &str, message: &str) -> anyhow::Result<String>;
}

/// Daemon mode: supervise the registered subsystems.
pub async fn run(runtime: Runtime) -> anyhow::Result<DaemonExit>;
```

`run_turn` is deliberately minimal. The full inbound command path, with session serialization, admission, cancellation, and approval, belongs to the `RuntimeIngress` contract tracked in [#11012](https://github.com/zeroclaw-labs/zeroclaw/issues/11012). `run_turn` exists so that the independent-consumer acceptance criterion has a stable surface before that contract lands, and it should become a thin client of `RuntimeIngress` once it does.

### Ownership and lifetime rules

1. **The runtime owns resolution; sources own construction.** The runtime decides which agent, which provider reference, and which generation a request is for. A source never reads ambient state to decide that.
2. **Capabilities are immutable per generation.** A config apply that changes wiring builds a new `RuntimeCapabilities`. A turn keeps the `Arc` it started with until it finishes, so a reload never swaps a provider out from under an in-flight turn.
3. **Returned handles are borrowed for a scope.** A provider or memory handle belongs to the session or turn that asked for it. Sources may cache and share instances; callers must not stash them past their session.
4. **One store per agent per generation.** Two memory requests for the same agent in one generation must reach the same underlying store. Today's code guarantees this implicitly by constructing from the same config; the contract makes it an obligation of the source.
5. **The observer is process-level per generation.** It is an instance, not a source, because it is not resolved per agent.
6. **Outbound only.** `ChannelSource` covers channels the runtime sends through: replies by alias, scheduled delivery, and approval prompts. Inbound turns arrive through `RuntimeIngress`, not through this contract.

### Security policy stays with the runtime

The issue requires resolved security policy to remain explicit across the boundary. The rule is:

- The runtime resolves each agent's `SecurityPolicy` and `RuntimeAdapter` and passes them *into* `ToolRequest`. A source cannot supply its own.
- Building a tool against a policy authorizes nothing. The runtime still gates and approves every call after construction, exactly as today.
- The runtime may add core tools and remove tools the policy excludes. It never adds a tool the policy forbids because a source returned it.

Parity tests in the first implementation slice must show that the effective tool set and approval behavior are unchanged for every agent in a representative config.

### How `DaemonRegistry` fits

`DaemonRegistry` is incorporated, not replaced:

- It keeps its role and its current registration methods.
- The channels, cron, SOP, and `RpcContext` starters gain access to the generation's `RuntimeCapabilities`, so they stop building providers, memory, and tools of their own. That signature change is its own slice (step 4 below). The gateway starter receives no capabilities: it is deleted at the v0.9.0 cut, and its own construction disappears as its routes move to RPC.
- The shared SOP engine and audit logger it carries today are capability-shaped state. They stay where they are in this proposal and are listed as a follow-up to move behind the capability generation.

### Relationship to other work

- **[#6864](https://github.com/zeroclaw-labs/zeroclaw/issues/6864)** was narrowed on 2026-08-23 to mechanical dependency gating. The orchestrator move it originally proposed now belongs to the runtime-owned session work. This proposal does not duplicate either. It supplies the capability contract the moved orchestrator will consume.
- **[#11012](https://github.com/zeroclaw-labs/zeroclaw/issues/11012) and its RFC [#9487](https://github.com/zeroclaw-labs/zeroclaw/issues/9487)** own the inbound command path (`RuntimeIngress` / `InboundTurn`). This proposal owns what a turn is *built from*. The two meet at the `Runtime` handle, which ingress handlers hold.
- **[#10998](https://github.com/zeroclaw-labs/zeroclaw/issues/10998)** (core tools only in the default runtime) consumes `ToolSource`: the optional tools move behind the application's `ToolSource`, and the runtime keeps only its core tools.

## Where the contract lives

The contract needs `Config` and `SecurityPolicy`, which live in `zeroclaw-config`. That rules out `zeroclaw-api`, which has no workspace dependencies by design. The remaining homes are:

| Option | For | Against |
| --- | --- | --- |
| A. `zeroclaw-runtime` (skeleton's current placement) | It *is* the runtime's public surface; no new crate; callers already depend on it | The crate is the transitional holding crate. Placing new public API there needs a Core Team exception under [ADR-016](./decisions/ADR-016-holding-crate-exceptions.md), which this proposal cannot grant itself |
| B. A new contract crate depending on api and config | Clean boundary; application wiring can depend on it without the runtime | A new crate ahead of the planned kernel extraction; the ADR-016 record already notes the risk of establishing boundaries early |
| C. The planned `zeroclaw-kernel` | The destination both active runtime exceptions already name | Does not exist; extracting it first would block this work on the agent-loop extraction |

**Decision: option A.** The contract lives in `zeroclaw-runtime`, under a holding-crate exception recorded per ADR-016. The exception row is proposed separately, as ADR-016 requires, in [#11092](https://github.com/zeroclaw-labs/zeroclaw/pull/11092): scope `src/composition.rs`, the entry-point adapters that consume it, and the capability-carrying `DaemonRegistry` starter signatures; destination `zeroclaw-kernel`; reviewed at the agent-loop extraction design review.

**The exception is pending Core Team review.** The placement was chosen under a delegated code call, which is not the Core Team approval ADR-016 requires. Until a Core Team member approves #11092, no exception exists, and the skeleton that accompanies this page must not merge.

## Migration order for callers

Every step keeps existing configuration and effective permissions. Old entry points stay as compatibility adapters until their last caller moves, as the issue's risk section requires.

| Step | Change | Evidence required |
| --- | --- | --- |
| 0 | This proposal and the skeleton. No callers. | Review of the API boundary |
| 1 | `DefaultCapabilities::from_config`: sources that wrap today's `create_*` functions unchanged | Parity tests for provider, memory, and tool-set resolution per agent |
| 2 | Runtime entry points gain `*_with_capabilities` forms: `agent::loop_::run`, `process_message_shared`, `Agent::from_config*`, `assemble_owned_execution`. Old signatures delegate through `DefaultCapabilities` | Existing agent tests pass unchanged; policy-propagation regression tests |
| 3 | `src/main.rs` builds `DefaultCapabilities` once and calls the new forms for `agent` and `daemon` | Startup, cancellation, and shutdown regression coverage for CLI and daemon |
| 4 | The channels starter, cron scheduler, SOP drivers, and `RpcContext` receive the generation's `RuntimeCapabilities`. The gateway starter is deleted at the v0.9.0 cut and receives no capabilities; its own construction disappears as its routes move to RPC | Parity for each migrated starter; no `create_*` calls left in the migrated starters. The gateway is not migrated in place: the check that it cannot re-couple is the dependency ratchet in step 7, not a rule |
| 5 | Channels orchestrator's per-sender provider cache becomes a `ProviderSource` implementation; ACP moves to the new forms. Sequenced with #11012 | No `create_*` calls left in `zeroclaw-channels` |
| 6 | Internal callers: delegate, cron, heartbeat, subagents, SOP tool specs | No `create_*` calls left in the runtime outside `DefaultCapabilities` |
| 7 | `DefaultCapabilities` and tier-3 (optional) tool assembly move to the application layer behind its `ToolSource`. Tier-1 (core) and tier-2 (host-coupled) tools stay runtime-constructed, and the runtime keeps `zeroclaw-tools` as an explained dependency through v0.9.0 (see below); dropping it is v1.0.0 work. Tools are grouped under three coarse Cargo features, `tools-core` (always on), `tools-host`, and `tools-extra` (both default on), plus per-integration features for heavy dependencies (with #10998) | A dependency ratchet test over `cargo metadata` with three rules: the runtime (its `zeroclaw-tools` edge explained), the gateway (no runtime, channels, tools, providers, memory, plugins, config, hardware, or sop-graph), and rpc-proto (api, config, and sop-graph only, no tokio I/O). An independent consumer running a real agent turn through `Runtime::run_turn` with a `ToolSource` that returns an empty tier 3. Size evidence comparing `--no-default-features --features agent-runtime,tools-core` against dist per target (#10998 AC5) |
| 8 | Remove the compatibility adapters | Removal is mechanical once no caller remains |

## Dependencies that remain, and why

The issue asks for any retained dependency to be explained rather than hidden behind an unused feature. The expected end state is:

- **`zeroclaw-tools`: retained through v0.9.0, as an explained edge.** Tier-1 core tools are split across the two crates today (`shell` and `file_read` in the runtime; `file_write`, `file_edit`, `glob_search`, `content_search`, the memory tools, `git_operations`, and `web_fetch` in `zeroclaw-tools`), and tier-2 tools need runtime internals (the scheduler, canvas store, ask-user handle, SOP engine, session backend, and live config) that `ToolRequest` does not carry. Moving either behind an application `ToolSource` would leak those internals into a public contract. So step 7 moves only tier 3, and the ratchet names this edge. Dropping the dependency is v1.0.0 work.
- **`zeroclaw-providers` and `zeroclaw-memory`: expected to remain for now**, for reasons outside turn composition: `doctor`, `quickstart`, `migration`, and vision routing construct providers or memory directly and are still hosted in this crate. Each is a separate extraction candidate under the holding-crate plan. Step 7's ratchet should allow exactly these edges and name them, so a new use fails the check.

## Acceptance mapping

| #10993 acceptance item | Delivered by |
| --- | --- |
| Document the public composition API, ownership and lifetime rules, `DaemonRegistry`, relevant part of #6864 | This page (step 0) |
| Independent consumer runs a real agent turn with supplied capabilities | Step 7, through `Runtime::run_turn` |
| Dependency checks demonstrate the boundary; retained dependencies explained | Step 7 ratchet and the section above |
| CLI and daemon use the shared contract, with policy, startup, cancellation, and shutdown coverage | Steps 2 and 3 |
| Implementation PRs and evidence linked to #7432 R1 | Each step's PR |

## Settled decisions

These were open when this page was first proposed. The v0.9.0 architecture review settled both.

1. **`run_turn` versus waiting for `RuntimeIngress`.** `run_turn` ships and stays minimal, as a client of the same session actor that `session/prompt` uses. It does not wait for #11012.
2. **Whether `ProviderSource` receives the requesting principal.** Yes, from the first implementation slice: `ProviderRequest` gains `principal: Option<&PrincipalId>` when the capability-taking constructors land. Adding a field to an embedder-implemented request type later would be a breaking change. The skeleton on this page does not carry the field yet.
