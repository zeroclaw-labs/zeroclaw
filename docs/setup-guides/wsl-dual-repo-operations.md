# WSL Dual-Repo Operations

This guide defines how to run a WSL-first workflow while temporarily keeping a legacy repo.

## Repo Roles

1. WSL primary repo: `/home/lasve046/projects/zeroclaw-wsl`
2. Legacy repo (to be demoted/archived): `/home/lasve046/pilot/zeroclaw`

## Why Not Copy Every Library

Do not reproduce every external library in WSL.

Only provision what active WSL workflows require:

1. Faster setup and lower disk usage.
2. Less drift and fewer stale dependencies.
3. Clear ownership of required assets.

## Bootstrap Required Libraries Into WSL

Default manifest: `scripts/wsl/lib-manifest.txt`

```bash
cd /home/lasve046/projects/zeroclaw-wsl
chmod +x scripts/wsl/bootstrap-libs.sh
scripts/wsl/bootstrap-libs.sh --dry-run
scripts/wsl/bootstrap-libs.sh --force
```

The manifest supports one entry per line:

```text
mode|source|target
```

Where `mode` is `symlink` or `copy`.

## Keep WSL Repo Current During Transition

```bash
cd /home/lasve046/projects/zeroclaw-wsl
chmod +x scripts/wsl/sync-from-win-archive.sh
scripts/wsl/sync-from-win-archive.sh --dry-run
# apply only if dry-run reports real source deltas
scripts/wsl/sync-from-win-archive.sh
```

Optional: include deletions from source:

```bash
scripts/wsl/sync-from-win-archive.sh --delete
```

## Demote And Archive Legacy Repo

After WSL package and workflows are stable:

1. Freeze legacy repo changes.
2. Final sync into WSL repo.
3. Rename legacy path with archive suffix, for example:
   `/home/lasve046/pilot/zeroclaw-win-archive`
4. Keep read-only for rollback reference.

## Recommended Team Policy

1. New work starts in the WSL primary repo only.
2. PRs are opened from WSL primary branches.
3. Legacy repo accepts no direct feature work once demoted.


### Sync Safety Guard

The sync script blocks archive->WSL sync when the archive commit is older than WSL.

Use override only for intentional rollback/replay:

```bash
scripts/wsl/sync-from-win-archive.sh --allow-older-source --dry-run
```

## Global Machine Resources

Set up shared shell and environment resources once per machine:

- [wsl-global-resources.md](wsl-global-resources.md)
