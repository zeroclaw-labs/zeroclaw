# WSL Global Resources

This guide sets up shared, machine-level resources that make ZeroClaw WSL operations consistent across shells and repositories.

## What It Configures

1. A global env file: `~/.config/zeroclaw-wsl/env.sh`
2. A global wrapper command: `~/.local/bin/zcwsl`
3. Optional auto-source block in `~/.bashrc`
4. Default WSL repo and archive paths for scripts

## Setup

```bash
cd /home/lasve046/projects/zeroclaw-wsl
chmod +x scripts/wsl/setup-global-resources.sh
scripts/wsl/setup-global-resources.sh
source ~/.config/zeroclaw-wsl/env.sh
```

Dry-run mode:

```bash
scripts/wsl/setup-global-resources.sh --dry-run
```

## Validate

```bash
chmod +x scripts/wsl/doctor-global-resources.sh
scripts/wsl/doctor-global-resources.sh
```

## Useful Commands After Setup

1. `zcroot` -> jump to WSL primary repo
2. `zcarchive` -> jump to legacy archive repo
3. `zcproceed` or `zcwsl` -> run WSL proceed checks

## Notes

1. `scripts/wsl/sync-from-win-archive.sh` has a safety guard and may exit non-zero when archive history is not fast-forward-safe.
2. That non-zero is expected in WSL-primary mode and prevents accidental downgrade sync.
