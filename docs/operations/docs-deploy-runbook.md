# Docs Deploy Runbook

Workflow: `.github/workflows/docs-deploy.yml`

## Policy Contract

- Policy file: `.github/release/docs-deploy-policy.json`
- Guard script: `scripts/ci/docs_deploy_guard.py`
- Guard artifacts:
  - `docs-deploy-guard.json`
  - `docs-deploy-guard.md`
  - `audit-event-docs-deploy-guard.json`

## Lanes

- `Docs Quality Gate`: markdown quality + added-link checks
- `Docs Preview Artifact`: PR/manual preview package
- `Deploy Docs to GitHub Pages`: production deployment lane

## Triggering

- PR/push when docs or README markdown changes
- manual dispatch for preview or production
- manual production supports optional rollback via `rollback_ref`

## Quality Controls

- `scripts/ci/docs_quality_gate.sh`
- `scripts/ci/collect_changed_links.py` + lychee added-link checks

## Deployment Rules

- preview: upload `docs-preview` artifact only
- production: deploy to GitHub Pages on `main` push or manual production dispatch from `main`
- manual production promotion requires `preview_evidence_run_url` when policy requires it
- `rollback_ref` (manual production only) must resolve to a commit that is an ancestor of production branch (`main`) when policy requires ancestor validation

## Failure Handling

1. Re-run markdown and link gates locally.
2. Fix broken links / markdown regressions first.
3. Re-dispatch production deploy only after preview artifact checks pass.
4. Inspect `docs-deploy-guard.json` / `audit-event-docs-deploy-guard.json` for promotion/rollback contract violations.

## Rollback Validation (Manual Drill)

Use `workflow_dispatch` with:

- `deploy_target=production`
- `preview_evidence_run_url=<link to successful preview run>`
- `rollback_ref=<known-good commit/tag>`

Validation expectations:

1. Guard mode resolves to `rollback`.
2. Guard `ready=true` and no violations.
3. Deploy summary shows source ref equal to rollback commit SHA.
4. Pages deployment completes successfully.
