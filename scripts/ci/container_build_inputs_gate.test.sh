#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
gate="${root_dir}/scripts/ci/container_build_inputs_gate.sh"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT

expect_fail() {
    local name="$1" file="$2" needle="$3"
    local output
    if output="$(bash "$gate" "$file" 2>&1)"; then
        echo "expected ${name} to fail, but it passed" >&2
        echo "$output" >&2
        exit 1
    fi
    if ! grep -q -- "$needle" <<<"$output"; then
        echo "expected ${name} to report '${needle}'" >&2
        echo "$output" >&2
        exit 1
    fi
}

expect_pass() {
    local name="$1" file="$2"
    if ! output="$(bash "$gate" "$file" 2>&1)"; then
        echo "expected ${name} to pass" >&2
        echo "$output" >&2
        exit 1
    fi
}

build_only="${fixture_dir}/build-only.Dockerfile"
cat >"$build_only" <<'EOF'
FROM rust:1 AS builder
WORKDIR /app
COPY Cargo.toml .
COPY src/ src/
RUN cargo build --release --locked -p zeroclaw
EOF
expect_fail "missing root build script" "$build_only" "never copies root build.rs"

rs_copy="${fixture_dir}/rs-copy.Dockerfile"
sed 's|^COPY src/ src/$|COPY src/ src/\nCOPY *.rs .|' "$build_only" >"$rs_copy"
expect_pass "explicit *.rs copy" "$rs_copy"

context_copy="${fixture_dir}/context-copy.Dockerfile"
sed 's|^COPY src/ src/$|COPY . .|' "$build_only" >"$context_copy"
expect_pass "whole-context copy" "$context_copy"

build_stage="${fixture_dir}/other-stage.Dockerfile"
cat >"$build_stage" <<'EOF'
FROM node:22 AS web
COPY . .
RUN npm ci

FROM rust:1 AS builder
COPY src/ src/
RUN cargo build --release -p zeroclaw
EOF
expect_fail "copy in another stage" "$build_stage" "stage 2: builds the root package"

tools_only="${fixture_dir}/tools-only.Dockerfile"
cat >"$tools_only" <<'EOF'
FROM rust:1
RUN cargo install --locked cargo-audit --version 0.22.1
EOF
expect_pass "unrelated cargo install" "$tools_only"

commented="${fixture_dir}/commented.Dockerfile"
cat >"$commented" <<'EOF'
FROM rust:1
COPY src/ src/
# RUN cargo build --release -p zeroclaw
EOF
expect_pass "commented-out build" "$commented"

if ! output="$(cd "$root_dir" && bash "$gate" 2>&1)"; then
    echo "expected every repository definition to pass the gate" >&2
    echo "$output" >&2
    exit 1
fi

echo "container_build_inputs_gate tests passed"
