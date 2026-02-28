# GHCR Tag Policy

This document defines the production container tag contract for `.github/workflows/pub-docker-img.yml`.

## Policy Source

- Machine policy: `.github/release/ghcr-tag-policy.json`
- Enforcement script: `scripts/ci/ghcr_publish_contract_guard.py`
- Workflow integration: `.github/workflows/pub-docker-img.yml` (`publish` job)
- Related vulnerability gate policy: `.github/release/ghcr-vulnerability-policy.json` (`scripts/ci/ghcr_vulnerability_gate.py`)

## Tag Taxonomy

Release publishes are restricted to stable git tags matching `vX.Y.Z`.

For each publish run, the workflow must produce three GHCR tags:

1. `vX.Y.Z` (release tag, immutable)
2. `sha-<12>` (commit SHA tag, immutable)
3. `latest` (mutable pointer for the newest stable release)

## Immutability Contract

The guard enforces digest parity:

1. `digest(vX.Y.Z) == digest(sha-<12>)`
2. `digest(latest) == digest(vX.Y.Z)` (while `require_latest_on_release=true`)

If any required tag is missing, not pullable, or violates digest parity, publish contract validation fails.

## Rollback Mapping

Rollback candidates are emitted deterministically from policy class order (`rollback_priority`):

1. `sha-<12>`
2. `vX.Y.Z`

The guard outputs this mapping to `ghcr-publish-contract.json` for auditability.

## Artifacts and Retention

Publish run emits:

- `ghcr-publish-contract.json`
- `ghcr-publish-contract.md`
- `audit-event-ghcr-publish-contract.json`
- `ghcr-vulnerability-gate.json`
- `ghcr-vulnerability-gate.md`
- `audit-event-ghcr-vulnerability-gate.json`
- Trivy reports (`trivy-<tag>.sarif`, `trivy-<tag>.txt`, `trivy-<tag>.json`, `trivy-sha-<12>.txt`, `trivy-sha-<12>.json`, `trivy-latest.txt`, `trivy-latest.json`)

Retention defaults:

- Contract artifacts: `21` days
- Vulnerability gate artifacts: `21` days
- Trivy scan artifacts: `14` days

Retention values are defined in `.github/release/ghcr-tag-policy.json` and reflected in workflow artifact uploads.
