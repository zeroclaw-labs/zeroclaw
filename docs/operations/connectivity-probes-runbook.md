# Connectivity Probes Runbook

This runbook defines how maintainers operate provider endpoint connectivity probes in CI.

Last verified: **February 24, 2026**.

## Scope

Primary workflow:

- `.github/workflows/ci-provider-connectivity.yml`

Probe engine and config:

- `scripts/ci/provider_connectivity_matrix.py`
- `.github/connectivity/providers.json`

## Probe Model

Configuration file: `.github/connectivity/providers.json`

Each provider entry defines:

- `id`: provider identifier
- `url`: endpoint URL to probe
- `method`: HTTP method (`HEAD` or `GET`)
- `critical`: whether failure should gate in enforce mode

Global field:

- `global_timeout_seconds`: probe timeout for DNS + HTTP checks

## Trigger and Enforcement

`CI Provider Connectivity` behavior:

- Schedule: every 6 hours
- Manual dispatch: `fail_on_critical=true|false`
- PR/push: runs when probe config/script/workflow changes

Enforcement policy:

- critical endpoint unreachable + `fail_on_critical=true` -> workflow fails
- non-critical endpoint unreachable -> reported but non-blocking

## CI Artifacts

Per run artifacts include:

- `provider-connectivity-matrix.json`
- `provider-connectivity-matrix.md`
- normalized audit event JSON when emitted by workflow

Markdown summary is appended to `GITHUB_STEP_SUMMARY`.

## Local Reproduction

Enforced mode:

```bash
python3 scripts/ci/provider_connectivity_matrix.py \
  --config .github/connectivity/providers.json \
  --output-json provider-connectivity-matrix.json \
  --output-md provider-connectivity-matrix.md \
  --fail-on-critical
```

Report-only mode:

```bash
python3 scripts/ci/provider_connectivity_matrix.py \
  --config .github/connectivity/providers.json \
  --output-json provider-connectivity-matrix.json \
  --output-md provider-connectivity-matrix.md
```

## Triage Playbook

1. Read matrix markdown for quick status.
2. For failures, inspect row-level fields in JSON:
   - `dns_ok`
   - `http_status`
   - `reachable`
   - `notes`
3. Resolve by class:
   - DNS/transport errors: check network, provider status, retry manually
   - HTTP 401/403: rotate credentials or verify auth configuration
   - HTTP 404/5xx: verify endpoint contract and upstream service health
4. Re-run manually before escalating sustained incidents.

## Change Control

When editing `.github/connectivity/providers.json`:

1. Keep critical endpoint list minimal and stable.
2. Document why endpoint criticality changed.
3. Run local probe once before merging.
4. Update this runbook when contract fields or gating behavior changes.
