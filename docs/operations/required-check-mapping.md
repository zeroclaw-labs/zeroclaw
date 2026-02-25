# Required Check Mapping

This document maps merge-critical workflows to expected check names.

## Merge to `dev` / `main`

| Required check name | Source workflow | Scope |
| --- | --- | --- |
| `CI Required Gate` | `.github/workflows/ci-run.yml` | core Rust/doc merge gate |
| `Security Audit` | `.github/workflows/sec-audit.yml` | dependencies, secrets, governance |
| `Feature Matrix Summary` | `.github/workflows/feature-matrix.yml` | feature-combination compile matrix |
| `Workflow Sanity` | `.github/workflows/workflow-sanity.yml` | workflow syntax and lint |

Feature matrix lane check names (informational, non-required):

- `Matrix Lane (default)`
- `Matrix Lane (whatsapp-web)`
- `Matrix Lane (browser-native)`
- `Matrix Lane (nightly-all-features)`

## Promotion to `main`

| Required check name | Source workflow | Scope |
| --- | --- | --- |
| `Main Promotion Gate` | `.github/workflows/main-promotion-gate.yml` | branch + actor policy |
| `CI Required Gate` | `.github/workflows/ci-run.yml` | baseline quality gate |
| `Security Audit` | `.github/workflows/sec-audit.yml` | security baseline |

## Release / Pre-release

| Required check name | Source workflow | Scope |
| --- | --- | --- |
| `Verify Artifact Set` | `.github/workflows/pub-release.yml` | release completeness |
| `Pre-release Guard` | `.github/workflows/pub-prerelease.yml` | stage progression + tag integrity |
| `Nightly Summary & Routing` | `.github/workflows/feature-matrix.yml` (`profile=nightly`) | overnight integration signal |

## Verification Procedure

1. Resolve latest workflow run IDs:
   - `gh run list --repo zeroclaw-labs/zeroclaw --workflow feature-matrix.yml --limit 1`
   - `gh run list --repo zeroclaw-labs/zeroclaw --workflow ci-run.yml --limit 1`
2. Enumerate check/job names and compare to this mapping:
   - `gh run view <run_id> --repo zeroclaw-labs/zeroclaw --json jobs --jq '.jobs[].name'`
3. If any merge-critical check name changed, update this file before changing branch protection policy.

## Notes

- Use pinned `uses:` references for all workflow actions.
- Keep check names stable; renaming check jobs can break branch protection rules.
- GitHub scheduled/manual discovery for workflows is default-branch driven. If a release/nightly workflow only exists on `dev`, promotion to `main` is required before default-branch schedule visibility is expected.
- Update this mapping whenever merge-critical workflows/jobs are added or renamed.
