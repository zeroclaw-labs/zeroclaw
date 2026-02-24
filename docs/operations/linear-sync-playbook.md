# Linear Sync Playbook (Repo Maintainers)

This playbook defines the operational contract for synchronizing GitHub execution with Linear for `zeroclaw-labs/zeroclaw`.

## Scope

- Team: `RMN` (`Repo Maintainers`)
- Objects: GitHub issues, pull requests, and multi-stage orchestration runs
- Goal: keep planning status, execution status, and merge readiness visible in one place

## Why This Matters

Without explicit synchronization, GitHub and Linear drift in different directions:

- GitHub may show active PR updates while Linear still looks idle.
- Linear may show "in progress" after work has already merged.
- Blocked work can disappear from maintainer dashboards because blocker context lives only in PR comments.

This playbook makes stage transitions auditable and time-bounded.

## Required Mapping

Map every active GitHub track to exactly one Linear issue.

- GitHub issue only:
  - Create Linear issue in `RMN`.
  - Add GitHub issue URL to Linear description.
  - Add Linear key (for example `RMN-123`) in GitHub issue body or top comment.
- GitHub PR:
  - Reuse the same Linear issue when the PR implements that scope.
  - Add PR URL to Linear description/comment.
  - Add Linear key to PR body.

## State Mapping

Use this default mapping unless maintainers explicitly override it:

- `triage` / `backlog`:
  - issue acknowledged, not started
- `started`:
  - implementation in progress, or draft PR open
- `started` + blocker comment:
  - blocked execution; include owner + ETA
- `completed`:
  - merged and post-merge CI green

## Update SLAs

- Update Linear within 15 minutes of each gate transition.
- Do not mark work complete in conversation if Linear still shows non-terminal state.

Each update must include:

- current stage (`intake`, `impl`, `validate`, `review`, `merged`)
- latest reference (commit SHA and/or PR number)
- CI summary (green, failing check names, or pending)
- next owner/action

## Command Templates (`linear` CLI)

Create issue in `RMN`:

```bash
cat > /tmp/linear-desc.md <<'EOF'
## Source
- GitHub issue: <url>

## Scope
- <summary>
EOF

linear issue create \
  --team RMN \
  --title "<title>" \
  --description-file /tmp/linear-desc.md
```

Move to started:

```bash
linear issue update RMN-123 --state started
```

Add transition comment:

```bash
cat > /tmp/linear-comment.md <<'EOF'
Stage: validate
PR: https://github.com/zeroclaw-labs/zeroclaw/pull/1234
CI: Lint Gate=pass, Test=running
Next: rerun integration tests
EOF

linear issue comment add RMN-123 --body-file /tmp/linear-comment.md
```

Mark complete after merge and green post-merge CI:

```bash
linear issue update RMN-123 --state completed
```

## Failure Handling

If Linear CLI/API is temporarily unavailable:

1. Record fallback note in run artifacts (`.codex/runs/<run_id>/`).
2. Continue GitHub execution only if it is safe to proceed.
3. Retry sync before final handoff/closure.
4. Include unsynced window and recovery timestamp in final report.
