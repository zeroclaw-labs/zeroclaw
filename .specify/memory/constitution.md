<!--
Sync Impact Report
Version change: template (unversioned placeholder) -> 1.0.0
Modified principles:
- Principle 1: trait-driven architecture -> explicit trait/factory-first extension rule
- Principle 2: security posture -> explicit secure-by-default and least-privilege rule
- Principle 3: operational safety -> deterministic fail-fast and explicit boundary failures
- Principle 4: reversibility -> non-breaking scoped changes + rollback-first rule
- Principle 5: product quality -> deterministic/performance/reproducibility rule
Added sections:
- Section 2: Additional constraints for docs governance, security, and runtime contracts
- Section 3: Development workflow for spec-driven work and review gates
- Governance: amendment procedure, versioning policy, and compliance checks
Removed sections:
- None
Templates requiring updates:
- [x] .specify/templates/plan-template.md
- [x] .specify/templates/spec-template.md
- [x] .specify/templates/tasks-template.md
- [x] .specify/templates/checklist-template.md
Follow-up TODOs:
- None
-->
# ZeroClaw Constitution

## Core Principles

### 1. Trait-First Extensibility

ZeroClaw must keep trait interfaces and factory registration as the primary extension
boundary for providers, channels, tools, memory, runtime, and observability.
Extensions must either implement an existing trait contract or add a narrowly
scoped trait with migration and compatibility planning.

Extensions that couple unrelated concerns directly to runtime internals are
forbidden unless a review-approved architecture change explicitly documents a
justified exception.

### 2. Secure by Default and Least Privilege

All execution paths touching provider credentials, command execution, network
routing, filesystem writes, or hardware peripherals must be deny-by-default.
Permission escalations must be explicit, scoped, and auditable through runtime
and observability logs.

No feature may bypass security boundaries by falling through to permissive
fallbacks. If a boundary cannot be evaluated safely, execution must fail and
delegate remediation.

### 3. Explicit Failure and Error Transparency

Behavioral changes must preserve explicit error signaling over silent degradation.
Fallback paths are permitted only when defined and safe, and every fallback is
observable at the same severity level as the originating failure.

When a boundary, provider, or subsystem cannot satisfy input constraints,
processing should fail fast with an actionable diagnostic and a minimal safe
recovery path.

### 4. Reversibility and Deterministic Operation

Project execution should remain reversible through scoped changes, controlled
blast-radius, and explicit rollback paths.

Runtime-relevant work must preserve deterministic behavior unless a feature
explicitly requires stochastic behavior and documents migration, compatibility, and
rollback criteria.

### 5. Lean, Deterministic, and Maintainable Engineering

ZeroClaw prefers small, auditable, low-dependency implementations aligned to
performance and size constraints.

Complexity may only be introduced when required to satisfy a validated user or
security need, and every major introduction must include clear local impact and
maintenance rationale.

## ZeroClaw Runtime and Extension Constraints

- Core modules (`src/*`) and docs contract files are treated as user-facing
  interfaces. Backward compatibility at config and command boundaries is
  prioritized over broad API churn.
- New feature modules must follow existing `src/{providers,channels,tools,memory,security,runtime,peripherals,observability}` trait
  boundaries and factory style.
- High-risk areas include `src/security`, `src/runtime`, `src/gateway`, and
  `src/tools`; changes require explicit threat, risk, and rollback documentation.
- Documentation and reference updates must preserve i18n governance for supported
  locales listed in AGENTS (`en`, `zh-CN`, `ja`, `ru`, `fr`, `vi`, `el`).

## Development, Review, and Test Governance

- All feature work must start with a PRD/spec and spec-tracking linkage.
- Implementation cannot start until required review gates are explicit in the
  feature spec and plan.
- Security-sensitive edits require independent failure-mode and rollback review
  before implementation.
- Runtime and protocol changes must update corresponding contract references in
  `docs/commands-reference.md`, `docs/providers-reference.md`,
  `docs/channels-reference.md`, and `docs/config-reference.md`.
- No behavior-changing PR may proceed without a documented rollback boundary and a
  compliance check on high-risk surfaces.

## Governance

The Constitution is the highest-priority local governance source for Spec Kit
execution. Conflicts are resolved in this order: security posture, deterministic
behavior, minimal blast radius, then feature throughput.

### Amendment Procedure

1. Draft a proposal in `/specs` with affected rules and rationale.
2. Update this constitution in place with version bump and sync impact note.
3. Attach a migration/rollback path and any impacted templates.
4. Review and approve via PR before implementation.

### Versioning Policy

- MAJOR: removes or redefines existing constitutional requirements.
- MINOR: adds new principles or materially expands scope.
- PATCH: clarifies wording, examples, or documentation procedures.
- Constitution versions use MAJOR.MINOR.PATCH semver.

### Compliance Review

- PRDs and plans must cite the specific principle coverage for their scope.
- Reviewers must confirm security, reversibility, and fallback/error behavior.
- Runtime-impacting PRs must link to test coverage or explicit validated
  deferral rationale.

### Governance Files

- Runtime contract surface: `docs/commands-reference.md`, `docs/providers-reference.md`,
  `docs/channels-reference.md`, `docs/config-reference.md`, `docs/operations-runbook.md`.
- Brownfield decomposition: PRD and per-subsystem specs under `docs/specs/`.
- Spec tracker and quality map: `spec-tracking-review.md`.

**Version**: 1.0.0 | **Ratified**: 2026-03-02 | **Last Amended**: 2026-03-02
