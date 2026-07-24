---
id: ADR-008
title: Anchor goal mode in the durable task control plane
date: 2026-06-25
status: accepted
relates-to:
  - https://github.com/zeroclaw-labs/zeroclaw/issues/8303
  - https://github.com/zeroclaw-labs/zeroclaw/issues/7929
  - https://github.com/zeroclaw-labs/zeroclaw/pull/8217
  - crates/zeroclaw-runtime
  - crates/zeroclaw-config
  - crates/zeroclaw-tools
---

# ADR-008: Anchor goal mode in the durable task control plane

## Context

Goal mode changes an agent interaction from a single response into autonomous
work against a user objective. A goal can outlive the inbound message that
started it, pause while waiting for input or an external dependency, recover
after daemon restart, and spend model usage across multiple worker, verifier,
or delegated calls.

That kind of work needs one durable authority for its lifecycle. The system must
be able to answer which work is alive, which work is eligible to run another
turn, which work is paused, which work is terminal, and which actor or route is
allowed to resume or cancel it. A prompt convention or side registry would make
those answers dependent on duplicated state.

Budget accounting adds another source-of-truth constraint. Tokens and cost are
facts produced by model-provider calls. Remaining budget is derived from a
limit and consumed usage; storing it as its own mutable counter would create a
second accounting system.

Goal mode also crosses a trust boundary. A slash command and a model-generated
request can both express that a goal should start, but neither should be able to
self-authorize trusted runtime facts such as caller identity, route, channel, or
agent eligibility.

The main options considered were ordinary multi-turn chat with prompt guidance,
a separate goal subsystem, or anchoring goal mode in the existing durable task
control plane with goal-specific extension state.

## Decision

We will anchor goal mode in the durable task control plane instead of building a
parallel goal task system.

The task control plane is the source of truth for goal lifecycle, ownership,
route, principal, parent relation, and recovery eligibility. Goal-specific state
may extend task records, but it must not duplicate lifecycle, identity, route,
timestamp, delivery, or terminal-state facts owned by the task record.

Consumed model usage remains owned by the canonical usage ledger. Goal budgets
are limits interpreted against usage records; remaining budget is always
derived, never stored as a second mutable total.

Pause, resume, cancellation, restart recovery, and completion are control-plane
policy decisions. Prompt text can explain or request those transitions, but it
does not authorize them.

All goal entry paths use one trusted Rust-side admission gate. Runtime context
supplies trusted facts such as surface, channel, principal, route, and agent
eligibility; model-supplied arguments cannot declare those facts.

Goal completion requires an explicit completion decision by the goal controller.
Silence, lack of further tool calls, or a final-looking assistant message is not
enough to mark durable autonomous work complete.

## Consequences

Goal mode inherits the existing supervised task lifecycle instead of adding a
second task registry. This keeps restart recovery, cancellation, and future
supervision improvements attached to one control-plane contract.

The control plane becomes responsible for distinguishing paused but resumable
goal work from terminal, lost, or still-running work. That increases the
importance of precise task status transitions and restart recovery policy.

Budget enforcement becomes auditable because consumed usage is derived from
canonical usage records. It also means goal mode cannot be correct until every
model call that can spend against a goal reports usage with goal attribution.

Delegation and background work must respect the same lifecycle and usage
boundaries. Work that cannot report completion and usage back to the owning goal
cannot safely participate in goal mode.

A single trusted admission gate prevents command and model-initiated entry paths
from becoming separate authorization systems. The cost is that every supported
surface must route goal entry through the shared command/admission machinery.

Detailed implementation choices, including command syntax, pause reason
inventory, payload shapes, verifier configuration, and rollout staging, are left
to implementation plans and follow-up PRs. They must remain consistent with this
ADR's source-of-truth boundaries.

## References

- RFC #8303: <https://github.com/zeroclaw-labs/zeroclaw/issues/8303>
- Shared command-catalogue RFC #7929: <https://github.com/zeroclaw-labs/zeroclaw/issues/7929>
- Durable task control plane PR #8217: <https://github.com/zeroclaw-labs/zeroclaw/pull/8217>
- Control-plane commit: <https://github.com/zeroclaw-labs/zeroclaw/commit/607d69ef44dca07e2605e822db13fb437b462f4a>
- Gateway ask-user/free-form gap #7776: <https://github.com/zeroclaw-labs/zeroclaw/issues/7776>
- Cached token and cost accounting #7248: <https://github.com/zeroclaw-labs/zeroclaw/issues/7248>
- Command localization gap #6548: <https://github.com/zeroclaw-labs/zeroclaw/issues/6548>
