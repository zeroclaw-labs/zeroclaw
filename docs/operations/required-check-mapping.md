# Required Check Mapping

This document maps merge-critical workflows to expected check names.

## Merge to `dev` / `main`

| Required check name | Source workflow | Scope |
| --- | --- | --- |
| `CI Required Gate` | `.github/workflows/ci-run.yml` | core Rust/doc merge gate |
| `Security Audit` | `.github/workflows/sec-audit.yml` | dependencies, secrets, governance |
| `Feature Matrix Summary` | `.github/workflows/feature-matrix.yml` | feature-combination compile matrix |
| `Workflow Sanity` | `.github/workflows/workflow-sanity.yml` | workflow syntax and lint |

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
| `Nightly Summary & Routing` | `.github/workflows/nightly-all-features.yml` | overnight integration signal |

## Notes

- Use pinned `uses:` references for all workflow actions.
- Keep check names stable; renaming check jobs can break branch protection rules.
- Update this mapping whenever merge-critical workflows/jobs are added or renamed.
