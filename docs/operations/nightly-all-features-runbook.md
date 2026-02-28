# Nightly All-Features Runbook

This runbook describes the nightly integration matrix execution and reporting flow.

Primary workflow path: `.github/workflows/feature-matrix.yml` with `profile=nightly`

Legacy/dev-only workflow template: `.github/workflows/nightly-all-features.yml`

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
- Trend snapshot: `nightly-history.json` (latest 3 completed nightly-profile runs)
- Retention: 30 days for lane + summary artifacts

## Scheduler and Activation Notes

- Schedule contract: daily at `03:15 UTC` (`cron: 15 3 * * *`).
- Determinism contract: pinned Rust toolchain (`1.92.0`), locked Cargo commands, explicit apt package install for all-features lane.
- Nightly profile runs are emitted by `feature-matrix.yml`; this keeps manual dispatch and schedule discoverable from the active workflow catalog.

## Ownership Routing and Escalation

Owner routing source: `.github/release/nightly-owner-routing.json`

- `default` -> `@chumyin`
- `whatsapp-web` -> `@chumyin`
- `browser-native` -> `@chumyin`
- `nightly-all-features` -> `@chumyin`

Escalation thresholds:

- Single-lane nightly failure: notify mapped owner within 30 minutes of triage start.
- Same lane fails for 2 consecutive nightly runs: escalate in release governance thread and link both run URLs.
- 3 or more lanes fail in one nightly run: open incident issue and page on-call maintainer.
- Failure unresolved for 24 hours: escalate to maintainers list and block related release promotion tasks.

SLA targets:

- Acknowledge: within 30 minutes during working window.
- Initial diagnosis update: within 4 hours.
- Mitigation PR or rollback decision: within 24 hours.

## Traceability (Last 3 Runs)

Use:

- `gh run list --repo zeroclaw-labs/zeroclaw --workflow feature-matrix.yml --limit 10`
- `gh run view <run_id> --repo zeroclaw-labs/zeroclaw --json jobs,headSha,event,createdAt,url`
- inspect `nightly-history.json` in `nightly-all-features-summary` artifact

Manual trigger (nightly profile):

- `gh workflow run feature-matrix.yml --repo zeroclaw-labs/zeroclaw --ref dev -f profile=nightly -f fail_on_failure=true`

Project update expectation:

- Every weekly status update links the latest 3 nightly runs (URL + conclusion + failed lanes).

## Failure Handling

1. Inspect `nightly-summary.md` for failed lanes and owners.
2. Download the failed lane artifact and rerun the exact command locally.
3. Capture fix PR + test evidence.
4. Link remediation back to release or CI governance issues.
5. If escalation threshold is hit, include escalation ticket/runbook action in the issue update.
