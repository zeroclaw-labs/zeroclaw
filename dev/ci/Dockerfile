# syntax=docker/dockerfile:1.7

FROM rust:1.98-slim@sha256:cc0448b41c3b7b7fea44f5dc50eacba729a56db365b65b7bd5e8a82d5b3db078

# Tools the checks need that the slim base does not ship:
# - g++: inkjet (zerocode's syntax highlighter) compiles its tree-sitter
#   grammars as C++; the base has cc but no C++ compiler.
# - ripgrep: provider dispatch gate in scripts/ci/rust_quality_gate.sh.
# - procps: tests shell out to kill/ps/pgrep.
# - python3, jq: release-workflow architecture tests run the release scripts.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    git \
    pkg-config \
    libssl-dev \
    curl \
    g++ \
    ripgrep \
    procps \
    python3 \
    jq \
    && rm -rf /var/lib/apt/lists/*

RUN rustup toolchain install 1.98.0 --profile minimal --component rustfmt --component clippy

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo install --locked cargo-audit --version 0.22.1 && \
    cargo install --locked cargo-deny --version 0.18.5

WORKDIR /workspace

CMD ["bash"]
