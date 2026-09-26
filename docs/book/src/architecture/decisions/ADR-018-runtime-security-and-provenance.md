---
id: ADR-018
title: Runtime security and provenance use runtime-owned decision boundaries
date: 2026-08-17
status: proposed
relates-to:
  - https://github.com/zeroclaw-labs/zeroclaw/issues/7142
  - https://github.com/zeroclaw-labs/zeroclaw/issues/6971
  - https://github.com/zeroclaw-labs/zeroclaw/issues/6954
  - https://github.com/zeroclaw-labs/zeroclaw/issues/7432
  - https://github.com/zeroclaw-labs/zeroclaw/issues/7141
  - https://github.com/zeroclaw-labs/zeroclaw/issues/7155
  - https://github.com/zeroclaw-labs/zeroclaw/issues/6996
  - https://github.com/zeroclaw-labs/zeroclaw/issues/6909
  - https://github.com/zeroclaw-labs/zeroclaw/issues/3767
  - https://github.com/zeroclaw-labs/zeroclaw/issues/6105
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8583
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8289
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8290
  - https://github.com/zeroclaw-labs/zeroclaw/pull/10425
  - docs/book/src/architecture/request-lifecycle.md
  - docs/book/src/architecture/background-work-lifecycle.md
  - docs/book/src/architecture/tool-execution-lifecycle.md
  - docs/book/src/security/model.md
  - crates/zeroclaw-api/src/ingress.rs
  - crates/zeroclaw-runtime/src/agent/turn/mod.rs
  - crates/zeroclaw-runtime/src/security/ingress.rs
  - crates/zeroclaw-runtime/src/security/audit.rs
  - crates/zeroclaw-runtime/src/approval/mod.rs
  - crates/zeroclaw-runtime/src/cron/scheduler.rs
  - crates/zeroclaw-runtime/src/tools/send_message_to_peer.rs
  - crates/zeroclaw-runtime/src/tools/spawn_subagent.rs
---

# ADR-018: Runtime Security and Provenance Use Runtime-Owned Decision Boundaries

## Context

ZeroClaw has several security-sensitive paths that all need the same answer to three questions: who or what caused this work, what policy allowed it, and what evidence proves the boundary was applied before the effect happened. Tool calls, shell commands, gateway and RPC mutations, scheduled jobs, SOP steps, steering messages, and peer-agent dispatches have historically answered those questions in different places.

That split creates two failure modes. A new action path can forget one step, such as approval, provenance stamping, final target validation, or audit evidence. A new extension point can also become too powerful if it can decide policy, suppress canonical evidence, and handle incident response as one replaceable provider.

Three accepted RFCs describe the architecture that fixes this at the runtime boundary:

