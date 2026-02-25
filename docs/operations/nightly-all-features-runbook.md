# Nightly All-Features Runbook

This runbook describes the nightly integration matrix execution and reporting flow.

Workflow: `.github/workflows/nightly-all-features.yml`

## Objective

- Continuously validate high-risk feature combinations overnight.
- Produce machine-readable and human-readable reports for rapid triage.

## Lanes

- `default`
- `whatsapp-web`
- `browser-native`
- `nightly-all-features`

Lane owners are configured in `.github/release/nightly-owner-routing.json`.

## Artifacts

- Per-lane: `nightly-lane-<lane>` with `nightly-result-<lane>.json`
- Aggregate: `nightly-all-features-summary` with `nightly-summary.json` and `nightly-summary.md`

## Failure Handling

1. Inspect `nightly-summary.md` for failed lanes and owners.
2. Download the failed lane artifact and rerun the exact command locally.
3. Capture fix PR + test evidence.
4. Link remediation back to release or CI governance issues.
