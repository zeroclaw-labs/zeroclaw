# PRD: ZeroClaw Runtime Specification Recovery and Controlled Evolution

This initiative applies to the ZeroClaw runtime framework as described in the repository [README](README.md) and [docs hub](docs/README.md).

## 1) Problem
ZeroClaw has grown into a modular but tightly coupled runtime with many high-risk boundary surfaces. There is no single spec-backed, auditable map of current behavioral contracts across gateway, providers, channels, tools, memory, security, runtime adapters, and observability. This causes hidden contract drift and increases risk during future changes.

## 2) Goals
- Capture current behavior comprehensively in Spec Kit format without changing execution.
- Decompose by stable boundaries (traits + factories + config contracts).
- Create explicit acceptance criteria for each subsystem and shared risk surface.
- Enable future PRs to be scoped against one or more specs with clear review gates.
- Produce a spec coverage map for architectural confidence and maintenance.

### Dependencies and stakeholders
Spec Kit format and repo structure; owners per spec as in spec-tracking-review. Product and docs context as in README and docs/README.md.

## 3) Success Metrics
- 100% of listed core seams has at least one spec.
- Each spec includes at least one acceptance criterion derived from existing code/tests.
- `spec-tracking-review.md` marks no subsystem as “unclassified”. (Unclassified means: a core seam in the boundary map in Architecture-Reference lacks a corresponding spec and risk tier in spec-tracking-review.)
- No change in runtime behavior for current brownfield baseline.

## 4) In-Scope
- Specification-only output for existing systems:
  - CLI + config contract
  - agent runtime orchestration
  - providers
  - channels/gateway
  - tools and tool governance
  - memory subsystem
  - security and pairing/policy
  - observability/metrics/logging
  - runtime adapters
  - plugins
  - peripherals
- Cross-cutting spec: docs governance and i18n impact tracking (inclusion at maintainer discretion; when included, follows the same format and tracking as other specs).

## 5) Out-of-Scope
- New feature implementation.
- New config keys.
- API breaking changes.
- Dependency graph refactors.
- New external network integrations unless already present.

## 6) Functional Requirements
- Each spec must use stable boundary language:
  - "current behavior"
  - "desired contract"
  - "non-disruptive implementation path"
- Every spec must include:
  - explicit success criteria
  - failure modes
  - migration/rollback notes for future implementation.
- Specs must be readable independently yet linkable to related specs.

## 7) Non-Functional Requirements
- Deterministic language and phrasing.
- No speculative design branches.
- Prefer explicit error/unsupported-state behavior language.
- Security-sensitive surfaces (gateway/security/tools/runtime) include threat and rollback notes.

## 8) Risks
- Overlapping language across specs for cross-module flows.
- Hidden coupling in plugin paths if spec scope is not explicit.
- Missing locale docs follow-through if later this PR touches shared documentation entrypoints.

## 9) Acceptance Criteria
- The following three deliverables plus all subsystem specs are generated and internally consistent: Architecture-Reference.md, spec-tracking-review.md, and the docs/specs index (docs/specs/README.md) plus all subsystem spec files in docs/specs/.
- `Architecture-Reference.md` includes at least one execution path from inbound message to tool memory/log emission.
- Initiative is complete when all in-scope specs exist, are linked from spec-tracking-review, and Architecture-Reference plus spec-tracking-review are reviewed and merged.
