# Docs Deploy Policy

This document defines the promotion and rollback validation contract for docs deployment.

## Policy Source

- Machine policy: `.github/release/docs-deploy-policy.json`
- Enforcement script: `scripts/ci/docs_deploy_guard.py`
- Workflow integration: `.github/workflows/docs-deploy.yml` (`docs-quality` job)

## Promotion Contract

For production deploys:

1. Source branch must be production branch (`main`).
2. Manual production dispatch must include preview promotion evidence (`preview_evidence_run_url`) when policy requires it.
3. Guard output must be `ready=true` before `Deploy Docs to GitHub Pages` lane can run.

## Rollback Contract

For manual production rollback:

1. Set `deploy_target=production`.
2. Provide `rollback_ref` (tag/sha/ref).
3. Guard resolves rollback ref to commit SHA.
4. If policy enables ancestor validation, rollback ref must be an ancestor of production branch history.

## Artifacts and Retention

Guard emits:

- `docs-deploy-guard.json`
- `docs-deploy-guard.md`
- `audit-event-docs-deploy-guard.json`

Retention defaults:

- Docs preview artifacts: `14` days
- Docs deploy guard artifacts: `21` days

Retention values are configured in `.github/release/docs-deploy-policy.json`.
