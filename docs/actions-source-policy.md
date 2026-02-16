# Actions Source Policy (Phase 1)

This document defines the current GitHub Actions source-control policy for this repository.

Phase 1 objective: lock down action sources with minimal disruption, before full SHA pinning.

## Current Policy

- Repository Actions permissions: enabled
- Allowed actions mode: selected
- SHA pinning required: false (deferred to Phase 2)

Selected allowlist patterns:

- `actions/*` (covers `actions/cache`, `actions/checkout`, `actions/upload-artifact`, `actions/download-artifact`, and other first-party actions)
- `docker/*`
- `dtolnay/rust-toolchain@*`
- `Swatinem/rust-cache@*`
- `DavidAnson/markdownlint-cli2-action@*`
- `lycheeverse/lychee-action@*`
- `EmbarkStudios/cargo-deny-action@*`
- `rhysd/actionlint@*`
- `softprops/action-gh-release@*`
- `sigstore/cosign-installer@*`
- `useblacksmith/*` (Blacksmith self-hosted runner infrastructure)

## Change Control Export

Use these commands to export the current effective policy for audit/change control:

```bash
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions
gh api repos/zeroclaw-labs/zeroclaw/actions/permissions/selected-actions
```

Record each policy change with:

- change date/time (UTC)
- actor
- reason
- allowlist delta (added/removed patterns)
- rollback note

## Why This Phase

- Reduces supply-chain risk from unreviewed marketplace actions.
- Preserves current CI/CD functionality with low migration overhead.
- Prepares for Phase 2 full SHA pinning without blocking active development.

## Agentic Workflow Guardrails

Because this repository has high agent-authored change volume:

- Any PR that adds or changes `uses:` action sources must include an allowlist impact note.
- New third-party actions require explicit maintainer review before allowlisting.
- Expand allowlist only for verified missing actions; avoid broad wildcard exceptions.
- Keep rollback instructions in the PR description for Actions policy changes.

## Validation Checklist

After allowlist changes, validate:

1. `CI`
2. `Docker`
3. `Security Audit`
4. `Workflow Sanity`
5. `Release` (when safe to run)

Failure mode to watch for:

- `action is not allowed by policy`

If encountered, add only the specific trusted missing action, rerun, and document why.

Latest sweep notes:

- 2026-02-16: Hidden dependency discovered in `release.yml`: `sigstore/cosign-installer@...`
    - Added allowlist pattern: `sigstore/cosign-installer@*`
- 2026-02-16: Blacksmith migration blocked workflow execution
    - Added allowlist pattern: `useblacksmith/*` for self-hosted runner infrastructure
    - Actions: `useblacksmith/setup-docker-builder@v1`, `useblacksmith/build-push-action@v2`

## Rollback

Emergency unblock path:

1. Temporarily set Actions policy back to `all`.
2. Restore selected allowlist after identifying missing entries.
3. Record incident and final allowlist delta.
