# Runtime composition

The runtime resolves the agent, config generation, principal and security policy.
An application supplies providers, memory, tools, outbound channels and an
observer through `zeroclaw_runtime::composition::RuntimeCapabilities`.

This boundary implements the turn-composition part of
[#10993](https://github.com/zeroclaw-labs/zeroclaw/issues/10993), under
[#7432 R1](https://github.com/zeroclaw-labs/zeroclaw/issues/7432).
[#11174](https://github.com/zeroclaw-labs/zeroclaw/pull/11174) introduced the five
sources and capability entry points;
[#11187](https://github.com/zeroclaw-labs/zeroclaw/pull/11187) carries the caller
and child-operation wiring. Supplying sources now selects a complete generic
registry by default. Native construction requires an explicit compatibility
recipe request.

## Public API

`RuntimeCapabilities` has five generation-owned fields:

| Field | Type | Resolution |
| --- | --- | --- |
| `providers` | `Arc<dyn ProviderSource>` | Agent, provider reference, model and principal |
| `memory` | `Arc<dyn MemorySource>` | Agent and config generation |
| `tools` | `Arc<dyn ToolSource>` | Agent, resolved policy, selected runtime and canonical memory |
| `channels` | `Arc<dyn ChannelSource>` | Outbound configured alias |
| `observer` | `Arc<dyn Observer>` | One observer for the generation |

```rust
pub trait ToolSource: Send + Sync {
    fn uses_native_registry(&self, _request: &ToolRequest<'_>) -> bool {
        false
    }

    fn tools(&self, request: &ToolRequest<'_>)
        -> anyhow::Result<Vec<Box<dyn Tool>>>;
}
```

The additive `uses_native_registry` method is a construction selector, not an
authorization decision. It defaults to `false`. A supplied-only source returns
the complete eager registry, including an authoritative empty registry. The
runtime does not substitute native tools when the vector is empty. Configured
native tools, MCP connections, peripheral connections and native skill loading
belong to the explicitly requested compatibility recipe.

The public `defaults::NoSuppliedTools` adapter returns `true` and supplies no
additional extension tools. Application `DefaultCapabilities::from_config`
selects this adapter, so CLI and daemon defaults retain their native behavior.
The older runtime entry points select the same adapter through the one
config-backed defaults recipe. They invoke the existing canonical native
factory, rather than maintaining a second construction recipe.

`ToolRequest` retains its five borrowed inputs: config, agent alias,
`SecurityPolicy`, `RuntimeAdapter` and the agent's memory. SOP engines, audit
loggers, canvas stores, ACP views, forwarded environments and execution/live
config handles stay private to the compatibility adapter. The selector adds no
config key, capability kind or policy cache.

`ProviderRequest` carries config, agent alias, an optional explicit provider
reference, the resolved model and an optional principal. Provider sources own
construction and caching. `switched_model_provider` and
`vision_model_provider` default to `model_provider`; compatibility overrides
retain the existing alias/credential routing. A supplied-source refusal never
retries through native config.

`MemorySource::memory` is asynchronous. Requests for the same agent in one
generation must reach the same underlying store. The source chooses how it opens
or caches that store. Explicit existing memory overrides and memory-free runs
retain their documented entry-point behavior.

`ChannelSource` covers outbound delivery, not inbound turn admission. Generic
CLI paths request `cli` from their source and propagate absence. The private
config-backed source wraps the application's registered CLI factory.
`defaults::NoOutboundChannels` remains closed. Inbound routing belongs to
`RuntimeIngress` and [#11012](https://github.com/zeroclaw-labs/zeroclaw/issues/11012).

## Entry points and ownership

The implemented public consumer entry is `agent::run_with_capabilities`.
`agent::process_message_with_capabilities` and
`Agent::from_config_with_capabilities` use the same registry selection and
scoped policy seam. Owned SOP steps and independent delegate targets inherit
the originating capabilities and principal.

A turn retains its generation's sources until it finishes or is cancelled.
Changing source wiring creates a new `RuntimeCapabilities`; it never swaps a
provider underneath an admitted turn. Sources may share handles within their
scope, but must resolve live policy from its canonical owner instead of taking
a long-lived policy snapshot.

The runtime resolves `SecurityPolicy` and `RuntimeAdapter` before invoking a
tool source. Returning or constructing a tool grants nothing. Every source tool
passes through the same scoped registry filter and invocation approval path.
Source tools stay outside native prefilter arcs used for skill elevation. Under
the explicit native recipe, native names retain precedence and the factory's
existing child-capability slots bind before the scoped registry is sealed.

The compatibility recipe carries the existing delegate/channel handles and
live config/execution authority unchanged. It does not rebuild SOP state or
replace the daemon's shared config with a restart-only snapshot. Tool state
continues to follow [ADR-004](./decisions/ADR-004-tool-shared-state-ownership.md).

| Caller | Principal and source lifetime |
| --- | --- |
| Application CLI agent caller | No resolved principal; one generation per call |
| RPC-owned sessions | Resolved connection principal and admitted generation |
| Cron, heartbeat and SOP workers | Existing internal identity rules; originating generation |
| Delegate, subagent and peer operations | Originating sources and principal, with existing target admission |

## DaemonRegistry and lifecycle

`DaemonRegistry` keeps its subsystem registration and supervision role. The
application CLI and daemon both obtain defaults from
`src/composition/mod.rs`. The daemon capability entry points hand the shared
sources to heartbeat and cron work and flush the supplied observer on return,
including startup refusal.

Cancellation drops an in-progress source construction future and its owned
resources. Successful CLI completion flushes and releases the observer it
owned. Daemon shutdown retires its owned starters before flushing its
capability generation. Existing channel/RPC retirement and queued-admission
checks continue to govern reload and drain.

The source-taking starter migrations remain staged work: channels and RPC
starter signatures currently retain their existing authority/context contracts.
This API does not widen gateway composition. The gateway receives no new
capability or construction authority. The relevant part of
[#6864](https://github.com/zeroclaw-labs/zeroclaw/issues/6864) is its completed
mechanical dependency gating; this boundary does not recreate that work.

## Placement and design record

The bounded runtime composition exception was added by merged
[#11092](https://github.com/zeroclaw-labs/zeroclaw/pull/11092), commit
`105741bc9f`, with Core approval. Its scope is the composition API, capability
entry-point adapters and capability-carrying `DaemonRegistry` adapters. Its
planned destination is `zeroclaw-kernel`, reviewed at agent-loop extraction or
before adding a capability kind beyond the existing five. It does not authorize
introducing concrete construction into the holding runtime.

[#11090](https://github.com/zeroclaw-labs/zeroclaw/pull/11090) is an open, blocked
design proposal. Its proposed `Runtime::run_turn` handle is not the implemented
API. The existing public capability entry points provide the real consumer
boundary. This page records their behavior rather than treating the proposal's
review approvals as ratification.

## Retained dependencies and active owners

The runtime is still a transitional holding crate. The generic path avoids
native adapter construction; it does not claim those implementation crates have
left the compiled dependency graph.

| Dependency | Active owner and bounded reason |
| --- | --- |
| `zeroclaw-providers` | `composition/config_backed.rs` owns legacy configured provider and vision adapters; `agent/agent.rs` owns compatibility switch routing. Doctor/quickstart probes and shared model-route/parser values also remain. Generic provider requests use their source and cannot fall back to these constructors. |
| `zeroclaw-memory` | `composition/config_backed.rs` owns configured memory compatibility. Explicit memory-free/ACP adapters, response caches and session persistence retain their existing owners. The supplied consumer uses its own memory implementation. |
| `zeroclaw-tools` | `composition/native_tools.rs` invokes the one factory in `tools/mod.rs` only on explicit native recipe selection. The factory owns native tool handles; `tools/scoped.rs` owns policy/MCP/skill sealing. Generic builders cannot call the factory or load native integrations. |

The executable policy in
`tests/fixtures/runtime-composition-consumer/retained-dependencies.json` names
all retained workspace edges, including API/config, parser, SOP, logging,
process, infrastructure, TLS and optional plugin support. Its native metadata
check refuses unlisted edges, stale entries and private implementation imports
in the consumer. A mandatory construction-boundary check rejects concrete
factories in capability builders. Matching a manifest snapshot cannot waive
that check, and optional features cannot excuse a forbidden construction path.

## Executable evidence and acceptance mapping

The independent `runtime-composition-consumer` fixture depends only on public
runtime/API/config contracts and small test support. It uses no runtime
test-util, concrete provider/memory/tool/channel implementation or root
application dependency. It runs a real structured tool round trip with supplied
implementations of all five kinds, checks the exact catalog, canonical memory,
channel delivery and observer activity.

Armed controls demonstrate that native configured-provider construction refuses
the fixture config and that the canonical native tool factory creates its
session-tool database. The supplied turn must complete without that constructor
marker or native peripherals. This detects constructing then hiding native
tools as well as catalog leakage.

```bash
cargo test -p runtime-composition-consumer --lib --locked
cargo test -p zeroclaw-runtime --lib r1_daemon_
cargo test -p zeroclaw --lib composition::tests::
```

| #10993 criterion | Executable or source evidence |
| --- | --- |
| AC1 contract, ownership, lifetimes and DaemonRegistry | This page and `composition.rs` |
| AC2 independent real turn without native construction | Consumer round trip, exact catalog and armed constructor controls |
| AC3 dependency boundary and explained retention | Native locked Cargo metadata plus mandatory construction ratchet |
| AC4 shared CLI/daemon contract and lifecycle | Application caller parity, public source refusal/cancellation/completion tests, owning daemon startup/shutdown tests and existing admission/drain regressions |
| AC5 linkage to #7432 R1 | Implementation/evidence references #10993 and #7432; public filing and combined release acceptance follow the repository PR workflow |

The generic boundary and compatibility behavior require scoped native tests,
strict library Clippy, formatting and documentation checks. Combined CLI and
release artifact acceptance remain separate gates on the reviewed join.
