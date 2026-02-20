# Architecture Remediation Plan (2026-02-20)

## Scope
This plan targets clarity, redundancy, and runtime performance risks identified in current `src/channels`, `src/providers`, and CLI wiring.

## Current Problems

### P0: Behavioral drift from duplicated channel construction
- `channel doctor` and `channel start` construct channels through separate, duplicated code paths.
- A concrete mismatch already exists: Mattermost is started but not checked by doctor.

### P1: Potential async worker blocking during provider creation
- Provider initialization can trigger OAuth refresh using blocking HTTP paths.
- This can run in message-serving runtime paths and risks latency spikes under concurrency.

### P1: Command definition duplication between `main` and `lib`
- Multiple subcommand enums are defined in both places.
- This increases maintenance cost and drift risk for CLI contract changes.

### P2: Hot-path collection patterns with avoidable overhead
- History maintenance uses head-removal and frequent cloning in places that may scale with traffic.

## Design Direction

### Phase A: Unify channel construction (first)
- Extract a shared builder for configured channels.
- Reuse it in both runtime startup and doctor flows.
- Keep naming/labels separate from construction details if needed.

Expected outcome:
- Removes drift class of bugs.
- Fixes current Mattermost doctor inconsistency.

### Phase B: Isolate or eliminate blocking provider refresh in async paths
- Move OAuth refresh to async HTTP client where feasible.
- If full async conversion is not practical in one patch, isolate blocking sections explicitly with controlled boundaries.

Expected outcome:
- Reduces runtime stalls and tail latency risk.

### Phase C: Single source of truth for CLI command enums
- Keep command contract types in one module (`lib`), with `main` only routing and glue logic.

Expected outcome:
- Lower contract drift risk and reduced maintenance overhead.

### Phase D: Optimize history buffer operations
- Replace head-removal vector patterns with queue/ring style storage where appropriate.
- Keep behavior unchanged while lowering per-message overhead.

Expected outcome:
- More stable performance under high message volume.

## Validation Plan
- Unit/integration tests for channel builder reuse and doctor/start parity.
- Regression test for Mattermost doctor visibility.
- Focused tests around provider initialization behavior in async runtime.
- Standard checks:
  - `cargo fmt --all -- --check`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test`

## Rollback Strategy
- Land each phase as an isolated PR.
- Revert by phase if needed (`git revert <commit>`), minimizing blast radius.
- Avoid mixing refactors with behavior changes in one patch.

## Non-goals for this plan file
- No functional code change in this commit.
- No config schema change.
- No provider/channel feature expansion.
