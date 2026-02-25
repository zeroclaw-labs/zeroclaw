# Docs Deploy Runbook

Workflow: `.github/workflows/docs-deploy.yml`

## Lanes

- `Docs Quality Gate`: markdown quality + added-link checks
- `Docs Preview Artifact`: PR/manual preview package
- `Deploy Docs to GitHub Pages`: production deployment lane

## Triggering

- PR/push when docs or README markdown changes
- manual dispatch for preview or production

## Quality Controls

- `scripts/ci/docs_quality_gate.sh`
- `scripts/ci/collect_changed_links.py` + lychee added-link checks

## Deployment Rules

- preview: upload `docs-preview` artifact only
- production: deploy to GitHub Pages on `main` push or manual production dispatch

## Failure Handling

1. Re-run markdown and link gates locally.
2. Fix broken links / markdown regressions first.
3. Re-dispatch production deploy only after preview artifact checks pass.
