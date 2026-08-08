---
id: ADR-014
title: Execution-tree iteration budgets are owned by the root runtime profile
date: 2026-07-27
status: proposed
relates-to:
  - https://github.com/zeroclaw-labs/zeroclaw/issues/9323
  - https://github.com/zeroclaw-labs/zeroclaw/pull/9201
  - ADR-011
  - docs/book/src/agents/delegation.md
  - crates/zeroclaw-runtime/src/agent/loop_.rs
---

# ADR-014: Execution-Tree Iteration Budgets Are Owned by the Root Runtime Profile

## Context

ZeroClaw limits each agent loop with `max_tool_iterations` from that agent's effective runtime profile. That local limit does not bound aggregate work when a root loop creates children through `spawn_subagent`, bounded `delegate` calls, or another nested execution path. Parallel children can each consume their own local allowance, so a bounded root loop can still create an execution tree whose total work is not bounded by the root's policy.

The runtime contains a dormant optional `shared_budget` handle. Production roots currently pass `None`, child construction does not establish a shared owner consistently, and the existing surface has no configured lifecycle contract. [#9201](https://github.com/zeroclaw-labs/zeroclaw/pull/9201) hardens the atomic reservation primitive but intentionally does not select an owner or change production behavior.

RFC [#9323](https://github.com/zeroclaw-labs/zeroclaw/issues/9323) proposes separating the per-loop limit from an aggregate execution-tree limit. This record defines who would own that aggregate limit, which descendants would participate, and how exhaustion would reach the root without conflating iterations with delegation depth or child concurrency.

## Decision

### Keep local and tree limits distinct

Every agent loop continues to enforce its effective runtime profile's `max_tool_iterations`. A root execution may additionally enforce `max_execution_tree_iterations`, configured on the root's effective runtime profile. The tree limit is an aggregate reservation pool; it does not replace or raise any participating loop's local limit.

An omitted or inherited unset tree limit preserves current behavior and disables aggregate accounting for that execution. When configured, the value must be at least two: one work iteration plus the retained root-finalization reservation defined below. An explicitly configured value below two is invalid rather than another sentinel or an implicit fallback.

A participating loop may proceed only while both its local allowance and the shared tree allowance permit another iteration. A child whose local limit is larger than the root's local limit may use that larger local limit, but it cannot exceed the tree's remaining aggregate allowance.

Delegation depth and concurrent child count remain separate controls. An iteration budget must not be used as an implicit depth or fan-out limit, and those limits must not be inferred from its configured value.

### Classify tree membership by lifecycle

Every agent-loop construction declares one of two lifecycle relationships: it starts a root tree or joins an existing tree. A top-level entry point with no enclosing agent execution starts a root. A nested execution that is awaited and whose result returns to the caller before that caller completes joins the caller's tree. A background execution that returns a task handle and may outlive the caller starts a new root tree.

The production entry point that starts a root resolves `max_execution_tree_iterations` once and creates one canonical budget boundary for that tree. Joined descendants receive a descendant view of the same budget; they must not re-read configuration, copy a numeric snapshot into a new counter, or create a replacement budget for the same tree. Constructors must require the lifecycle relationship explicitly rather than infer membership from an optional handle.

Participation follows execution ownership:

- synchronous `spawn_subagent` children share the caller's tree budget;
- synchronous and parallel `delegate` children share the caller's tree budget whether their authority mode is bounded or independent, while keeping each target agent's local runtime profile;
- `background: true` delegation starts a new tree budget from the target's effective profile whether its authority mode is bounded or independent; and
- awaited live SOP nested steps join the enclosing tree, while top-level or independently scheduled SOP work starts a new root.

Bounded versus independent delegation controls the child's authority and policy inheritance. Synchronous versus background execution controls tree membership. Parallel scheduling does not change membership. New nested execution paths must make the same lifecycle classification; accidental inheritance or omission is not a valid ownership decision.

A non-agentic `delegate` call performs one provider request without creating a child tool loop. It does not claim a separate execution-tree iteration because the parent iteration that invokes the tool is already charged. Provider cost, action, and concurrency controls remain responsible for that request. If aggregate provider-call accounting is needed, it requires a separate budget rather than redefining a tool-loop iteration.

### Preserve a root finalization reservation

The canonical budget boundary exposes distinct root and descendant reservation capabilities. Once the root starts joined descendant work, it retains one tree reservation that a descendant capability cannot consume and preserves one remaining iteration from its own `max_tool_iterations`. A root that cannot preserve both reservations must refuse to start joined child work. This guarantees that child exhaustion does not, by itself, prevent the root from receiving child outcomes and producing a coherent final response. An undifferentiated shared atomic counter is insufficient for the tree-reservation rule.

The retained tree reservation is part of the configured aggregate cap, not an extra iteration added above it. Together with the preserved local iteration, it permits one tools-disabled root continuation after descendant outcomes are available or after the shared work pool is exhausted; it cannot start more child work. Both reservations may remain unused when the root completes without needing that continuation.

Each ordinary root or descendant iteration claims from the shared work pool immediately before its provider request begins. The root claims its retained tree reservation at the same point when it begins the final continuation. Cancellation before a successful claim consumes nothing. Once the provider request begins, the claim remains spent even if the request is cancelled or fails; claims are not refunded after externally observable work may have started. Failure of the one-shot final continuation returns a typed root error rather than retrying outside either cap.

### Report exhaustion without implicit tree cancellation

Failure to reserve another tree iteration produces a typed exhausted outcome for the affected child or nested path. Exhaustion does not automatically cancel siblings or the root. The root decides whether to continue with partial results, cancel remaining work, or return an error according to the calling workflow.

Cancellation and budget exhaustion remain distinct signals. Cancellation stops work according to the execution lifecycle; exhaustion denies further reservations. A cancellation that wins before reservation prevents the claim; a reservation that wins first follows the no-refund rule above. Implementations must handle these races without underflow, double reservation, or work continuing after either signal should have stopped it.

### Gate acceptance on production wiring

This ADR remains proposed until all of these conditions are met:

- the root runtime profile exposes and validates the optional `max_execution_tree_iterations` source, and canonical constructors require every agent execution to start or join a tree;
- production roots create one budget boundary, joined paths share its descendant capability, and background paths create an independent root budget across the delegation and SOP lifecycles described above, while non-agentic delegation remains covered by its parent iteration;
- deterministic production-path tests prove the configured aggregate total, descendant exclusion from the retained reservation, preservation of the root's final local iteration, a child's local limit larger than the root's local limit, root finalization after child or root work-pool exhaustion, one-shot finalization failure, typed exhaustion, and cancellation races before and after reservation; and
- operator documentation distinguishes local iteration limits, execution-tree limits, delegation depth, and concurrent child limits.

If the production contract is not implemented, the dormant `shared_budget` field and propagation surface should be removed rather than retained as an unowned speculative mechanism.

## Consequences

Positive consequences:

- Operators can bound total nested agent work without forcing every agent to use the root's local loop limit.
- All joined descendants consume one visible source of truth rather than independently reconstructed counters.
- The root can finish coherently when descendants exhaust the aggregate allowance.
- Background work has an explicit independent lifecycle instead of accidentally draining or escaping its caller's budget.

Negative consequences:

- Every production entry point and nested execution path must classify its tree ownership explicitly.
- Parallel reservations, cancellation, and partial completion require typed outcomes and concurrency tests.
- A tree-wide limit adds another runtime-profile setting that operators must distinguish from local iterations, depth, and fan-out.
- Reserving root finalization capacity means children cannot consume the full configured total themselves.

## References

- [RFC #9323: Define execution-tree iteration budget ownership](https://github.com/zeroclaw-labs/zeroclaw/issues/9323)
- [PR #9201: Harden dormant shared iteration reservation](https://github.com/zeroclaw-labs/zeroclaw/pull/9201)
- [ADR-011: Configured agents have explicit runtime boundaries under one daemon](./ADR-011-multi-agent-runtime-boundaries.md)
- [Delegation and subagents](../../agents/delegation.md)
- `crates/zeroclaw-runtime/src/agent/loop_.rs`
- `crates/zeroclaw-runtime/src/tools/spawn_subagent.rs`
- `crates/zeroclaw-runtime/src/tools/delegate.rs`