- [#7142](https://github.com/zeroclaw-labs/zeroclaw/issues/7142) accepts a runtime-owned security decision pipeline for protected actions. The pipeline combines built-in policy, canonical principal grants, approval requirements, sandbox constraints, restrictive overlays, action fingerprints, single-use runtime permits, canonical audit events, and outcome records.
- [#6971](https://github.com/zeroclaw-labs/zeroclaw/issues/6971) accepts credential-surface authority, security posture reporting, and one runtime-owned model-admission path. Credential-shaped config fields require explicit handling classifications, protected values stay out of save and property-readback paths, and the runtime credential-boundary map identifies where plaintext credentials may flow. Every model-bound turn and steering injection receives a trusted ingress envelope before the model sees it, while transport-edge authentication and abuse controls stay outside that shared model-admission layer.
- [#6954](https://github.com/zeroclaw-labs/zeroclaw/issues/6954) accepts runtime-owned provenance for internally initiated turns. Cron, daemon, SOP, subagent, and peer-agent work must carry internal principals, conversation binding, reply provenance, explicit missing-context behavior, and separate execution, delivery, and persistence outcomes.

These decisions are adjacent to, but not replacements for, the canonical-principal decision in [#7141](https://github.com/zeroclaw-labs/zeroclaw/issues/7141), the command/tool confirmation decision in [#7155](https://github.com/zeroclaw-labs/zeroclaw/issues/7155), the filesystem sandbox decision in [#6996](https://github.com/zeroclaw-labs/zeroclaw/issues/6996), and consumer-specific target validation such as [#6909](https://github.com/zeroclaw-labs/zeroclaw/issues/6909).

The current codebase contains important groundwork, not the complete target. `IngressContext`, `TurnOrigin`, and `IngressDecision` exist; the turn engine calls the ingress policy at loop entry and steering drain; the default policy returns `Loop`; `AuditLogger` is reusable hash-chain scaffolding; and canonical-principal groundwork has begun. The internal-principal and separated-outcome slice is still under review in [#10425](https://github.com/zeroclaw-labs/zeroclaw/pull/10425). This ADR records the target architecture and remains proposed until the gates below are met.

## Decision

### Keep the runtime as the mandatory security authority

Protected actions go through one runtime-owned security decision pipeline before execution or mutation. The pipeline constructs a typed request from trusted runtime context, resolves built-in policy and canonical-principal grants, applies no-escalation and sandbox constraints, composes restrictive overlays, collects trusted approval when required, revalidates freshness immediately before the effect, creates canonical audit evidence, mints a single-use runtime permit, and consumes that permit at the owned execution boundary.

The permit is the handoff between decision and effect. It is bound to the normalized action, principal, policy generations, runtime epoch, target identity, approval expiry, and fingerprint schema. It cannot be serialized, reconstructed from arguments, duplicated for a sibling action, or consumed more than once.

Security-sensitive work is denied when the runtime cannot classify it. A new protected boundary must be registered in the action inventory or a mechanically equivalent structure that names the owner, request constructor, execution boundary, mandatory policy layers, and audit classification.

### Let extensions narrow policy, not replace the floor

Restrictive overlays may deny an action, narrow its resources, or require stronger or fresher approval. They may not convert a denial into an allow, broaden principal grants, weaken sandbox or risk-profile limits, choose themselves from untrusted request data, suppress canonical audit events, or mark baseline incident response complete.

Optional audit sinks and responders are separate extension points. They can receive redacted projections or add containment and notification behavior, but they cannot remove, rewrite, delay, or replace the runtime-created canonical event and baseline runtime hooks.

Future public plugin, WIT, remote-policy, dynamic-library, or incident-responder APIs require their own implementation and trust review. This ADR accepts the authority boundaries, not a speculative third-party security-provider ABI.

### Keep credential surfaces classified and protected

Credential-shaped config fields must have explicit handling classifications, such as encrypted secret, path-only reference, public value, external auth store, compatibility environment path, or requires follow-up. A new credential-shaped field cannot enter the schema without a classification test, audit, or mechanically equivalent ratchet.

Protected credential values must stay out of save and property-readback paths. The classification layer says how values are stored or exposed; it does not by itself decide every runtime component that may receive plaintext.

A separate runtime credential-boundary map names where plaintext may flow across provider and channel clients, tools, MCP servers, delegated work, CLI wrappers, gateway handlers, logs, receipts, events, and error paths. This ADR records that map as a required implementation gate and allows focused hardening driven by it, but it does not accept a full credential broker or a new default isolation profile.

### Use one model-admission path for inbound content

Every model-bound turn passes through the shared ingress policy before model execution. Every steering injection receives and is evaluated against its own trusted ingress envelope rather than inheriting stale sender, message, transport, or trust facts from the original turn.

The ingress policy produces `Loop`, `Annotate`, `Gate`, or `Drop`. Default `Loop` preserves current behavior and does not allocate managed SOP work. `Annotate` frames content as untrusted data before continuing. `Gate` hands the turn to a managed human or SOP decision path. `Drop` refuses the turn and records the reason.

Transport-edge controls remain mandatory and separate. Channel pairing, sender allowlists, signature checks, payload validation, rate limiting, protocol-specific filtering, and resource-abuse controls run before the shared model-admission layer. The shared layer decides whether normalized content may enter the model; it does not authenticate the transport.

Before configurable non-`Loop` policy becomes reachable, unimplemented non-`Loop` dispositions must fail closed. A `Gate` branch that falls through as `Loop` is not compatible with this decision once configuration can select `Gate`.

### Stamp provenance from runtime facts

Provenance is stamped by trusted runtime entry code, never by message content or tool arguments. Origin and trust are separate facts. Internal, scheduled, delegated, or relayed work may derive from external content and does not become trusted merely because the immediate caller is internal.

Canonical principals from #7141 supply authenticated actor identity and grants where an external or authenticated actor exists. Internal principals from #6954 identify runtime-owned initiators such as cron jobs, daemon tasks, SOP steps, peer-agent dispatch, and nested sub-turns. Those internal principals are provenance facts, not external peers, and they never appear in peer groups or grant tables as if they were transport users.

Subturns preserve the parent provenance or record a parent chain so delegated work does not lose the identity and trust facts that caused it. SOP and daemon-originated turns must use names that distinguish their concrete task sources in traces and run records.

### Bind internal work to conversations without transferring authority

An internally initiated turn may carry one runtime-internal conversation binding. The binding identifies where history should be loaded from and where the completed turn should append, subject to the normal trim and persistence rules. Cron `session_target = "main"` binds to the agent's main interactive session; `isolated` deliberately carries no binding.

Binding is not delivery and not memory recall. Binding answers which conversation owns the context and append. Delivery answers where the user sees an announcement or reply. Memory recall remains controlled by the memory injection policy.

Peer-agent replies use opaque reply provenance, not a shared session. The sender's turn captures a correlation id, sender identity, and sender conversation binding before dispatch. The recipient executes as its own agent and cannot read or dereference the sender's session handle. On completion, the runtime appends or delivers the recipient's final text along that provenance.

V1 peer reply provenance is process-local. A completed live-process recipient turn must either deliver along the provenance or record a failed delivery outcome, but in-flight unbound `degrade` dispatches may be lost across crash. A durable dispatch row or durable queue is a separate follow-up, not part of this V1 decision.

Missing context is explicit. `isolated` means no binding is expected, `require` rejects dispatch when required reply provenance cannot be derived, and `degrade` runs without the binding while recording the degradation. The recipient or message content cannot weaken `require` into `degrade`.

### Keep outcomes and status evidence separate

Execution, delivery, persistence, and audit durability are separate outcomes. A compatibility status may summarize them, but it must be derived from the independent axes rather than written as a competing source of truth.

Security posture reports distinguish configured intent, resolved or probed capability, and runtime-verified enforcement. Words such as `active` and `enforced` are reserved for evidence tied to the runtime instance that actually applied the boundary. Optional projections, logs, observer events, and receipts may expose useful evidence, but they do not become another security authority.

When the canonical durable audit sink is enabled, the pre-execution event is committed before the protected mutation. If that commit fails, the mutation is denied and security status reports the degraded or unavailable state. Outcome-write failure after an effect records an explicit unknown or degraded audit state, because an action that already ran cannot be undone by audit persistence.

### Keep rollout sliced and reversible

Implementation follows the routing tracker in [#7432](https://github.com/zeroclaw-labs/zeroclaw/issues/7432) and the focused implementation issues and PRs linked from the source RFCs. The runtime security pipeline, canonical audit sink, credential-surface classification, runtime credential-boundary map, ingress stamping, non-`Loop` policy, internal-principal envelope, conversation binding, peer reply provenance, and durable-dispatch follow-up must land as independently reviewable slices.

No single PR should attempt to route every action, every transport, every audit sink, and every internal-turn consumer through the full target at once. Each slice must preserve existing default behavior unless that slice explicitly owns a compatibility change.

### Acceptance gates

This ADR remains proposed until all of these conditions are met:

- the runtime action pipeline has a typed request and decision vocabulary, an action inventory or equivalent ratchet for protected boundaries, a single-use unforgeable permit, generation and runtime-epoch freshness checks, and final target revalidation at the owned execution or mutation boundary;
- direct, nested, scheduled, delegated, SOP, gateway, RPC, and security-sensitive tool paths cannot bypass the registered pipeline boundary, and unknown or unclassified protected actions fail closed;
- built-in policy, #7141 principal grants, no-escalation, #7155 approval provenance, #6996 sandbox constraints, and consumer-specific revalidation such as #6909 remain mandatory layers outside restrictive-overlay control;
- credential-shaped config fields have explicit handling classifications, with a schema audit, test, or equivalent ratchet that fails new unclassified fields;
- protected credential values remain absent from save and property-readback paths, with non-exposure coverage where applicable;
- the runtime credential-boundary map covers provider and channel clients, tools, MCP servers, delegated work, CLI wrappers, gateway handlers, logs, receipts, events, and error paths, with resulting hardening gaps routed to focused follow-ups rather than treated as an accepted credential broker;
- the canonical minimized security event is runtime-created, includes policy decisions and outcome evidence without secret payloads, and the enabled durable sink commits the pre-execution event before mutation or denies the mutation on persistence failure;
- optional audit sinks and incident responders can add projections or responses but cannot suppress, replace, delay, or rewrite canonical evidence or baseline runtime hooks;
- every model-bound turn and steering injection receives its own trusted ingress envelope with transport, sender or principal, message identity where available, origin, and trust facts stamped by entry code rather than message content;
- configurable `Annotate`, `Gate`, and `Drop` behavior cannot be enabled until unimplemented non-`Loop` dispositions fail closed with audit evidence and tests;
- transport-edge authentication, pairing, allowlists, signature checks, payload validation, rate limits, self/bot filtering, and abuse controls remain enforced before normalized model admission;
- internal principals are implemented for cron, daemon/SOP, peer-agent, and subturn entry points, with tests proving they are provenance facts and never peer-membership grants;
- conversation binding loads and appends history through a serialized per-conversation owner, cron `session_target = "main"` becomes visible in the next interactive turn, and `isolated` remains deliberately unbound;
- peer reply provenance delivers or records completed live-process recipient output without exposing the sender's session handle to the recipient, and `require` cannot be weakened by the recipient or message content;
- execution, delivery, and persistence outcomes remain independently stored or emitted, with any compatibility rollup derived from those axes;
- posture, release, and standing documentation use configured, resolved/probed, and runtime-verified evidence language accurately, including correction of prior active-enforcement claims that were only phase-1 scaffolding; and
- #7432 and the focused implementation trackers identify the remaining slices, owners, tests, and release or rollback notes before this ADR is marked accepted.

## Consequences

Positive consequences:

- Reviewers get one runtime-owned question for protected effects: did this action receive a current decision and consume a valid permit at its boundary?
- Security extensions become less dangerous because they can narrow or project, but not replace the mandatory floor.
- Inbound content decisions, steering messages, scheduled work, and peer-agent replies share provenance vocabulary instead of inventing local trust stories.
- Operators can distinguish configured policy, resolved capability, and runtime-verified enforcement instead of reading all status output as equally proven.
- Internal work can participate in conversation history and reply routing without turning cron jobs, SOP steps, or peer agents into fake external channel peers.

Negative consequences:

- The runtime must carry more structured context through action and turn boundaries.
- Some currently useful code paths remain only partially proven until their implementation slices add the ratchets and tests named above.
- A default-preserving rollout is slower than a single broad security rewrite.
- Non-`Loop` ingress behavior and durable peer dispatch cannot ship casually; they need failure, provenance, and compatibility evidence first.
- Audit durability introduces file I/O and fail-closed behavior when enabled, so compatibility and rollback must be explicit.

## References

- [RFC #7142: Runtime-owned security decision pipeline and restrictive overlays](https://github.com/zeroclaw-labs/zeroclaw/issues/7142)
- [RFC #6971: Security posture, credential boundaries, and universal ingress policy](https://github.com/zeroclaw-labs/zeroclaw/issues/6971)
- [RFC #6954: Provenance, conversation binding, and reply contract for internally initiated agent turns](https://github.com/zeroclaw-labs/zeroclaw/issues/6954)
- [Tracker #7432: v0.9.0 auth, security, gateway, and breaking-change queue](https://github.com/zeroclaw-labs/zeroclaw/issues/7432)
- [RFC #7141: Pluggable inbound authentication and canonical principals](https://github.com/zeroclaw-labs/zeroclaw/issues/7141)
- [RFC #7155: Per-execution confirmation tier and command pattern policy](https://github.com/zeroclaw-labs/zeroclaw/issues/7155)
- [RFC #6996: Granular sandbox policy](https://github.com/zeroclaw-labs/zeroclaw/issues/6996)
- [Issue #6909: Consumer-specific desktop target validation](https://github.com/zeroclaw-labs/zeroclaw/issues/6909)
- [Issue #6105: Agent does not have context of the cron job it is run](https://github.com/zeroclaw-labs/zeroclaw/issues/6105)
- [Tracker #8289: OIDC milestone: canonical principals and inbound authentication](https://github.com/zeroclaw-labs/zeroclaw/issues/8289)
- [Tracker #8290: multi-user milestone: per-principal isolation and per-sender authorization](https://github.com/zeroclaw-labs/zeroclaw/issues/8290)
- [PR #10425: internal-principal envelope and separated cron run outcomes](https://github.com/zeroclaw-labs/zeroclaw/pull/10425)
- [Request lifecycle](../request-lifecycle.md)
- [Background work lifecycle](../background-work-lifecycle.md)
- [Tool execution lifecycle](../tool-execution-lifecycle.md)
- [Security model](../../security/model.md)
- `crates/zeroclaw-api/src/ingress.rs`
- `crates/zeroclaw-runtime/src/agent/turn/mod.rs`
- `crates/zeroclaw-runtime/src/security/ingress.rs`
- `crates/zeroclaw-runtime/src/security/audit.rs`
- `crates/zeroclaw-runtime/src/approval/mod.rs`
- `crates/zeroclaw-runtime/src/cron/scheduler.rs`
- `crates/zeroclaw-runtime/src/tools/send_message_to_peer.rs`
- `crates/zeroclaw-runtime/src/tools/spawn_subagent.rs`
