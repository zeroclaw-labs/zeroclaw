# Feature Matrix Runbook

This runbook defines the feature matrix CI lanes used to validate key compile combinations.

Workflow: `.github/workflows/feature-matrix.yml`

## Lanes

- `default`: `cargo check --locked`
- `whatsapp-web`: `cargo check --locked --no-default-features --features whatsapp-web`
- `browser-native`: `cargo check --locked --no-default-features --features browser-native`
- `nightly-all-features`: `cargo check --locked --all-features`

## Triggering

- PRs and pushes to `dev` / `main` on Rust + workflow paths
- merge queue (`merge_group`)
- weekly schedule
- manual dispatch

## Artifacts

- Per-lane report: `feature-matrix-<lane>`
- Aggregated report: `feature-matrix-summary` (`feature-matrix-summary.json`, `feature-matrix-summary.md`)

## Failure Triage

1. Open `feature-matrix-summary.md` and identify failed lane(s).
2. Download lane artifact (`nightly-result-<lane>.json`) for exact command + exit code.
3. Reproduce locally using the reported command.
4. Attach reproduction output to the corresponding Linear execution issue.
