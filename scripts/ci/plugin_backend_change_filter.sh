#!/usr/bin/env bash

# Classifies changed paths for the plugin backend CI job.
#
# Reads one changed path per line on stdin and prints "true" when any path
# affects the plugin backends or the feature-gated runtime coverage they
# carry (the live-config plugin regression compiles zeroclaw-runtime with
# plugins-wasm-cranelift, so runtime-only changes must run the job too).
# zeroclaw-config is listed for the same reason: it is the canonical home of
# the operator-facing plugin config surface that zeroclaw-plugins compiles
# against, so a change there can break this job while nothing under
# crates/zeroclaw-plugins moves.
# Prints "false" otherwise. Always exits 0; the workflow step forwards the
# printed value to GITHUB_OUTPUT.

set -euo pipefail

run=false

while IFS= read -r path; do
    case "$path" in
        crates/zeroclaw-plugins/*|\
        crates/zeroclaw-runtime/*|\
        crates/zeroclaw-config/*|\
        wit/*|\
        Cargo.toml|Cargo.lock|\
        .github/workflows/ci.yml|\
        scripts/ci/plugin_backend_change_filter*.sh)
            run=true
            ;;
    esac
done

echo "$run"
