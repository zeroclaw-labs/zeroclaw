# CoffeeAnon/zeroclaw Fork

## Upstream
- Repository: https://github.com/zeroclaw-labs/zeroclaw
- Remote name: `upstream`
- Fork base commit: `3141e9a` (between v0.1.7 and v0.1.8)
- Fork base tag: `fork-base-v0.1.8-pre`

## Upstream Sync Policy
Upstream syncs are **manual and deliberate**. Do not auto-merge or rebase
from upstream without reviewing the changelog and testing locally.

To check how far behind upstream:
```bash
git fetch upstream
git log --oneline HEAD..upstream/main | wc -l
```

To review upstream changes before syncing:
```bash
git log --oneline HEAD..upstream/main
git diff HEAD...upstream/main -- src/config/schema.rs  # check for breaking config changes
```

## Local Changes (relative to upstream)
- `src/config/schema.rs`: Added `ZEROCLAW_GATEWAY_PAIRED_TOKENS` env var override
- `Dockerfile.sam`: Narrowed chown scope to `/opt/sam-tools/home/.serena`
- `src/channels/`, `src/heartbeat/`: Proactive messaging feature

## Image Tags
- `citizendaniel/zeroclaw-sam:v1.2.0` — built from `sam-v1.2.0` tag
