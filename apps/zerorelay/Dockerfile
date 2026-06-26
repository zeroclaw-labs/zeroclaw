# syntax=docker/dockerfile:1.7-labs
#
# zerorelay - the ZeroClaw nominated relay (blind forwarder).
#
# Build from the REPOSITORY ROOT (zerorelay is a workspace member):
#   podman build -f apps/zerorelay/Dockerfile -t zerorelay:latest .
# or via compose:
#   cd apps/zerorelay && podman compose up --build
#
# The relay is stateless, holds no inner-session keys, and never decrypts the
# inner mTLS. It DOES terminate an outer TLS + WebSocket session, so it needs its
# own server certificate: mount one and point --tls-cert/--tls-key at it (see the
# default CMD below and compose.yaml). Admission is --registration-mode open|
# allowlist with --allow/--deny keyed on the daemon pubkey FINGERPRINT (sha256
# hex), plus an optional shared-secret gate via --relay-token.

# ── Stage 1: build only the relay binary ───────────────────────
FROM rust:1.94-slim AS builder
WORKDIR /app
COPY . .
RUN --mount=type=cache,id=zeroclaw-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=zeroclaw-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=zerorelay-target,target=/app/target,sharing=locked \
    cargo build --release -p zerorelay \
    && cp target/release/zerorelay /usr/local/bin/zerorelay

# ── Stage 2: minimal distroless runtime (no shell) ─────────────
FROM gcr.io/distroless/cc-debian13:nonroot AS runtime
COPY --from=builder /usr/local/bin/zerorelay /usr/local/bin/zerorelay
EXPOSE 8443
USER nonroot
ENTRYPOINT ["/usr/local/bin/zerorelay"]
# Outer TLS material is required; mount it at /etc/zerorelay/certs (see compose).
CMD ["--bind", "0.0.0.0:8443", "--tls-cert", "/etc/zerorelay/certs/relay.crt", "--tls-key", "/etc/zerorelay/certs/relay.key"]
