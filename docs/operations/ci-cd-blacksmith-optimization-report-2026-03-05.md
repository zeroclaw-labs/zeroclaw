# CI/CD + Blacksmith Optimization Report

Date: 2026-03-05 (UTC)

## Scope

This report summarizes repository changes applied to implement the CI/CD hardening
and performance plan across security, signal quality, and runtime throughput.

## Implemented Changes

### 1) Release supply-chain hardening

- `pub-release.yml` already installs Syft via pinned installer script with checksum validation (`scripts/ci/install_syft.sh`).
- No remote `curl | sh` Syft install path remains in release workflow.

### 2) Container vulnerability gate before push

- Added pre-push Trivy gate in `.github/workflows/pub-docker-img.yml`.
- New behavior:
  - build local release-candidate image (`linux/amd64`)
  - block publish on `CRITICAL` findings
  - report `HIGH` findings as advisory warnings
- Post-push Trivy evidence collection remains for release/sha/latest parity and audit artifacts.
- Updated policy:
  - `.github/release/ghcr-vulnerability-policy.json` now blocks `CRITICAL` only.
  - `docs/operations/ghcr-vulnerability-policy.md` updated accordingly.

### 3) `pull_request_target` safety contract enforcement

- Added explicit safety contract in `docs/actions-source-policy.md`.
- Extended `scripts/ci/ci_change_audit.py` policy checks to block newly introduced unsafe workflow-script JS patterns in `.github/workflows/scripts/**`:
  - `eval(...)`
  - `Function(...)`
  - `vm.runInContext/runInNewContext/runInThisContext/new vm.Script`
  - dynamic `child_process` execution APIs
- Added/updated tests in `scripts/ci/tests/test_ci_scripts.py`.

### 4) Branch protection baseline documentation

- Added `docs/operations/branch-protection.md` with:
  - protected branch baseline (`dev`, `main`)
  - required checks and branch rules
  - export commands for live policy snapshots
- Added snapshot directory doc:
  - `docs/operations/branch-protection/README.md`
- Linked baseline in:
  - `docs/pr-workflow.md`
  - `docs/operations/required-check-mapping.md`

### 5) PR lint/test defaults and CI signal quality

- `ci-run.yml` already runs lint/test/build by default for Rust-impacting PRs (no `ci:full` label requirement).
- Updated stale workflow docs (`.github/workflows/main-branch-flow.md`) to reflect actual behavior.

### 6) Binary size guardrails (PR + release parity)

- Added Windows binary size enforcement in `.github/workflows/pub-release.yml`.
- Added PR binary-size regression job in `.github/workflows/ci-run.yml`:
  - compares PR head binary vs base SHA binary
  - default max allowed increase: `10%`
  - fails PR merge gate when threshold is exceeded
- Added helper script:
  - `scripts/ci/check_binary_size_regression.sh`

### 7) Blacksmith throughput and cache contention

- Heavy CI jobs continue to run on Blacksmith-tagged runners.
- Scoped Docker build cache keys added in `.github/workflows/pub-docker-img.yml`:
  - separate scopes for PR smoke and release publish paths
  - reduced cache contention across event types.

### 8) CI telemetry improvements

- Added per-job telemetry summaries in `ci-run.yml` for lint/test/build/binary-size lanes:
  - rust-cache hit/miss output
  - job duration (seconds)
- Added binary-size regression summary output to step summary.

### 9) Coverage follow-up (non-blocking)

- Added non-blocking coverage workflow:
  - `.github/workflows/test-coverage.yml`
  - uses `cargo-llvm-cov` and uploads `lcov.info`
  - does not gate merge by default.

### 10) Developer experience follow-up

- Added Windows bootstrap entrypoint:
  - `bootstrap.ps1`
- Updated setup docs:
  - `README.md`
  - `docs/one-click-bootstrap.md`
- Added release note category config:
  - `.github/release.yml`
- Updated release docs:
  - `docs/release-process.md`

## Validation Performed

- Targeted CI policy tests:
  - `python3 -m unittest -k ci_change_audit scripts.ci.tests.test_ci_scripts`
  - result: pass (8 tests)
  - note: executed with hooks disabled via:
    - `GIT_CONFIG_COUNT=1`
    - `GIT_CONFIG_KEY_0=core.hooksPath`
    - `GIT_CONFIG_VALUE_0=/dev/null`
- Script syntax checks:
  - `bash -n scripts/ci/check_binary_size_regression.sh` (pass)
  - `python3 -m py_compile scripts/ci/ci_change_audit.py scripts/ci/ghcr_vulnerability_gate.py` (pass)
- Diff hygiene:
  - `git diff --check` (pass)

## Known Follow-up

- Live branch protection JSON export is documented but not committed in this change set
  because local `gh` authentication token is currently invalid.
  After re-authentication, run export commands in `docs/operations/branch-protection.md`
  and commit:
  - `docs/operations/branch-protection/dev-protection.json`
  - `docs/operations/branch-protection/main-protection.json`
  - `docs/operations/branch-protection/rulesets.json` (if applicable)
