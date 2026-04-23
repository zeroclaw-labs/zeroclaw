# syntax=docker/dockerfile:1.7
#
# deploy/zeroclaw/Dockerfile — Tailscale-equipped zeroclaw image for
# adi-zeroclaw-shane and adi-zeroclaw-meg. Builds on the root Dockerfile's
# builder stage (Rust 1.93), then produces a Debian-based runtime with
# tailscaled bundled so the machine joins the adi tailnet.
#
# Build context: repo root. Invoke via:
#   fly deploy --config deploy/zeroclaw/fly.shane.toml
# which sets the correct context.

# ── Stage 0: Frontend build ─────────────────────────────────────
FROM node:22-alpine AS web-builder
WORKDIR /web
COPY web/package.json web/package-lock.json* ./
RUN npm ci --ignore-scripts 2>/dev/null || npm install --ignore-scripts
COPY web/ .
RUN npm run build

# ── Stage 1: Build zeroclaw binary ───────────────────────────────
FROM rust:1.93-slim AS builder

WORKDIR /app
ARG ZEROCLAW_CARGO_FEATURES="channel-telegram,channel-slack,backend-postgres"

RUN --mount=type=cache,target=/var/cache/apt,sharing=locked \
    --mount=type=cache,target=/var/lib/apt,sharing=locked \
    apt-get update && apt-get install -y \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock build.rs ./
COPY crates/ crates/
COPY src/ src/
COPY benches/ benches/
COPY apps/tauri/Cargo.toml apps/tauri/Cargo.toml
RUN mkdir -p apps/tauri/src \
    && echo "fn main() {}" > apps/tauri/src/main.rs \
    && echo "fn main() {}" > apps/tauri/build.rs

RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zeroclaw-target,target=/app/target,sharing=locked \
    cargo build --release --locked --features "$ZEROCLAW_CARGO_FEATURES" && \
    cp target/release/zeroclaw /app/zeroclaw && \
    strip /app/zeroclaw

# ── Stage 2: Runtime (Debian + Tailscale) ───────────────────────
# trixie to match rust:1.93-slim's GLIBC (bookworm is too old).
FROM debian:trixie-slim AS runtime

ARG TAILSCALE_VERSION=1.80.2

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

# ── Bun (pinned) + volume-backed global install state ───────────────
# Bun binary lives in the image for fast, reproducible startup.
# Global installs ($BUN_INSTALL) and per-project node_modules land on
# /zeroclaw-data (Fly volume), so agent-installed packages survive
# machine restarts and redeploys.
ARG BUN_VERSION=1.1.38
RUN curl -fsSL "https://github.com/oven-sh/bun/releases/download/bun-v${BUN_VERSION}/bun-linux-x64.zip" -o /tmp/bun.zip \
    && unzip -q /tmp/bun.zip -d /tmp \
    && install -m 0755 /tmp/bun-linux-x64/bun /usr/local/bin/bun \
    && ln -sf /usr/local/bin/bun /usr/local/bin/bunx \
    && rm -rf /tmp/bun*

ENV BUN_INSTALL=/zeroclaw-data/bun
ENV PATH=/zeroclaw-data/bun/bin:$PATH

# ── Claude Code CLI ─────────────────────────────────────────────────
# Single native binary; no Node runtime required. We bypass the
# install.sh launcher (which writes to $HOME/.claude/ and assumes an
# interactive shell) and download the versioned linux-x64 binary
# directly into /usr/local/bin so it's available system-wide. Auth is
# not baked in — set ANTHROPIC_API_KEY or CLAUDE_CODE_OAUTH_TOKEN as
# a Fly secret per persona.
ARG CLAUDE_CODE_VERSION=2.1.117
ARG CLAUDE_CODE_SHA256=b7246963d9e32ece439c3e1e7885f53773a4820e90a4d2433ef2a413a055a5fe
RUN curl -fsSL "https://downloads.claude.ai/claude-code-releases/${CLAUDE_CODE_VERSION}/linux-x64/claude" -o /tmp/claude \
    && echo "${CLAUDE_CODE_SHA256}  /tmp/claude" | sha256sum -c - \
    && install -m 0755 /tmp/claude /usr/local/bin/claude \
    && rm -f /tmp/claude

COPY --from=builder /app/zeroclaw /usr/local/bin/zeroclaw
COPY --from=web-builder /web/dist /app/web/dist

