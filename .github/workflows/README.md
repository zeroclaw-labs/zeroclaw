# Workflow Directory Layout

GitHub Actions only loads workflow entry files from:

- `.github/workflows/*.yml`
- `.github/workflows/*.yaml`

Subdirectories are not valid locations for workflow entry files.

Repository convention:

1. Keep runnable workflow entry files at `.github/workflows/` root.
2. Keep cross-tooling/local CI scripts under `scripts/ci/` when they are used outside Actions.

Workflow behavior documentation in this directory:

- `.github/workflows/master-branch-flow.md`

Current workflows:

- `.github/workflows/ci.yml` — PR checks (test + build)
- `.github/workflows/ci-full.yml` — manual full cross-platform build matrix
- `.github/workflows/release.yml` — automatic beta release on push to `master`
- `.github/workflows/promote-release.yml` — manual stable release
