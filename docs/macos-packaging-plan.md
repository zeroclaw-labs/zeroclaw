# macOS Intel Packaging Plan

## Objective

Ship a one-download macOS Intel (`x86_64`) release that starts ClawPilot + Mission Control without requiring Homebrew, Rust, Node, npm, or Convex tooling on end-user machines.

## Architecture audit summary

- Runtime is Rust binary (`zeroclaw`) and already supports daemon mode with explicit queue/results paths.
- Mission Control is a Next.js app and was previously developer-centric with Convex client hooks.
- Runtime bridging already exists through local API routes (`/api/runtime/runs*`) and file queues/results.

## Packaging strategy (chosen)

Use a **single portable app folder** packaged as:

1. `.zip` (required)
2. `.dmg` (practical; included)

Bundle contents:

- prebuilt `zeroclaw` (`x86_64-apple-darwin`)
- Next.js production standalone server output
- bundled Node runtime binary (`node`) so no system Node install is needed
- launcher scripts:
  - `start-clawpilot.command`
  - `stop-clawpilot.command`

Mission Control persistence is switched to a local JSON store through Next.js API routes (`/api/mission/*`) so no Convex dev server is required for end users.

## Release build flow

1. Build Rust binary for `x86_64-apple-darwin`.
2. Build Mission Control with `next build` (`output: "standalone"`).
3. Copy standalone server + static assets + public assets into app folder.
4. Copy Node binary into app folder.
5. Generate ZIP and DMG from the packaged app folder.

## Runtime defaults (end-user)

Launcher writes to:

- `~/.clawpilot/queue`
- `~/.clawpilot/results`
- `~/.clawpilot/mission-control`
- `~/.clawpilot/logs`

This preserves workspace-first/local-first behavior while avoiding global package managers.

## Security and path protections

- Queue/results remain explicit local paths.
- Mission Control production store is local filesystem only.
- No broadening of runtime permissions was introduced in packaging.

## Limitations

- DMG and ZIP are unsigned/not notarized in this path.
- First launch may trigger macOS Gatekeeper prompts.
- Launcher currently starts local processes in background and logs to `~/.clawpilot/logs`.
