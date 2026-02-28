# Canary Gate Runbook

Workflow: `.github/workflows/ci-canary-gate.yml`
Policy: `.github/release/canary-policy.json`

## Inputs

- candidate tag + optional SHA
- observed error rate
- observed crash rate
- observed p95 latency
- observed sample size
- `trigger_rollback_on_abort` (workflow_dispatch only, default `true`)
- `rollback_branch` (workflow_dispatch only, default `dev`)
- `rollback_target_ref` (optional explicit rollback target ref)

## Cohort Progression

Defined in `.github/release/canary-policy.json`:

- `canary-5pct` for 20 minutes
- `canary-20pct` for 20 minutes
- `canary-50pct` for 20 minutes
- `canary-100pct` for 60 minutes (final confidence window)

Promotion guidance:

1. Run `dry-run` for each cohort window first.
2. Promote to next cohort only when decision is `promote`.
3. Stop progression on `hold` and open investigation.
4. Trigger rollback flow on `abort`.

## Decision Model

- `promote`: all metrics within configured thresholds
- `hold`: soft breach or policy violations (for example insufficient sample)
- `abort`: hard breach (`>1.5x` threshold)

## Observability Signals

Guarded policy signals:

- `error_rate`
- `crash_rate`
- `p95_latency_ms`
- `sample_size`

All signals are emitted in `canary-guard.json` and rendered in run summary.

## Execution Modes

- `dry-run`: generate decision + artifacts only
- `execute`: allow marker tag + optional repository dispatch

## Abort-to-Rollback Integration

When `decision=abort` and `trigger_rollback_on_abort=true`, `CI Canary Gate` dispatches `.github/workflows/ci-rollback.yml` automatically with guarded execute inputs.

Dispatched rollback defaults:

- `branch`: workflow input `rollback_branch` (default `dev`)
- `mode`: `execute`
- `allow_non_ancestor`: `false`
- `fail_on_violation`: `true`
- `create_marker_tag`: `true`
- `emit_repository_dispatch`: `true`
- `target_ref`: optional (`rollback_target_ref`), otherwise rollback guard uses latest release tag strategy

The canary run summary emits a `Canary Abort Rollback Trigger` section to make dispatch behavior auditable.

## Artifacts

- `canary-guard.json`
- `canary-guard.md`
- `audit-event-canary-guard.json`

## Operational Guidance

1. Use `dry-run` first for every candidate.
2. Never execute with sample size below policy minimum.
3. For `abort`, include root-cause summary in release issue and keep candidate blocked.
4. For `abort` with auto-trigger enabled, verify the linked `CI Rollback Guard` run completed and review `ci-rollback-plan` artifacts.
