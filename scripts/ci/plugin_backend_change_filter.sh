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
# The root-package channel activation and channel egress e2e targets are listed
# individually because they are the pieces of this job's coverage that live
# outside a crate directory: they drive zeroclaw-runtime, which zeroclaw-plugins
# cannot depend on without inverting the crate graph, so they have to be root
# `zeroclaw` test targets. The root binary's plugin modules are listed for the
# same reason: `mod plugins` and the plugin registry only compile under
# `plugins-wasm`, so the default-feature Test job cannot run their tests and
# this job is where they run. `src/main.rs` holds the plugin CLI itself.
# The gateway's `/plugin/{path}` ingress is listed for the same reason: its
# handler and HTTP tests compile only under the gateway's `plugins-wasm`
# feature, so this job is the only required one that runs them and the plugin
# webhook golden frames that pin its public HTTP contract. The paths are the
# handler and its tests, the router that mounts it, the manifest that defines
# the feature, the golden harness and fixtures, the shared webhook types in
# zeroclaw-api, and the plugin webhook ingress in zeroclaw-infra. The infra
# crate root and manifest are listed too: the root holds the dedup limit
# helpers and the committed-key set the ingress stores its delivered message
# keys in, and the manifest defines the ingress's dependencies. Other gateway
# and infra paths stay out: the default-feature Test job covers them.
# The standalone gateway's forwarder and the two-process IPC proof run only
# here, so the gateway's core connection, the RPC client and wire crates they
# compile against, the JSON-RPC request plumbing in zeroclaw-api that the
# client sends through (`RpcOutbound`), the root IPC e2e target, and the
# channel fixture helpers shared by both root e2e targets are listed too.
# Prints "false" otherwise. Always exits 0; the workflow step forwards the
# printed value to GITHUB_OUTPUT.

set -euo pipefail

run=false

while IFS= read -r path; do
    case "$path" in
        crates/zeroclaw-plugins/*|\
        crates/zeroclaw-runtime/*|\
        crates/zeroclaw-config/*|\
        tests/plugin_channel_runtime_e2e.rs|\
        tests/channel_egress_e2e.rs|\
        tests/plugin_webhook_ipc_e2e.rs|\
        tests/support/plugin_channel_fixture.rs|\
        crates/zeroclaw-gateway/src/core_rpc.rs|\
        crates/zeroclaw-rpc-client/*|\
        crates/zeroclaw-rpc-proto/*|\
        crates/zeroclaw-gateway/src/plugin_webhook*|\
        crates/zeroclaw-gateway/src/lib.rs|\
        crates/zeroclaw-gateway/Cargo.toml|\
        crates/zeroclaw-gateway/tests/golden*|\
        crates/zeroclaw-api/src/webhook.rs|\
        crates/zeroclaw-api/src/jsonrpc.rs|\
        crates/zeroclaw-infra/src/plugin_webhook*|\
        crates/zeroclaw-infra/src/lib.rs|\
        crates/zeroclaw-infra/Cargo.toml|\
        src/plugins/*|src/plugin_registry.rs|src/main.rs|\
        wit/*|\
        Cargo.toml|Cargo.lock|\
        .github/workflows/ci.yml|\
        scripts/ci/plugin_backend_change_filter*.sh)
            run=true
            ;;
    esac
done

echo "$run"