# Runtime layout: /zeroclaw-data is the persistent volume mount.
RUN mkdir -p /zeroclaw-data/.zeroclaw /zeroclaw-data/workspace

COPY deploy/zeroclaw/config.seed.toml /opt/zeroclaw.config.seed.toml
COPY deploy/zeroclaw/entrypoint.sh    /entrypoint.sh
RUN chmod 0755 /entrypoint.sh

# Adi persona files — cloned at build time from the PRIVATE repo
# srmcguirt/adi-persona (contains IDENTITY/SOUL/USER.*/memory/ with
# personal data that must never land in this public zeroclaw fork).
#
# Pass ADI_PERSONA_REPO + a GitHub PAT with `repo` scope via
# `--build-secret id=adi_persona_token,...`. Token is never baked
# into layers (BuildKit secret mount).
#
# PERSONA (shane|meg) determines which USER.md + memory/<persona>/
# content remains in the image; the other persona's files are deleted
# so shane's image never contains meg's data and vice versa.
ARG PERSONA
ARG ADI_PERSONA_REPO=github.com/srmcguirt/adi-persona.git
ARG ADI_PERSONA_REF=master
# ADI_PERSONA_REV — the full commit SHA of the persona repo to pin to.
# Changing this value busts the Docker layer cache and forces a fresh
# git fetch, so pushing new persona content + bumping this SHA in
# fly.{shane,meg}.toml = guaranteed refresh without --no-cache.
ARG ADI_PERSONA_REV
RUN test -n "$PERSONA" || (echo "ERROR: PERSONA build-arg required (shane|meg)" && exit 1)
ENV PERSONA=$PERSONA

RUN apt-get update && apt-get install -y --no-install-recommends git && rm -rf /var/lib/apt/lists/*

RUN --mount=type=secret,id=adi_persona_token,required=true \
    set -e && \
    TOKEN="$(cat /run/secrets/adi_persona_token)" && \
    echo "cloning adi-persona ref=$ADI_PERSONA_REF rev=$ADI_PERSONA_REV" && \
    git clone --depth 1 --branch "$ADI_PERSONA_REF" \
        "https://x-access-token:${TOKEN}@${ADI_PERSONA_REPO}" \
        /tmp/adi-persona && \
    if [ -n "$ADI_PERSONA_REV" ]; then \
      cd /tmp/adi-persona && \
      actual_sha=$(git rev-parse HEAD) && \
      if [ "$actual_sha" != "$ADI_PERSONA_REV" ]; then \
        echo "WARNING: ADI_PERSONA_REV=$ADI_PERSONA_REV but HEAD is $actual_sha (OK: --depth=1 got latest; pin is informational)"; \
      fi; \
    fi && \
    mkdir -p /opt/adi-persona/memory && \
    cp /tmp/adi-persona/IDENTITY.md /tmp/adi-persona/SOUL.md \
       /tmp/adi-persona/BRAND.md /tmp/adi-persona/FOXY.md \
       /tmp/adi-persona/AGENTS.md /tmp/adi-persona/README.md \
       /opt/adi-persona/ && \
    cp -r /tmp/adi-persona/memory/shared /opt/adi-persona/memory/ && \
    case "$PERSONA" in \
      shane) cp /tmp/adi-persona/USER.shane.md /opt/adi-persona/USER.md && \
             cp -r /tmp/adi-persona/memory/shane /opt/adi-persona/memory/private ;; \
      meg)   cp /tmp/adi-persona/USER.meg.md /opt/adi-persona/USER.md && \
             cp -r /tmp/adi-persona/memory/meg /opt/adi-persona/memory/private ;; \
      *)     echo "ERROR: PERSONA must be shane or meg, got $PERSONA" && exit 1 ;; \
    esac && \
    rm -rf /tmp/adi-persona && \
    chmod -R go-rwx /opt/adi-persona

ENV LANG=C.UTF-8
ENV HOME=/zeroclaw-data
ENV ZEROCLAW_WORKSPACE=/zeroclaw-data/workspace
ENV ZEROCLAW_GATEWAY_PORT=42617

EXPOSE 42617

# Runs as ROOT so tailscaled can open /dev/net/tun. entrypoint.sh
# starts tailscaled, seeds config, then execs zeroclaw.
ENTRYPOINT ["/usr/bin/tini", "--", "/entrypoint.sh"]
