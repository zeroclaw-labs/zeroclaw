---
id: ADR-011
title: Configured agents have explicit runtime boundaries under one daemon
date: 2026-07-19
status: accepted
relates-to:
  - https://github.com/zeroclaw-labs/zeroclaw/issues/5890
  - https://github.com/zeroclaw-labs/zeroclaw/pull/6398
  - ADR-005
  - ADR-010
  - docs/book/src/agents/overview.md
  - docs/book/src/agents/internals.md
  - docs/book/src/channels/peer-groups.md
---

# ADR-011: Configured Agents Have Explicit Runtime Boundaries Under One Daemon

This is a retroactive record of the multi-agent architecture shipped through [#6398](https://github.com/zeroclaw-labs/zeroclaw/pull/6398) in v0.8.0-beta-1. The date above is the date this ADR was added, not the original implementation date.

Accepted RFC [#5890](https://github.com/zeroclaw-labs/zeroclaw/issues/5890) proposed a broader multi-agent UX and configuration model. This record captures five durable boundaries that shipped: named identities, one supervising daemon, explicit channel relationships, agent-scoped memory, and per-agent runtime-policy and workspace scope. It does not ratify the RFC's unshipped alias-only join requirement, `memory_namespaces` schema, bundle taxonomy, swarm shape, or deferred dashboard and observability contracts.

## Context

ZeroClaw originally centered configuration and runtime behavior on one implicit agent. Multi-agent operation requires identities that can coexist without sharing workspace, memory, permissions, or channel reachability by accident.

The V3 implementation replaced the implicit singleton with configured agents keyed by alias. Each agent resolves its own runtime inputs and scoped boundaries while one daemon coordinates the enabled set. Explicit grants connect agents where collaboration is intended.

## Decision

### Agent identity and supervision

Each `[agents.<alias>]` entry defines an addressable agent identity. The alias selects the configured agent at CLI, channel, daemon, and other runtime entry points. A single-agent install uses the same model with one configured entry; there is no separate privileged singleton architecture.

Each agent has its own workspace boundary and identity source. Identity files are resolved for that agent rather than from one install-wide personality.

One `zeroclaw daemon` process supervises the enabled configured agents and their channel bindings. This does not mean each agent owns an always-running process or loop. It means daemon operation starts and coordinates the configured set rather than requiring one daemon per identity.

### Explicit communication

The durable rule is that authorized channel relationships are explicit. The current implementation uses per-agent channel bindings and peer groups. Co-residence in one daemon does not make agents channel peers.

Cross-agent channel messaging currently requires a shared peer-group relationship on the relevant channel. Peer groups are mutual routing and inbound-acceptance boundaries; agents outside that relationship do not become reachable merely because they are configured in the same install.

For compatibility, the runtime deterministically assigns channel ownership when no configured agent declares any channel binding. That fallback preserves older installs; it does not establish implicit sharing as the multi-agent contract.

Delegation and other cross-agent capabilities may impose additional gates. This ADR does not make peer-group membership a universal authorization mechanism for every capability.

### Workspace and runtime-policy scope

Each enabled agent resolves a risk profile and a workspace boundary. The default filesystem boundary is the agent's own workspace. An access entry on the addressed agent explicitly grants that agent access to the named sibling workspace.

The workspace-only jail can be disabled by the resolved risk profile, including full autonomy, or by the agent's unrestricted-filesystem setting. Remaining forbidden-path policy and host permissions still apply.

Sharing a provider, channel adapter, bundle, or other referenced configuration does not merge the agents' workspace or resolved runtime-policy scope. It may intentionally share the referenced resource or credentials: secrets remain install-wide rather than per-agent.

### Memory isolation

The configured agent identity is the scope consumed by the memory contract. Backends enforce that scope through stored agent metadata or an agent-owned storage boundary and scoped adapter.

Cross-agent recall is additive and explicit. It requires compatible same-backend access; the runtime does not silently bridge different backend kinds.

[ADR-005](./ADR-005-pluggable-memory-backends.md) owns storage, attribution, recall allowlists, backend compatibility, and destructive-operation constraints. [ADR-010](./ADR-010-memory-authority-boundaries.md) separately assigns authority between session history, curated memory, and enrichment.

### Schema evolution boundary

The durable decision is the named identity and explicit boundary model above. Exact config references, bundle aliases, and filesystem fields may evolve without superseding this ADR as long as those boundaries remain intact.

## Consequences

Positive consequences:

- Single-agent and multi-agent installs share one runtime model.
- Operators can reason about workspace, memory, runtime policy, and channel reachability per named agent.
- Sharing is visible through configuration rather than inferred from process co-residence.
- Cross-agent collaboration can be added selectively without weakening the default scoped boundaries.

Negative consequences:

- Every agent-aware entry point must carry or resolve an agent alias correctly.
- Configuration validation must reject dangling aliases and incompatible cross-agent grants before runtime use.
- Shared install-wide backends and adapters need agent attribution and filtering rather than relying on separate processes for scope.
- Secrets remain installation-wide, so per-agent workspace and runtime-policy scope does not imply per-agent credential isolation.

Follow-up decisions:

- New cross-agent capabilities must define their own authorization and attribution boundary rather than assuming peer-group or daemon membership is sufficient.
- Changes that weaken default workspace, risk-profile, or memory scope require an explicit architecture and security decision.

## References

- [RFC #5890: Multi-agent UX flow](https://github.com/zeroclaw-labs/zeroclaw/issues/5890)
- [PR #6398: Multi-agent V3 runtime and schema](https://github.com/zeroclaw-labs/zeroclaw/pull/6398)
- [Agents](../../agents/overview.md)
- [Running agents](../../agents/operating.md)
- [Runtime internals](../../agents/internals.md)
- [Peer groups](../../channels/peer-groups.md)
- `crates/zeroclaw-config/src/schema.rs`
- `crates/zeroclaw-config/src/multi_agent.rs`
- `crates/zeroclaw-config/src/policy.rs`
- `crates/zeroclaw-channels/src/orchestrator/mod.rs`
- `crates/zeroclaw-memory/src/agent_scoped.rs`
