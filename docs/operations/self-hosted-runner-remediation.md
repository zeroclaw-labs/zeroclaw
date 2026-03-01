# Self-Hosted Runner Remediation Runbook

This runbook provides operational steps for self-hosted runner capacity incidents.

## Scope

Use this when CI jobs remain queued, runner availability drops, or runner hosts fill disk.

## Scripts

- `scripts/ci/runner_health_report.py`
  - Queries GitHub Actions runner state and workflow queue pressure.
  - Produces console summary and optional JSON report.
- `scripts/ci/runner_disk_cleanup.sh`
  - Reclaims stale runner workspace/temp/diag files.
  - Defaults to dry-run mode and requires explicit `--apply`.
- `scripts/ci/queue_hygiene.py`
  - Removes queued-run backlog from obsolete workflows and stale duplicate runs.
  - Defaults to dry-run mode; use `--apply` to execute cancellations.

## 1) Health Check

```bash
python3 scripts/ci/runner_health_report.py \
  --repo zeroclaw-labs/zeroclaw \
  --require-label self-hosted \
  --require-label aws-india \
  --min-online 3 \
  --min-available 1 \
  --max-queued-runs 20 \
  --output-json artifacts/runner-health.json
```

Auth note:

- The script reads token from `--token`, then `GH_TOKEN`/`GITHUB_TOKEN`, then falls back to `gh auth token`.

Recommended alert thresholds:

- `online < 3` (critical)
- `available < 1` (critical)
- `queued runs > 20` (critical)
- `busy ratio > 90%` (warning)

## 2) Disk Cleanup (Dry-Run First)

```bash
scripts/ci/runner_disk_cleanup.sh \
  --runner-root /home/ubuntu/actions-runner-pool \
  --work-retention-days 2 \
  --diag-retention-days 7
```

Apply mode (after draining jobs):

```bash
scripts/ci/runner_disk_cleanup.sh \
  --runner-root /home/ubuntu/actions-runner-pool \
  --work-retention-days 2 \
  --diag-retention-days 7 \
  --apply
```

Optional with Docker cleanup:

```bash
scripts/ci/runner_disk_cleanup.sh \
  --runner-root /home/ubuntu/actions-runner-pool \
  --apply \
  --docker-prune
```

Safety behavior:

- `--apply` aborts if runner worker/listener processes are detected, unless `--force` is provided.
- default mode is non-destructive.

## 3) Recovery Sequence

1. Pause or reduce non-blocking workflows if queue pressure is high.
2. Run health report and capture JSON artifact.
3. Run disk cleanup in dry-run mode, review candidate list.
4. Drain runners, then apply cleanup.
5. Re-run health report and confirm queue/availability recovery.

## 3.1) Build Smoke Exit `143` Triage

When `CI Run / Build (Smoke)` fails with `Process completed with exit code 143`:

1. Treat it as external termination (SIGTERM), not a compile error.
2. Confirm the build step ended with `Terminated` and no Rust compiler diagnostic was emitted.
3. Check current pool pressure (`runner_health_report.py`) before retrying.
4. Re-run once after pressure drops; persistent `143` should be handled as runner-capacity remediation.

Important:

- `error: cannot install while Rust is installed` from rustup bootstrap can appear in setup logs on pre-provisioned runners.
- That message is not itself a terminal failure when subsequent `rustup toolchain install` and `rustup default` succeed.

## 4) Queue Hygiene (Dry-Run First)

Dry-run example:

```bash
python3 scripts/ci/queue_hygiene.py \
  --repo zeroclaw-labs/zeroclaw \
  --obsolete-workflow "CI Build (Fast)" \
  --dedupe-workflow "CI Run" \
  --output-json artifacts/queue-hygiene.json
```

Apply mode:

```bash
python3 scripts/ci/queue_hygiene.py \
  --repo zeroclaw-labs/zeroclaw \
  --obsolete-workflow "CI Build (Fast)" \
  --dedupe-workflow "CI Run" \
  --max-cancel 200 \
  --apply \
  --output-json artifacts/queue-hygiene-applied.json
```

Safety behavior:

- At least one policy is required (`--obsolete-workflow` or `--dedupe-workflow`).
- `--apply` is opt-in; default is non-destructive preview.
- Deduplication is PR-only by default; use `--dedupe-include-non-pr` only when explicitly handling push/manual backlog.
- Cancellations are bounded by `--max-cancel`.

## Notes

- These scripts are operational tools and do not change merge-gating policy.
- Keep threshold values aligned with observed runner pool size and traffic profile.
