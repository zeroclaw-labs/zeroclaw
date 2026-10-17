# syntax=docker/dockerfile:1.7

FROM rust:1.99-slim@sha256:24e632c09342c20abf8312cf4f61430a911c01ed3a5e4c02b87292b1c39c5273

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    git \
    pkg-config \
    libssl-dev \
    curl \
    && rm -rf /var/lib/apt/lists/*

RUN rustup toolchain install 1.99.0 --profile minimal --component rustfmt --component clippy
RUN rustup default 1.99.0

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo install --locked cargo-audit --version 0.22.1 && \
    cargo install --locked cargo-deny --version 0.18.5

WORKDIR /workspace

CMD ["bash"]
