# syntax=docker/dockerfile:1.7-labs
#
# zerorelay - the ZeroClaw nominated relay (blind forwarder).
#
# Build from the REPOSITORY ROOT (zerorelay is a workspace member):
#   podman build -f apps/zerorelay/Dockerfile -t zerorelay:latest .
# or via compose:
#   cd apps/zerorelay && podman compose up --build
#
# The relay holds no inner-session keys and never decrypts the inner mTLS. It DOES
# terminate an outer TLS + WebSocket session, so it needs its own server cert - but
# it SELF-PROVISIONS one (CA + server cert) into --tls-dir on first run when no
# --tls-cert is given (no openssl). Mount --tls-dir as a volume so the cert (and
# the CA daemons/clients trust) persists across restarts; pass --tls-san <public
# hostname/IP> so the cert covers the relay's reachable address. To bring your own
# (e.g. a public-CA cert) pass --tls-cert/--tls-key instead. Admission is
# --registration-mode open|allowlist with --allow/--deny keyed on the daemon pubkey
# FINGERPRINT (sha256 hex), plus an optional shared-secret gate via --relay-token.

# --- Stage 1: build only the relay binary ---
FROM rust:1.96.1-slim AS builder
WORKDIR /app
COPY . .
RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zerorelay-target,target=/app/target,sharing=locked \
    cargo build --release -p zerorelay \
    && cp target/release/zerorelay /usr/local/bin/zerorelay

# --- Stage 2: minimal distroless runtime (no shell) ---
FROM gcr.io/distroless/cc-debian13:nonroot AS runtime
COPY --from=builder /usr/local/bin/zerorelay /usr/local/bin/zerorelay
# Ship a default config (bind 0.0.0.0:8443, self-provision TLS into /data/tls,
# open admission). Mount your own at /etc/zerorelay/relay.toml to override, then
# `kill -HUP` the process to hot-reload the [admission] section.
COPY apps/zerorelay/relay.example.toml /etc/zerorelay/relay.toml
EXPOSE 8443
USER nonroot
ENTRYPOINT ["/usr/local/bin/zerorelay"]
# Drive the relay from the config file. Self-provisions the outer TLS cert into
# the mounted /data/tls volume (see compose). Add a `sans` entry (or --tls-san
# <public-host>) for a publicly reachable relay.
CMD ["--config", "/etc/zerorelay/relay.toml"]
