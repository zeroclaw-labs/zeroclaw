# WSL Rollout Notes

Branch: `wsl-primary-bootstrap`

## Scope Delivered

1. Established WSL primary working repository.
2. Demoted legacy Windows repo to archive path.
3. Added selective WSL libs bootstrap tooling.
4. Added safe archive sync tooling with guard rails.
5. Added global machine-level setup and doctor tooling.

## PR Summary Template

Use this summary in the PR body:

```text
## What changed
- Added WSL-first operational scripts under scripts/wsl/
- Added global setup script for shell/env integration
- Added doctor script for machine-level WSL readiness checks
- Added WSL docs for dual-repo and global resources
- Added sync safety guard to prevent unsafe archive replay

## Why
- Make WSL the preferred, reliable daily runtime
- Keep legacy Windows repo available as read-only archive
- Reduce onboarding errors and terminal exit-code confusion

## Validation
- scripts/wsl/proceed.sh exits 0
- scripts/wsl/doctor-global-resources.sh exits 0
- ~/.config/zeroclaw-wsl/env.sh generated and sourceable
- ~/.local/bin/zcwsl wrapper executable
```

## Rollout Plan

1. Merge `wsl-primary-bootstrap` to default branch.
2. Announce WSL-primary policy and archive policy.
3. Ask contributors to run:
   - `scripts/wsl/setup-global-resources.sh`
   - `scripts/wsl/doctor-global-resources.sh`
4. Keep legacy archive read-only except emergency rollback.
5. Remove any remaining Windows-native setup references in follow-up PRs if needed.

## Rollback Plan

1. If WSL setup breaks for contributors, keep branch merged but disable auto adoption by:
   - removing `~/.bashrc` source block,
   - reverting wrapper usage,
   - running local repo-only scripts directly.
2. For severe regressions, cherry-pick revert commits for:
   - `scripts/wsl/setup-global-resources.sh`
   - `scripts/wsl/doctor-global-resources.sh`
   - docs updates.

## Known Safe Guard Behavior

`sync-from-win-archive.sh` may intentionally return non-zero when histories diverge or archive is older.
This is expected and prevents accidental downgrade sync.
Use `--allow-older-source` only for intentional rollback/replay.
