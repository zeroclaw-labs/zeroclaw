# Branch Protection Baseline

This document is the repository-side baseline for branch protection on `dev` and `main`.
It should be updated whenever branch/ruleset policy changes.

## Baseline Date

- Baseline updated: 2026-03-05 (UTC)

## Protected Branches

- `dev`
- `main`

## Required Checks

Required check names are versioned in [required-check-mapping.md](./required-check-mapping.md).
At minimum, protect both branches with:

- `CI Required Gate`
- `Security Audit`
- `Feature Matrix Summary`
- `Workflow Sanity`

## Required Branch Rules

- Require a pull request before merging.
- Require status checks before merging.
- Require at least one approving review.
- Require CODEOWNERS review for protected paths.
- Dismiss stale approvals on new commits.
- Restrict force-pushes.
- Restrict bypass access to org owners/admins only.

## Export Procedure

Export live policy snapshots whenever branch protection changes:

```bash
mkdir -p docs/operations/branch-protection
gh api repos/zeroclaw-labs/zeroclaw/branches/dev/protection \
  > docs/operations/branch-protection/dev-protection.json
gh api repos/zeroclaw-labs/zeroclaw/branches/main/protection \
  > docs/operations/branch-protection/main-protection.json
```

If your org uses repository rulesets, also export:

```bash
gh api repos/zeroclaw-labs/zeroclaw/rulesets \
  > docs/operations/branch-protection/rulesets.json
```

## Validation Checklist

After updating branch protection:

1. Confirm required check names exactly match [required-check-mapping.md](./required-check-mapping.md).
2. Confirm merge queue compatibility for required workflows (`merge_group` on merge-critical workflows).
3. Confirm direct pushes are blocked for non-admin users.
4. Commit updated JSON snapshots under `docs/operations/branch-protection/`.
