# syntax=docker/dockerfile:1.7

FROM rust:1.99.1-slim@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    git \
    pkg-config \
    libssl-dev \
    curl \
    && rm -rf /var/lib/apt/lists/*

RUN rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy
RUN rustup default 1.99.1

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo install --locked cargo-audit --version 0.22.1 && \
    cargo install --locked cargo-deny --version 0.18.5

WORKDIR /workspace

CMD ["bash"]
