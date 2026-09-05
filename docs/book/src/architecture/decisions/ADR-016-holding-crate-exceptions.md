---
id: ADR-016
title: Holding-crate exceptions are bounded, recorded, and granted by the Core Team
date: 2026-09-02
status: proposed
relates-to:
  - ADR-007
  - crates/zeroclaw-runtime/AGENTS.md
  - docs/book/src/foundations/fnd-001-intentional-architecture.md
  - https://github.com/zeroclaw-labs/zeroclaw/pull/10557
  - https://github.com/zeroclaw-labs/zeroclaw/pull/10410
  - https://github.com/zeroclaw-labs/zeroclaw/pull/10179
---

# ADR-016: Holding-Crate Exceptions Are Bounded, Recorded, and Granted by the Core Team

## Context

`crates/zeroclaw-runtime/AGENTS.md` declares that crate a transitional holding area, instructs contributors not to add new functionality there, and names the subsystems awaiting extraction. The instruction is unconditional and has no exception process.

That combination has no answer for the ordinary case: accepted work lands on a subsystem that still lives in the holding crate, and the crate it is supposed to move to has not been built. The contract forbids the only available home, and the destination does not exist yet. Three pull requests reached that state and resolved it three different ways.

The cron precondition gate went through [#10220](https://github.com/zeroclaw-labs/zeroclaw/pull/10220), then a proposed one-off exception, and finally the full extraction in [#10557](https://github.com/zeroclaw-labs/zeroclaw/pull/10557). The extraction was the right outcome, but it was reached by building two complete alternatives and discarding one. A contributor should be able to establish whether extraction is required before implementing it twice.

[#10410](https://github.com/zeroclaw-labs/zeroclaw/pull/10410) kept shared config and agent-lifecycle coordination in the runtime rather than invent a lifecycle crate ahead of the planned daemon extraction. Moving that code to `zeroclaw-infra` would invert an existing dependency, since config already depends on infra. Extracting early would therefore establish a boundary the roadmap does not want. That decision is still waiting on repository-level acceptance.

[#10179](https://github.com/zeroclaw-labs/zeroclaw/pull/10179) hit the same rule, but its transport had no receiving caller. Retirement, or an explicit ownership decision, was the better answer there than any exception.

Those three cases differ in kind, not just in size. Extraction was proportionate for one, would have produced the wrong boundary for another, and was beside the point for the third. A single unconditional instruction cannot separate them, and silence about exceptions has meant each contributor guesses.

The alternatives are to keep the instruction unconditional and require extraction in every case, to relax it generally, or to keep extraction as the default and define how a bounded exception is granted.

## Decision

Extraction remains the default. The Core Team may approve a bounded exception when immediate extraction would require a disproportionate refactor, or would establish a crate boundary the roadmap does not intend.

### What an exception must state

An approved exception names all four of:

- **Permitted scope.** The specific paths the exception covers. Not a subsystem in the abstract.
- **Intended destination.** The crate the code is expected to move to, so the exception describes a delay rather than a reversal.
- **Approving authority.** Who granted it.
- **Expiry or review condition.** What ends it: a named extraction landing, a release, or a review date.

### How it is granted

The record is created before the feature merges, and separately from it. A feature pull request cannot grant itself an exception, because the contract it would be waiving is the one constraining it.

An exception requires a concrete supported use case. Code with no receiving caller does not qualify; retirement or an explicit ownership decision is the correct answer there.

### What an exception is not

An exception permits continued work on a subsystem the holding crate already contains. It never permits introducing a new subsystem there, and it does not generalise from one subsystem to another. Granting one for cron says nothing about the daemon.

## Consequences

Contributors get a decision path that can be resolved before implementation rather than after. The cron case cost two complete implementations to answer a question that a short record could have settled first.

Every exception is visible and dated. The active-exception table in the holding-crate contract makes the accumulated debt legible, and each entry names what ends it, so an exception that has quietly become permanent is apparent rather than buried.

The Core Team takes on judging proportionality case by case. That is deliberate: the three cases above show the judgment cannot be reduced to a size threshold, because "disproportionate refactor" and "wrong boundary" are different reasons that happen to share a symptom.

The holding-crate instruction keeps its force. This does not weaken it; it supplies the process the instruction assumed but never defined.

## Acceptance

ADR-016 remains proposed until:

- `crates/zeroclaw-runtime/AGENTS.md` states the exception rule and carries an active-exception table;
- at least one exception has been granted or refused through this process, demonstrating it is usable rather than only written down.
