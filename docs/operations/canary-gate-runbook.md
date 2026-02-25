# Canary Gate Runbook

Workflow: `.github/workflows/ci-canary-gate.yml`
Policy: `.github/release/canary-policy.json`

## Inputs

- candidate tag + optional SHA
- observed error rate
- observed crash rate
- observed p95 latency
- observed sample size

## Decision Model

- `promote`: all metrics within configured thresholds
- `hold`: soft breach or policy violations (for example insufficient sample)
- `abort`: hard breach (`>1.5x` threshold)

## Execution Modes

- `dry-run`: generate decision + artifacts only
- `execute`: allow marker tag + optional repository dispatch

## Artifacts

- `canary-guard.json`
- `canary-guard.md`
- `audit-event-canary-guard.json`

## Operational Guidance

1. Use `dry-run` first for every candidate.
2. Never execute with sample size below policy minimum.
3. For `abort`, include root-cause summary in release issue and keep candidate blocked.
