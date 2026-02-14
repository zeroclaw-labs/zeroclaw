# ── Stage 1: Build ────────────────────────────────────────────
FROM rust:1.83-slim AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release --locked && \
    strip target/release/zeroclaw

# ── Stage 2: Runtime (distroless nonroot — no shell, no OS, tiny, UID 65534) ──
FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=builder /app/target/release/zeroclaw /usr/local/bin/zeroclaw

# Default workspace (owned by nonroot user)
VOLUME ["/workspace"]
ENV ZEROCLAW_WORKSPACE=/workspace

# Explicitly set non-root user (distroless:nonroot defaults to 65534, but be explicit)
USER 65534:65534

ENTRYPOINT ["zeroclaw"]
CMD ["gateway"]
