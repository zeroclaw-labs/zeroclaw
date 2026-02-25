# Pre-release Stage Gates

Workflow: `.github/workflows/pub-prerelease.yml`
Policy: `.github/release/prerelease-stage-gates.json`

## Stage Model

- `alpha`
- `beta`
- `rc`
- `stable`

## Guard Rules

- Tag format: `vX.Y.Z-(alpha|beta|rc).N`
- Stage transition must follow policy (`alpha -> beta -> rc -> stable`)
- No stage regression allowed for the same semantic version
- Tag commit must be reachable from `origin/main`
- `Cargo.toml` version at tag must match tag version

## Outputs

- `prerelease-guard.json`
- `prerelease-guard.md`
- `audit-event-prerelease-guard.json`

## Publish Contract

- `dry-run`: guard + build + artifact manifest only
- `publish`: create/update GitHub prerelease and attach built assets
