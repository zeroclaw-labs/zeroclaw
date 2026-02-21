# Workflow Directory Layout

GitHub Actions only loads workflow entry files from:

- `.github/workflows/*.yml`
- `.github/workflows/*.yaml`

Subdirectories are not valid locations for workflow entry files.

Repository convention:

1. Keep runnable workflow entry files at `.github/workflows/` root.
2. Keep workflow-only helper scripts under `.github/workflows/scripts/`.
3. Keep cross-tooling/local CI scripts under `scripts/ci/` when they are used outside Actions.

Workflow behavior documentation in this directory:

- `.github/workflows/main-branch-flow.md`

Current workflow helper scripts:

- `.github/workflows/scripts/ci_workflow_owner_approval.js`
- `.github/workflows/scripts/ci_license_file_owner_guard.js`
- `.github/workflows/scripts/lint_feedback.js`
- `.github/workflows/scripts/pr_auto_response_contributor_tier.js`
- `.github/workflows/scripts/pr_auto_response_labeled_routes.js`
- `.github/workflows/scripts/pr_check_status_nudge.js`
- `.github/workflows/scripts/pr_intake_checks.js`
- `.github/workflows/scripts/pr_labeler.js`
- `.github/workflows/scripts/test_benchmarks_pr_comment.js`
