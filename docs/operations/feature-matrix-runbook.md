# Feature Matrix Runbook

This runbook defines the feature matrix CI lanes used to validate key compile combinations.

Workflow: `.github/workflows/feature-matrix.yml`

Profiles:

- `compile` (default): merge-gate compile combinations
- `nightly`: integration-oriented nightly lane commands + trend snapshot

## Lanes

- `default`: `cargo check --locked`
- `whatsapp-web`: `cargo check --locked --no-default-features --features whatsapp-web`
- `browser-native`: `cargo check --locked --no-default-features --features browser-native`
- `nightly-all-features`: `cargo check --locked --all-features`

## Triggering

- PRs and pushes to `dev` / `main` on Rust + workflow paths
- merge queue (`merge_group`)
- weekly schedule (`compile`)
- daily schedule (`nightly`)
- manual dispatch (`profile=compile|nightly`)

## Artifacts

- Per-lane report (`compile`): `feature-matrix-<lane>`
- Per-lane report (`nightly`): `nightly-lane-<lane>`
- Aggregated report: `feature-matrix-summary` (`feature-matrix-summary.json`, `feature-matrix-summary.md`)
- Retention: 21 days for lane + summary artifacts
- Nightly profile summary artifact: `nightly-all-features-summary` (`nightly-summary.json`, `nightly-summary.md`, `nightly-history.json`) with 30-day retention

## Retry Policy

- `compile` profile: max attempts = 1
- `nightly` profile: max attempts = 2 (bounded single retry)
- Lane artifacts record `attempts_used` and `max_attempts` for auditability

## Required Check Contract

Branch protection should use stable, non-matrix-expanded check names for merge gates:

- `Feature Matrix Summary` (from `feature-matrix.yml`)

Matrix lane jobs stay observable but are not required check targets:

- `Matrix Lane (default)`
- `Matrix Lane (whatsapp-web)`
- `Matrix Lane (browser-native)`
- `Matrix Lane (nightly-all-features)`

Check-name stability rule:

- Do not rename the job names above without updating `docs/operations/required-check-mapping.md`.
- Keep lane names in the matrix include-list stable to avoid check-name drift.

Verification commands:

- `gh run list --repo zeroclaw-labs/zeroclaw --workflow feature-matrix.yml --limit 3`
- `gh run view <run_id> --repo zeroclaw-labs/zeroclaw --json jobs --jq '.jobs[].name'`

## Failure Triage

1. Open `feature-matrix-summary.md` and identify failed lane(s), owner, and failing command.
2. Download lane artifact (`nightly-result-<lane>.json`) for exact command + exit code.
3. Reproduce locally with the exact command and toolchain lock (`--locked`).
4. Attach local reproduction logs + fix PR link to the active Linear execution issue.

## High-Frequency Failure Classes

| Failure class | Signal | First response | Escalation trigger |
| --- | --- | --- | --- |
| Rust dependency lock drift | `cargo check --locked` fails with lock mismatch | run `cargo update -p <crate>` only when needed; regenerate lockfile in focused PR | same lane fails on 2 consecutive runs |
| Feature-flag compile drift (`whatsapp-web`) | missing symbols or cfg-gated modules | run the lane command locally and inspect feature-gated module imports | unresolved in 24h |
| Feature-flag compile drift (`browser-native`) | platform/feature binding compile errors | inspect browser-native cfg paths and recent dependency bumps | unresolved in 24h |
| System package dependency drift (`nightly-all-features`) | missing `libudev`/`pkg-config` or linker errors | verify apt install step succeeded; rerun in clean container with same deps | recurs 3 times in 7 days |
| CI environment/runtime regressions | lane timeout or infrastructure transient failure | re-run once, compare with prior successful run, then isolate infra vs code | 2+ lanes impacted in one run |
| Summary aggregation contract break | `Feature Matrix Summary` fails to parse artifacts | verify artifact names + JSON schema from lane outputs | any merge-gate failure on protected branches |

## Debug Data Expectations

- Lane JSON must include: lane, status, exit_code, duration_seconds, command.
- Summary JSON must include: total, passed, failed, per-lane rows, owner routing.
- Preserve artifacts for at least one full release cycle (21 days currently configured).
