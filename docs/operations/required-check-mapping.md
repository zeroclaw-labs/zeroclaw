# Required Check Mapping

This document maps merge-critical workflows to expected check names.

## Merge to `dev` / `main`

| Required check name | Source workflow | Scope |
| --- | --- | --- |
| `CI Required Gate` | `.github/workflows/ci-run.yml` | core Rust/doc merge gate |
| `Security Required Gate` | `.github/workflows/sec-audit.yml` | aggregated security merge gate |

### CI Run consolidated job names (referenced by CI Required Gate)

- `Quality Gate (Fmt + Clippy + Workspace + Package Checks)` — replaces `Lint Gate`, `Workspace Check`, `Package Check`
- `Test + Build` — replaces `Test`, `Build (Smoke)`

### Security audit consolidated job names (referenced by Security Required Gate)

- `Rust Security (Audit + Deny + Regressions)` — replaces `Security Audit`, `License & Supply Chain`, `Security Regression Tests`
- `Secrets Governance (Gitleaks)` — unchanged
- `Compliance (SBOM + Unsafe Debt)` — replaces `SBOM Snapshot`, `Unsafe Debt Audit`

Supplemental monitors (non-blocking unless added to branch protection contexts):

- `CI Change Audit` (`.github/workflows/ci-change-audit.yml`) — push-to-main only (removed from PR path)
- `CodeQL Analysis` (`.github/workflows/sec-codeql.yml`) — push-to-main + weekly only (removed from PR path)
- `Workflow Sanity` (`.github/workflows/workflow-sanity.yml`)
- `Feature Matrix Summary` (`.github/workflows/feature-matrix.yml`)

Feature matrix lane check names (informational, non-required):

- `Matrix Lane (default)` — runs on all profiles
- `Matrix Lane (whatsapp-web)` — nightly/weekly only
- `Matrix Lane (browser-native)` — nightly/weekly only
- `Matrix Lane (nightly-all-features)` — nightly/weekly only

## Release / Pre-release

| Required check name | Source workflow | Scope |
| --- | --- | --- |
| `Verify Artifact Set` | `.github/workflows/pub-release.yml` | release completeness |
| `Pre-release Guard` | `.github/workflows/pub-prerelease.yml` | stage progression + tag integrity |
| `Nightly Summary & Routing` | `.github/workflows/feature-matrix.yml` (`profile=nightly`) | overnight integration signal |

## Verification Procedure

1. Check active branch protection required contexts:
   - `gh api repos/zeroclaw-labs/zeroclaw/branches/main/protection --jq '.required_status_checks.contexts[]'`
2. Resolve latest workflow run IDs:
   - `gh run list --repo zeroclaw-labs/zeroclaw --workflow feature-matrix.yml --limit 1`
   - `gh run list --repo zeroclaw-labs/zeroclaw --workflow ci-run.yml --limit 1`
3. Enumerate check/job names and compare to this mapping:
   - `gh run view <run_id> --repo zeroclaw-labs/zeroclaw --json jobs --jq '.jobs[].name'`
4. If any merge-critical check name changed, update this file before changing branch protection policy.

## Notes

- Use pinned `uses:` references for all workflow actions.
- Keep check names stable; renaming check jobs can break branch protection rules.
- GitHub scheduled/manual discovery for workflows is default-branch driven. If a release/nightly workflow only exists on a non-default branch, merge it into the default branch before expecting schedule visibility.
- Update this mapping whenever merge-critical workflows/jobs are added or renamed.
