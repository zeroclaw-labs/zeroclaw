# WSL Primary Repository

This repository is the WSL-first working copy.

Primary goals:

1. Prefer all active development in WSL.
2. Keep a separate legacy repo during transition.
3. Make migration repeatable and low-risk.

Current remotes:

1. `origin` -> upstream GitHub repository
2. `win-archive` -> legacy local repository clone

Use `scripts/wsl/sync-from-win-archive.sh` to pull non-WSL changes from the legacy repo while both remain active.

Use `scripts/wsl/bootstrap-libs.sh` to provision only required external libraries/assets into WSL.
