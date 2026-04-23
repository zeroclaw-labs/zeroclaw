# Bun runtime + shell-accessible tooling on zeroclaw instances

**Date:** 2026-04-22
**Status:** Draft
**Scope:** `deploy/zeroclaw/Dockerfile` only
**Targets:** `adi-zeroclaw-shane`, `adi-zeroclaw-meg`

## Goal

Enable the zeroclaw agent's shell tool to run `bun`, `bunx`, `claude`, and `gstack` on the Fly machines, and let the agent install additional JS/TS packages on demand. Bun binary is pinned in the image; package state lives on the persistent volume and survives restarts.

## Non-goals

- Pre-provisioning any npm/bun packages beyond what the image installers require.
- Claude Code auth policy — handled separately via Fly secrets at deploy time.
- Changes to crates, config schema, entrypoint, or fly.toml.

## Context

The two zeroclaw instances run a Debian trixie-slim image produced from `deploy/zeroclaw/Dockerfile`. Today the runtime stage installs only `ca-certificates curl iproute2 procps tini git` plus `tailscaled` and the zeroclaw binary itself. Persistent state lives under `/zeroclaw-data/` on a Fly volume (`HOME=/zeroclaw-data`).

The agent's shell tool can already execute arbitrary commands, but there is no JS runtime or developer CLI installed, so common agent operations ("run this script", "try this MCP server", "spin up a headless browser for QA") fail immediately.

## Design

### Components and where each lives

| Component | Location | Why |
|---|---|---|
| `bun` binary (pinned v1.1.38) | `/usr/local/bin/bun` (image) | Reproducible, fast startup, matches tailscale pinning pattern already in the Dockerfile. |
| `bunx` symlink | `/usr/local/bin/bunx` → `bun` | Bun's own convention. |
| `$BUN_INSTALL` (global installs, cache) | `/zeroclaw-data/bun/` (volume) | Agent-installed CLIs persist across restarts; shared download cache speeds repeat `bun install`. |
| `claude` CLI | `/usr/local/bin/claude` (image) | Ecosystem-stable single binary; avoids a cold-start install on every fresh volume. |
| `ripgrep` | apt, `/usr/bin/rg` (image) | Claude Code search path fallback. |
| Headless Chromium runtime libs | apt (image) | Required by Playwright, which bun + gstack use to drive a browser. OS-level — agent cannot install these. |
| `gstack` + Chromium browser binary | `/zeroclaw-data/.claude/skills/gstack/` (volume; agent-installed at runtime via `git clone` + `./setup`) | Matches the agent-driven-install goal; Chromium (~150MB) caches under `/zeroclaw-data/.cache/ms-playwright/` so one download covers future invocations. |

### Dockerfile changes (stage 2, runtime)

Additions are layered so each concern is obvious from the diff:

**Layer A — extend the existing apt install:**

```dockerfile
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl iproute2 procps tini \
        unzip ripgrep \
        libnss3 libnspr4 \
        libatk1.0-0 libatk-bridge2.0-0 \
        libcups2 libdrm2 libgbm1 \
        libxcomposite1 libxdamage1 libxrandr2 libxfixes3 libxkbcommon0 \
        libasound2 libpango-1.0-0 libcairo2 \
        fonts-liberation \
    && curl -fsSL "https://pkgs.tailscale.com/stable/tailscale_${TAILSCALE_VERSION}_amd64.tgz" -o /tmp/tailscale.tgz \
    && tar -xzf /tmp/tailscale.tgz -C /tmp \
    && install -m 0755 "/tmp/tailscale_${TAILSCALE_VERSION}_amd64/tailscaled" /usr/local/bin/tailscaled \
    && install -m 0755 "/tmp/tailscale_${TAILSCALE_VERSION}_amd64/tailscale"  /usr/local/bin/tailscale \
    && rm -rf /tmp/tailscale* /var/lib/apt/lists/*
```

**Layer B — install bun:**

```dockerfile
ARG BUN_VERSION=1.1.38
RUN curl -fsSL "https://github.com/oven-sh/bun/releases/download/bun-v${BUN_VERSION}/bun-linux-x64.zip" -o /tmp/bun.zip \
    && unzip -q /tmp/bun.zip -d /tmp \
    && install -m 0755 /tmp/bun-linux-x64/bun /usr/local/bin/bun \
    && ln -sf /usr/local/bin/bun /usr/local/bin/bunx \
    && rm -rf /tmp/bun*

ENV BUN_INSTALL=/zeroclaw-data/bun
ENV PATH=/zeroclaw-data/bun/bin:$PATH
```

**Layer C — install Claude Code CLI:**

```dockerfile
RUN curl -fsSL https://claude.ai/install.sh | bash -s -- --install-dir /usr/local
```

The installer places `claude` in `/usr/local/bin/claude`. Auth is deliberately unset here — set `ANTHROPIC_API_KEY` or `CLAUDE_CODE_OAUTH_TOKEN` as a Fly secret per persona at deploy time.

Ordering: all three layers sit in stage 2 between the existing tailscale install block and the persona clone block. Bun + claude do not depend on each other or on persona state, so their placement is driven by cache churn — they go *before* the persona layer so bumping a persona SHA does not re-download them.

