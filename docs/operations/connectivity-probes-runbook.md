# Connectivity Probes Runbook

This runbook defines how maintainers operate the provider/model connectivity matrix probes.

Last verified: **February 24, 2026**.

## Scope

Covers the scheduled/manual workflow:

- `.github/workflows/ci-connectivity-probes.yml`
- `scripts/ci/provider_connectivity_matrix.py`
- `.github/connectivity/probe-contract.json`

Probe purpose:

- verify provider catalog discovery (`doctor models --provider ...`)
- classify failures into actionable buckets
- keep CI noise low with transient-failure policy
- publish machine-readable artifacts for triage

## Contract Model

Contract file: `.github/connectivity/probe-contract.json`

Each provider entry defines:

- `name`: display label in report
- `provider`: provider ID passed to `zeroclaw doctor models --provider`
- `required`: whether this provider can gate the run
- `secret_env`: required credential env var name for live probe
- `timeout_sec`: per-attempt timeout
- `retries`: retry count for transient failures
- `notes`: operator-facing context

Global field:

- `consecutive_transient_failures_to_escalate`: threshold for promoting `network` / `rate_limit` from warning to gate failure on required providers

## Failure Taxonomy

Categories in `connectivity-report.json`:

- `auth`: missing/invalid credential, permission denied, quota/access issues
- `network`: timeout, DNS/connectivity/TLS transport failures
- `unavailable`: unsupported endpoint, 404, empty model list, service unavailable
- `rate_limit`: HTTP 429 / explicit rate-limit errors
- `other`: uncategorized provider failures

## Gate Policy

Default policy implemented by `provider_connectivity_matrix.py`:

- Required provider + `auth` / `unavailable` / `other` => immediate gate failure
- Required provider + `network` / `rate_limit` => gate only after reaching transient threshold
- Optional provider failures => never gate
- Missing secret on required provider => immediate gate failure

For ad-hoc diagnostics, use workflow input `enforcement_mode=report-only`.

## CI Artifacts

Each run uploads:

- `connectivity-report.json` (full machine-readable matrix)
- `connectivity-summary.md` (human summary table)
- `.ci/connectivity-state.json` (transient tracking state)
- `.ci/connectivity-raw.log` (per-probe raw line log)

The markdown summary is also appended to `GITHUB_STEP_SUMMARY`.

## Local Reproduction

Build binary first:

```bash
cargo build --profile release-fast --locked --bin zeroclaw
```

Run probes in enforce mode:

```bash
python3 scripts/ci/provider_connectivity_matrix.py \
  --binary target/release-fast/zeroclaw \
  --contract .github/connectivity/probe-contract.json \
  --state-file .ci/connectivity-state.json \
  --output-json connectivity-report.json \
  --output-markdown connectivity-summary.md
```

Run report-only mode:

```bash
python3 scripts/ci/provider_connectivity_matrix.py \
  --binary target/release-fast/zeroclaw \
  --contract .github/connectivity/probe-contract.json \
  --report-only
```

## Triage Playbook

1. Open `connectivity-summary.md` for quick provider matrix.
2. For gate failures, inspect `category` and `message` in `connectivity-report.json`.
3. Follow category-specific actions:

- `auth`:
  - verify secret exists and is non-empty
  - rotate secret if revoked/expired
  - confirm plan/permission supports `/models`
- `network`:
  - retry once manually (`workflow_dispatch`)
  - check provider status page / GitHub Actions network incidents
  - escalate only after threshold is exceeded
- `unavailable`:
  - validate endpoint path contract
  - confirm provider still supports live model discovery
- `rate_limit`:
  - re-run later or reduce probe frequency for that provider
  - ensure provider plan allows current request rate
- `other`:
  - inspect raw log and provider response body
  - adjust classifier hints if recurring and actionable

## Change Control

When editing `.github/connectivity/probe-contract.json`:

1. Explain why provider requirement or threshold changed.
2. Keep required set small and stable to avoid alert fatigue.
3. Run local probe once before merging.
4. Update this runbook if policy behavior changed.
