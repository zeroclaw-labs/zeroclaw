# Workflow Directory Layout

GitHub Actions only loads workflow entry files from:

- `.github/workflows/*.yml`
- `.github/workflows/*.yaml`

Subdirectories are not valid locations for workflow entry files.

Repository convention:

1. Keep runnable workflow entry files at `.github/workflows/` root.
2. Keep cross-tooling/local CI scripts under `dev/` or `scripts/ci/` when used outside Actions.

Workflow behavior documentation in this directory:

- `.github/workflows/master-branch-flow.md`

Notable maintenance workflows:

- `factory-clerk.yml` - preview-only scheduled factory records cleanup audit; manual dispatch can run comment-only or safe apply modes for exact duplicate/fixed/superseded cleanup.
- `factory-inspector.yml` - preview-only scheduled intake quality audit; manual dispatch can comment on deterministic PR intake failures.
- `factory-testbench.yml` - read-only scheduled factory snapshot/replay audit with safety invariants.
- `factory-foreman.yml` - preview-only scheduled factory orchestration; manual dispatch can coordinate guarded comment/apply modes.