### Runtime state layout

After first use, the persistent volume looks like:

```
/zeroclaw-data/
├── .zeroclaw/              # existing: config.toml
├── workspace/              # existing: MEMORY.md, IDENTITY.md, ...
├── tailscale/              # existing: tailscaled state
├── bun/                    # new: $BUN_INSTALL
│   ├── bin/                #   global bun-installed CLIs (on PATH)
│   ├── cache/              #   download cache
│   └── install/            #   global node_modules
├── .claude/                # new: claude config + skills
│   └── skills/
│       └── gstack/         #   agent-cloned on demand
└── .cache/
    └── ms-playwright/      # new: Chromium binary cached by bun + playwright
```

Nothing the agent writes here needs explicit mkdir in the Dockerfile — bun, claude, and gstack all create their directories lazily on first use. Permissions inherit from the existing `HOME=/zeroclaw-data` setup.

### What the agent does on first use

Expected first-use flow, driven by the agent through its shell tool — no deploy-time pre-provisioning:

```bash
# Install gstack (one-time per volume)
git clone --single-branch --depth 1 \
    https://github.com/garrytan/gstack.git \
    /zeroclaw-data/.claude/skills/gstack
cd /zeroclaw-data/.claude/skills/gstack && ./setup   # downloads Chromium

# Install any ad-hoc tool
bun add -g some-cli-tool                              # lands in $BUN_INSTALL/bin
```

Both survive machine restarts because everything material lives on the volume.

## Trade-offs and rationale

**Bun binary in image, packages on volume.** Splitting "the runtime" from "what is installed with the runtime" avoids a chicken-and-egg where a fresh machine has nothing to bootstrap from. The image provides the floor; the volume provides the garden.

**Pinned release vs. `curl | bash` for bun.** Pinning matches the existing `TAILSCALE_VERSION=1.80.2` pattern in the same Dockerfile. Version bumps become explicit diffs, not silent drift at the next image rebuild.

**gstack NOT baked into the image.** Baking it would contradict the agent-driven-install goal and couple gstack updates to deploy cadence. The only gstack-adjacent concern that *must* be in the image is the set of Playwright Chromium runtime libs (`libnss3`, etc.) — those are OS-level and the agent cannot apt-install them.

**Claude Code baked in.** Unlike ecosystem packages, `claude` is a single CLI the agent will almost certainly want on day one. Baking it saves a runtime install step and keeps auth wiring consistent across restarts.

**Auth deferred.** The Fly secret name (`ANTHROPIC_API_KEY` vs `CLAUDE_CODE_OAUTH_TOKEN`) is a per-persona policy question. Keeping it out of this spec means the image does not assume a billing model.

## Risks

**Image size.** The Chromium runtime libs add roughly 80–120MB to the image. Acceptable: the existing image is already debian:trixie-slim + rust binary + tailscale, measured in hundreds of MB. This lifts it one tier, not an order of magnitude.

**Writable PATH.** `PATH=/zeroclaw-data/bun/bin:$PATH` prepends a volume-writable directory to PATH. An attacker with shell access can `bun add -g evil-tool` and the next shell invocation picks it up. This is intentional — it *is* the agent-driven-install feature — but it is a real capability increment. Existing zeroclaw security controls (prompt guard, channel allowlist, pairing scope) already gate who reaches the shell tool; bun does not widen the entry surface, only what the agent can do once it is in.

**First-use Chromium download.** The first invocation of gstack after a fresh volume downloads ~150MB of Chromium. A transient network flake mid-download will produce a partial cache that bun/playwright must re-validate. Acceptable: this is one-time per volume and retryable.

**apt install failures.** Adding ~13 new apt packages slightly increases the chance of a transient apt mirror failure at build time. Mitigation: the existing Dockerfile already uses `apt-get update && install ... && rm -rf /var/lib/apt/lists/*` in a single layer, which is the standard practice; no additional hardening needed.

## Testing

- **Build:** `flyctl deploy --config deploy/zeroclaw/fly.shane.toml` (and `fly.meg.toml`) succeeds.
- **Smoke test per machine** (via `fly machine exec`):
  - `bun --version` → prints 1.1.38
  - `bunx --version` → prints 1.1.38
  - `which claude && claude --version` → both succeed
  - `which rg` → `/usr/bin/rg`
  - `echo $BUN_INSTALL` → `/zeroclaw-data/bun`
- **Persistence check:** `bun add -g cowsay && fly machine restart <id> && cowsay hi` still works after restart (global install survived on volume).
- **Gstack install** (not a gate — the agent does it on demand): agent-driven `git clone … && ./setup` completes without missing-shared-lib errors.

## Rollback

Revert the Dockerfile commit. Next deploy drops the new tools; volume state under `/zeroclaw-data/bun/`, `/zeroclaw-data/.claude/`, and `/zeroclaw-data/.cache/` becomes dead data but does not harm the zeroclaw binary. Volume cleanup, if desired, is `rm -rf` over ssh — not required for correctness.
