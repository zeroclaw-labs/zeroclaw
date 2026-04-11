# Install ClawPilot on macOS Intel (2015 MacBook Pro compatible)

## What you download

Download one of these release artifacts:

- `ClawPilot-macos-intel-<version>.zip`
- `ClawPilot-macos-intel-<version>.dmg`

No Homebrew, Rust, Node, npm, or Convex setup is required.

## Install

### ZIP path

1. Download the ZIP.
2. Unzip it.
3. Move `ClawPilot-macos-intel` into `Applications` (or any folder you prefer).

### DMG path

1. Open the DMG.
2. Drag `ClawPilot-macos-intel` to your preferred location.

## First run

1. Open the app folder.
2. Double-click `start-clawpilot.command`.
3. Mission Control opens at: `http://127.0.0.1:4310`

If macOS blocks first run, right-click `start-clawpilot.command` → **Open**.

## Stop

Double-click `stop-clawpilot.command`.

## Data and logs

ClawPilot stores runtime data locally in your home directory:

- `~/.clawpilot/queue`
- `~/.clawpilot/results`
- `~/.clawpilot/mission-control`
- `~/.clawpilot/logs`

## Notes

- This package targets Intel Macs (`x86_64`) and does not require Apple Silicon.
- Package is local-first and runs without developer toolchains.
