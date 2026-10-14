---
id: ADR-015
title: Unified capability catalog is a read-only projection over canonical owners
date: 2026-08-22
status: proposed
relates-to:
  - https://github.com/zeroclaw-labs/zeroclaw/issues/9346
  - https://github.com/zeroclaw-labs/zeroclaw/issues/6489
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8908
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8850
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8367
  - docs/book/src/plugins/index.md
  - crates/zeroclaw-plugins/src/config.rs
---

# ADR-015: Unified Capability Catalog Is a Read-Only Projection Over Canonical Owners

## Context

ZeroClaw has several surfaces that describe capabilities: built-in channels and tools, installed plugin packages, registry-available packages, configured provider and channel aliases, gateway Integration entries, CLI plugin commands, web dashboard views, ZeroCode, and agent-facing setup guidance. These surfaces currently answer different questions and use overlapping words such as "installed", "configured", "enabled", "active", and "healthy".

The product direction in [#6489](https://github.com/zeroclaw-labs/zeroclaw/issues/6489) is one truthful catalog across integrations, built-ins, installable packages, configured instances, and runtime observations. That direction is sometimes summarized as "everything is a plugin", but the durable architecture is narrower: one catalog, not one implementation mechanism. Built-in and package-backed implementations may coexist indefinitely.

Accepted RFC [#9346](https://github.com/zeroclaw-labs/zeroclaw/issues/9346) defines the missing contract. The catalog must keep package facts, capability facts, implementation facts, configured-instance facts, and runtime observations separate. It must derive each fact from its canonical owner instead of creating another persisted lifecycle registry. It must also preserve compatibility with existing package and Integration projections before any route retirement, migration, or stable public API commitment.

This record captures that target architecture. It does not claim that the unified catalog projection, compatibility bridge, or runtime observation model has shipped.

## Decision

### Keep five identities separate

The unified catalog uses separate identities for separate facts:

- **Package artifact:** a built-in, installed, or registry-available artifact, identified by package source, namespace/name, version, and immutable content or admission revision when an artifact exists.
- **Capability:** typed behavior such as `channel:discord`, `provider:ollama`, `tool:web_search`, a memory backend, a skill, an observer, or a platform integration.
- **Implementation:** the built-in or package-provided implementation that supplies a capability.
- **Configured instance:** an operator-defined alias from the owning subsystem's canonical configuration.
- **Runtime observation:** transient activation, health, or failure evidence reported by the runtime owner for a runtime generation.

None of these identities substitutes for another. A package can expose multiple capabilities. A capability can have built-in, installed, and registry-available implementations. A configured instance can exist without an active runtime instance. A runtime observation can become stale without changing installation, configuration, or enablement.

Identifiers must not contain secrets, raw configuration values, access tokens, hostnames, user names, absolute paths, or mutable display labels. Package-provided capabilities and runtime observations remain bound to exact artifact provenance, so installed and registry versions are not unioned and upgrades cannot leave activation or health evidence ambiguous.

### Declare capability identity through owners

Capability identities are declared by the capability-family owner through typed built-in inventory fields or admitted package-manifest schema. The catalog joins artifacts and implementations to those declarations. It must not infer a logical identity from callable tool names, broad `PluginCapability` kinds, display names, or a parallel grouping table.

A family without an owner-provided typed declaration has no catalog capability identity until that owner adds one. This keeps grouping authority with the subsystem that understands the capability instead of moving it into the catalog projection.

### Project source-of-truth evidence, not lifecycle writes

The catalog is read-oriented. It materializes a view from canonical owners at request time or from a derived cache that carries enough source generations to invalidate itself. It does not accept lifecycle writes and does not persist another enablement, admission, configuration, activation, readiness, or health table.

Each state axis has one owner:

| Fact | Owner |
|---|---|
| Registry availability | the configured registry or index client |
| Built-in availability | the compiled built-in inventory |
| Installed package and admission state | the package installation and admission inventory |
| Capability identity, exports, and implementation origin | the capability-family owner through typed inventory or admitted manifest declarations |
| Configured instance | the owning subsystem's canonical `Config` section |
| Enabled state | canonical configuration plus the owning subsystem's activation policy |
| Active state | the runtime registry that instantiated or registered the instance |
| Health or failure | the capability-specific runtime owner or probe |
| Agent-facing readiness | an on-demand projection such as #8367, using catalog identities and evidence without becoming another lifecycle owner |

Missing evidence is `unknown`, not `false`. State outcomes distinguish known true, known false, unknown, and not applicable. Health is an owner-defined observation outcome, not independent booleans that can simultaneously claim `healthy` and `failed`. Runtime observations carry observation time, runtime generation, selected implementation, artifact provenance when package-backed, and a freshness rule. Once stale, health returns to unknown until refreshed.

A projection is not an atomic transaction across independent owners. Public payloads include `generated_at` and participating owner generations or provenance where useful so consumers cannot infer that package, configuration, and runtime facts were observed simultaneously.

### Keep resolver authority family-specific

Native/plugin collision and precedence are not global catalog policy. RFC [#8850](https://github.com/zeroclaw-labs/zeroclaw/issues/8850) supplies native/plugin collision behavior for channels and tools. The catalog projects that result for `channel:*` and `tool:*`.

For providers, memory backends, observers, skills, and platform integrations without an owner-defined resolver, the catalog reports every matching implementation with explicit unresolved or unknown conflict evidence and applies no implicit ordering. A later owner-defined resolver can become the source for that family without changing the catalog into the resolver.

### Keep visibility separate from authority

Catalog visibility can narrow what a user, UI, API, or agent sees. It cannot grant invocation authority.

Agent tool registries, risk profiles, per-run narrowing, destination policy, grants, approvals, and subject-scoped authorization remain outside the catalog. A consumer such as [#8367](https://github.com/zeroclaw-labs/zeroclaw/issues/8367) may derive point-in-time guidance from catalog evidence and subject-specific policy, but that guidance is a projection. It does not authorize an action, write lifecycle state, or become a configured-instance fact.

Public projections exclude credentials, secret references, raw configuration values, registry authentication, host identity, unrestricted filesystem paths, raw runtime errors, and private manifest fields. Registry and manifest text is untrusted metadata and must be rendered as data, not instructions.

### Preserve compatibility before convergence

`GET /api/plugins` remains a package-centric projection while package work stabilizes. `/api/integrations` remains a compatibility projection over the shared catalog until a separate compatibility decision authorizes retirement, redirect, or stable API breakage.

CLI, web, ZeroCode, gateway, and agent-facing readiness consume versioned projections from the same contract. Additive fields may be introduced compatibly. Identifier changes, route retirement, configuration migration, stable public API commitments, and marketplace trust policy require separate review with rollback and compatibility plans.

Package identity must map against the existing registry directions rather than minting another unrelated coordinate system. Implementation work should reconcile package coordinates with existing MCP-style package identity and the separately proposed OCI registry direction before a second consumer depends on them.

The evidence vocabulary intentionally follows established distributed-state practice: Kubernetes-style condition semantics for known, unknown, and observed facts, and systemd's distinction between enabled intent and active runtime state. ZeroClaw does not need to import those systems wholesale, but the catalog should preserve that separation.

### Acceptance gates

This ADR remains proposed until all of these conditions are met:

- package artifacts, capabilities, implementations, configured instances, runtime observations, and state evidence are documented with representative channel, provider, tool, platform, and multi-capability package examples;
- every logical capability identity comes from an owner-provided typed declaration, and the catalog cannot infer one from callable names or broad capability kinds;
- every projected state field names its source of truth and uses known, unknown, and not-applicable semantics correctly;
- package availability, installation, admission, configuration, enablement, activation, health, and agent-facing readiness remain independently representable and non-writable through the catalog;
- channel and tool built-in/plugin collision behavior matches #8850, while other capability families remain explicitly unresolved unless their owner defines a resolver;
- package-provided capabilities and runtime observations stay bound to exact artifact provenance across installed and available version differences, upgrades, reloads, and runtime generations;
- public projections expose generation or provenance metadata and do not imply atomic consistency across independent owners;
- catalog visibility cannot grant invocation authority or bypass agent, turn, destination, grant, approval, or policy checks;
- `/api/plugins` and `/api/integrations` have an additive compatibility bridge before any route convergence, retirement, or stable API commitment; and
- package-coordinate identity is reconciled against the existing MCP and OCI registry directions before more than one package consumer depends on it.

## Consequences

Positive consequences:

- Contributors can tell whether a fact is about package availability, installation, configuration, enablement, activation, health, or readiness.
- CLI, gateway, web, ZeroCode, and agent-facing guidance can use one vocabulary without copying lifecycle state.
- Built-in and plugin implementations can coexist without pretending every built-in has migrated to WASM.
- Runtime health and activation claims become evidence-bound instead of inferred from configuration or package presence.
- Compatibility work can proceed additively before public route or terminology changes.

Negative consequences:

- The catalog contract is more complex than a single `status` enum.
- Capability-family owners must add typed declarations before their capabilities can participate cleanly.
- Runtime owners must publish generation-scoped observations before the catalog can report active or health evidence.
- API convergence is slower because `/api/plugins` and `/api/integrations` must bridge through compatibility slices.
- Package-coordinate reconciliation must happen early enough to avoid another registry identity system.

## References

- [RFC #9346: Define the unified package/capability/config/runtime-state catalog contract](https://github.com/zeroclaw-labs/zeroclaw/issues/9346)
- [Tracker #6489: Unified capability catalog and plugin migration roadmap](https://github.com/zeroclaw-labs/zeroclaw/issues/6489)
- [PR #8908: Package-centric plugin list catalog](https://github.com/zeroclaw-labs/zeroclaw/pull/8908)
- [Issue #8850: Optional channels and tools move from compile-time features to runtime plugins](https://github.com/zeroclaw-labs/zeroclaw/issues/8850)
- [Issue #8367: Derived capability readiness for agent guidance](https://github.com/zeroclaw-labs/zeroclaw/issues/8367)
- [Plugins documentation](../../plugins/index.md)
- `crates/zeroclaw-plugins/src/config.rs`
