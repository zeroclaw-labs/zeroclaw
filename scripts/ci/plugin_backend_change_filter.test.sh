#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
classifier="${script_dir}/plugin_backend_change_filter.sh"

expect() {
    local name="$1"
    local expected="$2"
    local actual
    shift 2

    actual="$(printf '%s\n' "$@" | bash "$classifier")"

    if [ "$actual" != "$expected" ]; then
        echo "FAIL: expected '$expected' for $name, got '$actual'" >&2
        exit 1
    fi
}

# Plugin-only changes must run the backend job.
expect "plugin crate source" "true" \
    "crates/zeroclaw-plugins/src/wasm_tool.rs"
expect "plugin crate tests" "true" \
    "crates/zeroclaw-plugins/tests/reference_plugin_e2e.rs"

# Runtime-only changes must run the backend job: it carries the
# feature-gated live-config regression that protects the Agent
# constructor's live_config forwarding.
expect "runtime agent constructor" "true" \
    "crates/zeroclaw-runtime/src/agent/agent.rs"
expect "runtime live-config regression" "true" \
    "crates/zeroclaw-runtime/src/agent/plugin_live_config.rs"
expect "runtime elsewhere" "true" \
    "crates/zeroclaw-runtime/src/tools/mod.rs"

# Config-only changes must run the backend job: zeroclaw-config owns the
# operator-facing plugin config surface that zeroclaw-plugins compiles
# against, so this job can break with nothing under the plugin crate touched.
expect "config plugin entry schema" "true" \
    "crates/zeroclaw-config/src/schema.rs"
expect "config crate elsewhere" "true" \
    "crates/zeroclaw-config/src/lib.rs"
expect "mixed unrelated then config" "true" \
    "web/src/pages/AgentChat.tsx" \
    "crates/zeroclaw-config/src/schema.rs"

# The root-package channel activation e2e must run the backend job. It is the
# only piece of this job's coverage that lives outside a crate directory,
# because it drives zeroclaw-runtime from the root `zeroclaw` package.
expect "root channel activation e2e" "true" \
    "tests/plugin_channel_runtime_e2e.rs"
expect "mixed unrelated then activation e2e" "true" \
    "web/src/pages/AgentChat.tsx" \
    "tests/plugin_channel_runtime_e2e.rs"

expect "wit contracts" "true" "wit/v0/tool-plugin.wit"
expect "workspace manifest" "true" "Cargo.toml"
expect "workspace lockfile" "true" "Cargo.lock"
expect "ci workflow" "true" ".github/workflows/ci.yml"
expect "the filter itself" "true" \
    "scripts/ci/plugin_backend_change_filter.sh"
expect "the filter fixture" "true" \
    "scripts/ci/plugin_backend_change_filter.test.sh"
expect "mixed unrelated then runtime" "true" \
    "docs/book/src/plugins/typed-config.md" \
    "crates/zeroclaw-runtime/src/agent/agent.rs"

# Unrelated changes must keep the job skipped.
expect "docs-only changes" "false" \
    "docs/book/src/contributing/testing.md"
expect "web-only changes" "false" "web/src/pages/AgentChat.tsx"
expect "unrelated crate changes" "false" \
    "crates/zeroclaw-providers/src/openai.rs"
expect "other workflow changes" "false" \
    ".github/workflows/release.yml"
# The activation e2e is matched by exact path, not by a `tests/*` wildcard, so
# the rest of the root test suite must stay outside this job.
expect "other root tests" "false" "tests/test_live.rs"
expect "empty input" "false"

echo "plugin backend change filter tests: pass"
