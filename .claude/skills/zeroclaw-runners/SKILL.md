---
name: zeroclaw-runners
description: Manage self-hosted GitHub Actions runners for zeroclaw-labs/zeroclaw. Use when setting up runners, scaling capacity, troubleshooting CI job routing, checking runner status, or managing labels. Triggers on "runner setup", "add runners", "runner status", "CI not picking up jobs", "scale runners".
version: 0.1
last_updated: 2026-03-01
---

# ZeroClaw Self-Hosted Runners

Manage self-hosted GitHub Actions runners for the zeroclaw repo.

## Quick Reference

| Item | Value |
|------|-------|
| Repo | `zeroclaw-labs/zeroclaw` |
| Runner Taskfile | `~/actions-runner/Taskfile.yml` |
| Runner tarball | `~/actions-runner/actions-runner-osx-x64-2.332.0.tar.gz` |
| Required labels (Linux CI) | `[self-hosted, aws-india]` |
| macOS-only labels | `[self-hosted, macOS, X64]` |

## Runner Fleet

| Group | Purpose | Labels |
|-------|---------|--------|
| aws-india-* | Linux CI (build, test, lint) | `self-hosted, aws-india` |
| hetzner-* | Linux CI (overflow) | `self-hosted, aws-india` |
| mMacBook-* | macOS builds (future) | `self-hosted, macOS, X64` |

## Workflows

### Check Status

```bash
# All runners
gh api "repos/zeroclaw-labs/zeroclaw/actions/runners?per_page=100" \
  --jq '.runners[] | "\(.name)\t\(.status)\t\(.busy)"'

# Mac runners only
gh api "repos/zeroclaw-labs/zeroclaw/actions/runners?per_page=100" \
  --jq '.runners[] | select(.name | startswith("mMacBook")) | "\(.name)\t\(.status)\t\(.busy)"'

# Job queue
echo "Queued: $(gh run list -R zeroclaw-labs/zeroclaw --status queued --json name --jq 'length')"
echo "In Progress: $(gh run list -R zeroclaw-labs/zeroclaw --status in_progress --json name --jq 'length')"
```

### Add Runners (macOS)

1. Get registration token from GitHub Settings > Actions > Runners > Add runner
2. Run via Taskfile:
```bash
cd ~/actions-runner
task run-multi TOKEN=<token> RUNNER_COUNT=2
```

### Scale Down

```bash
# Stop and uninstall service
cd ~/actions-runner-N && ./svc.sh stop && ./svc.sh uninstall

# Remove from GitHub (get ID first)
gh api "repos/zeroclaw-labs/zeroclaw/actions/runners?per_page=100" \
  --jq '.runners[] | select(.name=="mMacBook-N") | .id'
gh api -X DELETE repos/zeroclaw-labs/zeroclaw/actions/runners/<ID>

# Clean up local directory
rm -rf ~/actions-runner-N
```

### Add/Remove Labels

```bash
# Add label
gh api -X POST repos/zeroclaw-labs/zeroclaw/actions/runners/<ID>/labels \
  --input - <<< '{"labels":["aws-india"]}'

# Remove label
gh api -X DELETE repos/zeroclaw-labs/zeroclaw/actions/runners/<ID>/labels/aws-india

# IMPORTANT: Restart runner after label change
cd ~/actions-runner-N && ./svc.sh stop && ./svc.sh start
```

### Restart All Runners

```bash
cd ~/actions-runner && task start-all
```

## Gotchas

| Issue | Cause | Fix |
|-------|-------|-----|
| Jobs queued but runners idle | Label mismatch | Check workflow `runs-on` vs runner labels |
| Label change not working | Runner cache | Restart runner service after API label change |
| Container action fails on macOS | Platform limitation | Container actions only work on Linux |
| "Runner is busy" on delete | Active job | Wait for job to complete, then delete |

## Taskfile Commands

| Command | Description |
|---------|-------------|
| `task run-multi TOKEN=x` | Set up N new runners |
| `task status` | Check all runner services |
| `task start-all` | Start all runner services |
| `task stop-all` | Stop all runner services |
| `task uninstall-all` | Stop and uninstall all services |
| `task logs` | Tail logs from all runners |
| `task cpu-info` | Show CPU info for scaling decisions |
| `task new-token-url` | Open browser to get registration token |

## Monitoring

```bash
# System resources while runners active
top -l 1 -n 0 | grep -E "CPU usage|PhysMem|Load Avg"

# Check what jobs each runner is processing
for dir in ~/actions-runner ~/actions-runner-{2,3,4}; do
  [ -d "$dir" ] && tail -5 "$dir/_diag/Worker_*.log" 2>/dev/null
done
```

## Gaps

> v0.1 - Based on 1 session. Needs validation:

- [ ] Linux runner setup (AWS/Hetzner) - only macOS documented
- [ ] Runner auto-scaling policies
- [ ] Cost/resource thresholds for scaling decisions
- [ ] Monitoring/alerting for offline runners
- [ ] Runner version upgrade workflow

## Out of Scope

- PR workflow -> use `zeroclaw-pr` skill
- CI pipeline configuration -> see `zeroclaw-pr/references/ci-and-github.md`
- Workflow YAML editing -> modify `.github/workflows/` directly
