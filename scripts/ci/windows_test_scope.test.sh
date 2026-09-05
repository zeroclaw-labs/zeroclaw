#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
selector="${script_dir}/windows_test_scope.py"
workflow="${script_dir}/../../.github/workflows/ci.yml"
fixture_dir="$(mktemp -d)"
trap 'rm -rf "$fixture_dir"' EXIT

repo_root="${fixture_dir}/repo"
mkdir -p "$repo_root/crates/zeroclaw-channels" \
    "$repo_root/crates/zeroclaw-api" \
    "$repo_root/crates/zeroclaw-plugins" \
    "$repo_root/crates/zeroclaw-gateway" \
    "$repo_root/crates/zeroclaw-providers" \
    "$repo_root/crates/zeroclaw-plugins/tests/fixtures/channel-fixture" \
    "$repo_root/apps/tauri"
metadata_file="${fixture_dir}/metadata.json"
cat > "$metadata_file" <<EOF
{
  "packages": [
    {"id": "path+file://${repo_root}#zeroclaw 0.8.4", "name": "zeroclaw", "manifest_path": "Cargo.toml"},
    {"id": "path+file://${repo_root}/crates/zeroclaw-api#zeroclaw-api 0.8.4", "name": "zeroclaw-api", "manifest_path": "crates/zeroclaw-api/Cargo.toml"},
    {"id": "path+file://${repo_root}/crates/zeroclaw-channels#zeroclaw-channels 0.8.4", "name": "zeroclaw-channels", "manifest_path": "crates/zeroclaw-channels/Cargo.toml"},
    {"id": "path+file://${repo_root}/crates/zeroclaw-plugins#zeroclaw-plugins 0.8.4", "name": "zeroclaw-plugins", "manifest_path": "crates/zeroclaw-plugins/Cargo.toml"},
    {"id": "path+file://${repo_root}/crates/zeroclaw-gateway#zeroclaw-gateway 0.8.4", "name": "zeroclaw-gateway", "manifest_path": "crates/zeroclaw-gateway/Cargo.toml"},
    {"id": "path+file://${repo_root}/crates/zeroclaw-providers#zeroclaw-providers 0.8.4", "name": "zeroclaw-providers", "manifest_path": "crates/zeroclaw-providers/Cargo.toml"},
    {"id": "path+file://${repo_root}/crates/zeroclaw-plugins/tests/fixtures/channel-fixture#zeroclaw-channel-plugin-fixture 0.1.0", "name": "zeroclaw-channel-plugin-fixture", "manifest_path": "crates/zeroclaw-plugins/tests/fixtures/channel-fixture/Cargo.toml"},
    {"id": "path+file://${repo_root}/apps/tauri#zeroclaw-desktop 0.8.4", "name": "zeroclaw-desktop", "manifest_path": "apps/tauri/Cargo.toml"}
  ],
  "workspace_members": [
    "path+file://${repo_root}#zeroclaw 0.8.4",
    "path+file://${repo_root}/crates/zeroclaw-api#zeroclaw-api 0.8.4",
    "path+file://${repo_root}/crates/zeroclaw-channels#zeroclaw-channels 0.8.4",
    "path+file://${repo_root}/crates/zeroclaw-plugins#zeroclaw-plugins 0.8.4",
    "path+file://${repo_root}/crates/zeroclaw-gateway#zeroclaw-gateway 0.8.4",
    "path+file://${repo_root}/crates/zeroclaw-providers#zeroclaw-providers 0.8.4",
    "path+file://${repo_root}/crates/zeroclaw-plugins/tests/fixtures/channel-fixture#zeroclaw-channel-plugin-fixture 0.1.0",
    "path+file://${repo_root}/apps/tauri#zeroclaw-desktop 0.8.4"
  ],
  "resolve": {
    "nodes": [
      {"id": "path+file://${repo_root}#zeroclaw 0.8.4", "deps": [{"pkg": "path+file://${repo_root}/crates/zeroclaw-api#zeroclaw-api 0.8.4"}, {"pkg": "path+file://${repo_root}/crates/zeroclaw-channels#zeroclaw-channels 0.8.4"}, {"pkg": "path+file://${repo_root}/crates/zeroclaw-gateway#zeroclaw-gateway 0.8.4"}, {"pkg": "path+file://${repo_root}/crates/zeroclaw-providers#zeroclaw-providers 0.8.4"}]},
      {"id": "path+file://${repo_root}/crates/zeroclaw-api#zeroclaw-api 0.8.4", "deps": []},
      {"id": "path+file://${repo_root}/crates/zeroclaw-channels#zeroclaw-channels 0.8.4", "deps": []},
      {"id": "path+file://${repo_root}/crates/zeroclaw-plugins#zeroclaw-plugins 0.8.4", "deps": [{"pkg": "path+file://${repo_root}/crates/zeroclaw-api#zeroclaw-api 0.8.4"}]},
      {"id": "path+file://${repo_root}/crates/zeroclaw-gateway#zeroclaw-gateway 0.8.4", "deps": [{"pkg": "path+file://${repo_root}/crates/zeroclaw-providers#zeroclaw-providers 0.8.4"}]},
      {"id": "path+file://${repo_root}/crates/zeroclaw-providers#zeroclaw-providers 0.8.4", "deps": []},
      {"id": "path+file://${repo_root}/crates/zeroclaw-plugins/tests/fixtures/channel-fixture#zeroclaw-channel-plugin-fixture 0.1.0", "deps": []},
      {"id": "path+file://${repo_root}/apps/tauri#zeroclaw-desktop 0.8.4", "deps": [{"pkg": "path+file://${repo_root}/crates/zeroclaw-channels#zeroclaw-channels 0.8.4"}]}
    ]
  }
}
EOF

run_selector() {
    local event="$1"
    local paths_file="$2"
    local metadata="$3"
    python3 "$selector" --event "$event" --changed-paths-file "$paths_file" --metadata-file "$metadata" --repo-root "$repo_root"
}

assert_selection() {
    local name="$1"
    local expected_mode="$2"
    local expected_packages="$3"
    local expected_reason="$4"
    local paths_file="$5"
    local expected_plugin_host="${6:-false}"
    local output
    output="$(run_selector pull_request "$paths_file" "$metadata_file")"
    SELECTION_OUTPUT="$output" EXPECTED_MODE="$expected_mode" EXPECTED_PACKAGES="$expected_packages" EXPECTED_REASON="$expected_reason" EXPECTED_PLUGIN_HOST="$expected_plugin_host" python3 - <<'PY'
import json
import os

values = {}
for line in os.environ["SELECTION_OUTPUT"].splitlines():
    key, separator, value = line.partition("=")
    assert separator and key in {"mode", "packages", "reason", "needs_plugin_host"} and "\n" not in value
    values[key] = value
assert values["mode"] == os.environ["EXPECTED_MODE"], (values, os.environ["EXPECTED_MODE"])
assert json.loads(values["packages"]) == json.loads(os.environ["EXPECTED_PACKAGES"]), values
if os.environ["EXPECTED_REASON"]:
    assert values["reason"] == os.environ["EXPECTED_REASON"], values
assert values["needs_plugin_host"] == os.environ["EXPECTED_PLUGIN_HOST"], values
assert set(values) == {"mode", "packages", "reason", "needs_plugin_host"}, values
PY
}

paths_file="$fixture_dir/paths"
printf '' > "$paths_file"
assert_selection "empty change set" skip '[]' 'No covered Rust compilation or test paths changed.' "$paths_file"

printf '%s\n' 'docs/book/src/testing.md' > "$paths_file"
assert_selection "skip" skip '[]' 'No covered Rust compilation or test paths changed.' "$paths_file"

printf '%s\n' 'crates/zeroclaw-channels/src/lib.rs' > "$paths_file"
assert_selection "one package and reverse dependent" scoped '["zeroclaw","zeroclaw-channels"]' '' "$paths_file"

printf '%s\n' 'crates/zeroclaw-api/src/lib.rs' > "$paths_file"
assert_selection "plugin host from reverse-dependent closure" scoped '["zeroclaw","zeroclaw-api","zeroclaw-plugins"]' '' "$paths_file" true

printf '%s\n' 'crates/zeroclaw-providers/src/lib.rs' 'crates/zeroclaw-channels/src/lib.rs' > "$paths_file"
assert_selection "multiple packages" scoped '["zeroclaw","zeroclaw-channels","zeroclaw-gateway","zeroclaw-providers"]' '' "$paths_file" true

printf '%s\n' 'crates/zeroclaw-providers/src/lib.rs' > "$paths_file"
assert_selection "provider feature owner" scoped '["zeroclaw","zeroclaw-gateway","zeroclaw-providers"]' '' "$paths_file" true

printf '%s\n' 'src/lib.rs' 'tests/integration.rs' > "$paths_file"
assert_selection "root feature owner" scoped '["zeroclaw"]' '' "$paths_file" true

printf '%s\n' 'crates/zeroclaw-gateway/src/api_plugins.rs' > "$paths_file"
assert_selection "gateway feature owner" scoped '["zeroclaw","zeroclaw-gateway"]' '' "$paths_file" true

printf '%s\n' 'crates/zeroclaw-channels/src/lib.rs' 'crates/zeroclaw-channels/tests/one.rs' > "$paths_file"
assert_selection "deduplication" scoped '["zeroclaw","zeroclaw-channels"]' '' "$paths_file"

printf '%s\n' 'crates/zeroclaw-channels/tests/fixture.md' > "$paths_file"
assert_selection "test fixture" scoped '["zeroclaw","zeroclaw-channels"]' '' "$paths_file"

printf '%s\n' 'crates/zeroclaw-plugins/tests/fixtures/channel-fixture/src/lib.rs' > "$paths_file"
assert_selection "dynamically consumed plugin fixture" full '[]' 'Dynamically consumed plugin test fixtures require the full suite.' "$paths_file" true

for plugin_path in \
    'crates/zeroclaw-plugins/src/lib.rs' \
    'crates/zeroclaw-runtime/src/lib.rs' \
    'crates/zeroclaw-config/src/lib.rs' \
    'wit/zeroclaw-plugin.wit' \
    'tests/plugin_channel_runtime_e2e.rs' \
    'Cargo.toml' \
    'Cargo.lock' \
    'scripts/ci/plugin_backend_change_filter.sh' \
    'scripts/ci/plugin_backend_change_filter.test.sh' \
    'scripts/ci/plugin_backend_change_filter-v2.sh'; do
    printf '%s\n' "$plugin_path" > "$paths_file"
    output="$(run_selector pull_request "$paths_file" "$metadata_file")"
    printf '%s\n' "$output" | grep -Fx 'needs_plugin_host=true' >/dev/null
done

printf '%s\n' 'Cargo.toml' 'crates/zeroclaw-plugins/tests/fixtures/channel-fixture/src/lib.rs' > "$paths_file"
assert_selection "mixed full trigger with plugin fixture" full '[]' '' "$paths_file" true

printf '%s\n' 'Cargo.toml' > "$paths_file"
assert_selection "full workspace manifest" full '[]' '' "$paths_file" true

printf '%s\n' 'crates/unknown/src/lib.rs' > "$paths_file"
assert_selection "unknown path" full '[]' '' "$paths_file"

printf '%s\n' 'crates/zeroclaw-channels/config/ambiguous.yaml' > "$paths_file"
assert_selection "ambiguous package path" full '[]' '' "$paths_file"

printf '%s\n' 'Cargo.lock' > "$paths_file"
assert_selection "lockfile only" full '[]' 'Cargo.lock changes require the full suite.' "$paths_file" true

printf '%s\n' 'Cargo.lock' 'crates/zeroclaw-channels/Cargo.toml' > "$paths_file"
assert_selection "manifest plus lockfile" full '[]' 'Cargo.lock changes require the full suite.' "$paths_file" true

printf '%s\n' 'Cargo.lock' 'crates/zeroclaw-channels/Cargo.toml' 'crates/zeroclaw-providers/src/lib.rs' > "$paths_file"
assert_selection "lockfile with multiple packages" full '[]' 'Cargo.lock changes require the full suite.' "$paths_file" true

printf '%s\n' 'Cargo.lock' 'crates/zeroclaw-channels/src/lib.rs' > "$paths_file"
assert_selection "lockfile with source change" full '[]' 'Cargo.lock changes require the full suite.' "$paths_file" true

assert_selection "desktop exclusion" skip '[]' 'No covered Rust compilation or test paths changed.' <(printf '%s\n' 'apps/tauri/src/main.rs')

printf '%s\n' '.cargo/config.toml' > "$paths_file"
assert_selection "cargo configuration" full '[]' '' "$paths_file" true

printf '%s\n' '.github/actions/rust-cache/action.yml' > "$paths_file"
assert_selection "workflow action" full '[]' '' "$paths_file" true

printf '%s\n' 'rust-toolchain.toml' > "$paths_file"
assert_selection "Rust toolchain" full '[]' '' "$paths_file" true

printf '%s\n' '.github/workflows/ci.yml' > "$paths_file"
assert_selection "workflow itself exercises plugin host path" full '[]' '' "$paths_file" true

printf '%s\n' 'scripts/ci/windows_test_scope.py' > "$paths_file"
assert_selection "selector itself exercises plugin host path" full '[]' '' "$paths_file" true

printf '%s\n' 'scripts/ci/windows_test_scope.test.sh' > "$paths_file"
assert_selection "selector contract itself exercises plugin host path" full '[]' '' "$paths_file" true

printf '%s\n' 'crates/zeroclaw-channels/src/$(touch should-not-exist).rs' > "$paths_file"
output="$(run_selector pull_request "$paths_file" "$metadata_file")"
if [ -e "$repo_root/should-not-exist" ] || printf '%s\n' "$output" | grep -q 'should-not-exist'; then
    echo "FAIL: changed path was executed or echoed" >&2
    exit 1
fi
printf '%s\n' "$output" | while IFS= read -r line; do
    case "$line" in
        mode=*|packages=*|reason=*|needs_plugin_host=*) ;;
        *) echo "FAIL: unsafe selector output: $line" >&2; exit 1 ;;
    esac
done

for event in push merge_group workflow_dispatch unknown; do
    output="$(python3 "$selector" --event "$event" --repo-root "$repo_root")"
    printf '%s\n' "$output" | grep -Fx 'mode=full' >/dev/null
    printf '%s\n' "$output" | grep -Fx 'needs_plugin_host=true' >/dev/null
done

output="$(python3 "$selector" --event pull_request --repo-root "$repo_root")"
printf '%s\n' "$output" | grep -Fx 'mode=full' >/dev/null
printf '%s\n' "$output" | grep -Fx 'reason=Changed paths or Cargo metadata are unavailable; selecting full is safer.' >/dev/null
printf '%s\n' "$output" | grep -Fx 'needs_plugin_host=true' >/dev/null

missing_paths="$fixture_dir/missing-paths"
output="$(python3 "$selector" --event pull_request --changed-paths-file "$missing_paths" --metadata-file "$metadata_file" --repo-root "$repo_root")"
printf '%s\n' "$output" | grep -Fx 'mode=full' >/dev/null
printf '%s\n' "$output" | grep -Fx 'needs_plugin_host=true' >/dev/null

printf '%s\n' '../outside.rs' > "$paths_file"
output="$(run_selector pull_request "$paths_file" "$metadata_file")"
printf '%s\n' "$output" | grep -Fx 'mode=full' >/dev/null
printf '%s\n' "$output" | grep -Fx 'needs_plugin_host=true' >/dev/null

printf '%s\n' 'crates/zeroclaw-api/src/lib.rs' > "$paths_file"
output="$(python3 "$selector" --event pull_request --changed-paths-file "$paths_file" --repo-root "$repo_root")"
printf '%s\n' "$output" | grep -Fx 'mode=full' >/dev/null
printf '%s\n' "$output" | grep -Fx 'reason=Changed paths or Cargo metadata are unavailable; selecting full is safer.' >/dev/null
printf '%s\n' "$output" | grep -Fx 'needs_plugin_host=true' >/dev/null

malformed_metadata="$fixture_dir/malformed.json"
printf '%s\n' '{"packages": []}' > "$malformed_metadata"
printf '%s\n' 'crates/zeroclaw-api/src/lib.rs' > "$paths_file"
output="$(run_selector pull_request "$paths_file" "$malformed_metadata")"
printf '%s\n' "$output" | grep -Fx 'mode=full' >/dev/null
printf '%s\n' "$output" | grep -F 'reason=Cargo metadata is malformed or unavailable' >/dev/null
printf '%s\n' "$output" | grep -Fx 'needs_plugin_host=true' >/dev/null

missing_metadata="$fixture_dir/missing.json"
output="$(run_selector pull_request "$paths_file" "$missing_metadata")"
printf '%s\n' "$output" | grep -Fx 'mode=full' >/dev/null
printf '%s\n' "$output" | grep -Fx 'reason=Cargo metadata is malformed or unavailable (FileNotFoundError).' >/dev/null
printf '%s\n' "$output" | grep -Fx 'needs_plugin_host=true' >/dev/null

printf '%s\n' 'crates/zeroclaw-plugins/tests/fixtures/channel-fixture/src/lib.rs' > "$paths_file"
output="$(run_selector pull_request "$paths_file" "$malformed_metadata")"
printf '%s\n' "$output" | grep -Fx 'mode=full' >/dev/null
printf '%s\n' "$output" | grep -F 'reason=Cargo metadata is malformed or unavailable' >/dev/null
printf '%s\n' "$output" | grep -Fx 'needs_plugin_host=true' >/dev/null

output="$(run_selector pull_request "$paths_file" "$missing_metadata")"
printf '%s\n' "$output" | grep -Fx 'mode=full' >/dev/null
printf '%s\n' "$output" | grep -Fx 'reason=Cargo metadata is malformed or unavailable (FileNotFoundError).' >/dev/null
printf '%s\n' "$output" | grep -Fx 'needs_plugin_host=true' >/dev/null

package_args="$(python3 "$selector" --package-args-json '["zeroclaw","zeroclaw-channels"]')"
test "$package_args" = $'-p\nzeroclaw\n-p\nzeroclaw-channels'

for invalid_packages in '[]' '{}' '["zeroclaw",""]' '["zeroclaw","zeroclaw"]' '["$(touch unsafe)"]'; do
    if python3 "$selector" --package-args-json "$invalid_packages" >/dev/null 2>&1; then
        echo "FAIL: invalid package JSON was accepted: $invalid_packages" >&2
        exit 1
    fi
done

if [ -e "$repo_root/unsafe" ]; then
    echo "FAIL: package JSON was executed" >&2
    exit 1
fi

WORKFLOW="$workflow" python3 - <<'PY'
import os
from pathlib import Path

workflow = Path(os.environ["WORKFLOW"]).read_text()
plugin_backend_job = workflow.split("\n  check-plugin-backends:\n", 1)[1].split(
    "\n  msrv:\n", 1
)[0]
scope_job = workflow.split("\n  windows-test-scope:\n", 1)[1].split(
    "\n  windows-test:\n", 1
)[0]
windows_job = workflow.split("\n  windows-test:\n", 1)[1].split(
    "\n  parallel-runtime-test-changes:\n", 1
)[0]
normalization = 'archive="$(cygpath -u "$archive")"'
extraction = 'tar zxf "$archive" -C "$HOME/.cargo/bin"'
skip_condition = "needs.windows-test-scope.outputs.mode != 'skip'"
package_conversion = 'scripts/ci/windows_test_scope.py --package-args-json "$PACKAGES_JSON"'
scoped_command = 'cargo nextest run --locked --no-fail-fast "${package_args[@]}"'
full_command = 'cargo nextest run --locked --no-fail-fast --workspace --exclude zeroclaw-desktop'
plugin_condition = 'if [[ "$NEEDS_PLUGIN_HOST" == "true" ]]; then'
plugin_components_command = 'cargo nextest run --locked --no-fail-fast \\\n              -p zeroclaw-plugins'
plugin_root_command = 'cargo nextest run --locked --no-fail-fast \\\n              --features plugins-wasm-cranelift \\\n              --test plugin_channel_runtime_e2e'
plugin_lib_command = "cargo nextest run --locked --no-fail-fast \\\n              -p zeroclaw-plugins \\\n              --no-default-features \\\n              --features plugins-wasm-cranelift \\\n              --lib"
plugin_runtime_config_command = "cargo nextest run --locked --no-fail-fast \\\n              -p zeroclaw-runtime \\\n              --features plugins-wasm-cranelift \\\n              --lib \\\n              live_agent_plugin_tool_observes_config_reload_after_construction"
plugin_runtime_admission_command = "cargo nextest run --locked --no-fail-fast \\\n              -p zeroclaw-runtime \\\n              --features plugins-wasm-cranelift \\\n              --lib \\\n              plugin_runtime::"
assert "bash scripts/ci/windows_test_scope.test.sh" in scope_job
metadata_fallback = 'if ! cargo metadata --locked --format-version 1 > "$metadata_file"; then'
assert metadata_fallback in scope_job
assert 'rm -f "$metadata_file"' in scope_job
assert scope_job.index(metadata_fallback) < scope_job.index('rm -f "$metadata_file"')
assert 'needs_plugin_host: ${{ steps.select.outputs.needs_plugin_host }}' in scope_job
assert 'printf "| Plugin host required | %s |\\n" "$NEEDS_PLUGIN_HOST"' in scope_job
assert skip_condition in windows_job
assert package_conversion in windows_job
assert scoped_command in windows_job
assert full_command in windows_job
assert 'name: Advisory Windows nextest (${{ needs.windows-test-scope.outputs.mode }}, plugin-host=${{ needs.windows-test-scope.outputs.needs_plugin_host }})' in windows_job
assert "if: needs.windows-test-scope.outputs.needs_plugin_host == 'true'" in windows_job
assert 'run: rustup target add wasm32-wasip2' in windows_job
assert plugin_condition in windows_job
assert plugin_components_command in windows_job
assert plugin_lib_command in windows_job
assert plugin_runtime_config_command in windows_job
assert plugin_runtime_admission_command in windows_job
assert "-p zeroclaw-gateway" in windows_job
assert "--features plugins-wasm" in windows_job
assert "--bin zeroclaw" in windows_job
assert "plugin_registry::" in windows_job
for admission_filter in (
    "plugin_runtime::",
    "tools::tests::shared_ceiling",
    "tools::tests::repeated_loader",
    "tools::tests::auto_discover",
    "tools::tests::colliding_plugin",
):
    assert admission_filter in plugin_backend_job
    assert admission_filter in windows_job
for target in ("channel_plugin_e2e", "tool_plugin_timeout_e2e", "reference_plugin", "reference_plugin_e2e", "tool_plugin_e2e"):
    assert f"--test {target}" in plugin_backend_job
    assert f"--test {target}" in windows_job
assert plugin_root_command in windows_job
assert "--test plugin_channel_runtime_e2e" in plugin_backend_job
assert 'plugin_components_status=${PIPESTATUS[0]}' in windows_job
assert 'plugin_lib_status=${PIPESTATUS[0]}' in windows_job
assert 'plugin_runtime_config_status=${PIPESTATUS[0]}' in windows_job
assert 'plugin_runtime_admission_status=${PIPESTATUS[0]}' in windows_job
assert 'plugin_gateway_status=${PIPESTATUS[0]}' in windows_job
assert 'plugin_cli_status=${PIPESTATUS[0]}' in windows_job
assert 'plugin_root_status=${PIPESTATUS[0]}' in windows_job
assert 'plugin_gateway_status != 0' in windows_job
assert 'plugin_cli_status != 0' in windows_job
assert '}plugin-gateway"' in windows_job
assert '}plugin-cli"' in windows_job
assert 'Plugin-host gateway status' in windows_job
assert 'Plugin-host CLI status' in windows_job
assert 'Failure inventory' in windows_job
assert 'Baseline duration' in windows_job
assert 'Plugin-host duration' in windows_job
assert 'Total duration' in windows_job
assert windows_job.index("scoped)") < windows_job.index(scoped_command)
assert windows_job.index("full)") < windows_job.index(full_command)
assert windows_job.index(plugin_condition) < windows_job.index(plugin_components_command)
assert windows_job.index(plugin_components_command) < windows_job.index('plugin_components_status=${PIPESTATUS[0]}')
assert windows_job.index('plugin_components_status=${PIPESTATUS[0]}') < windows_job.index(plugin_lib_command)
assert windows_job.index(plugin_lib_command) < windows_job.index('plugin_lib_status=${PIPESTATUS[0]}')
assert windows_job.index('plugin_lib_status=${PIPESTATUS[0]}') < windows_job.index(plugin_runtime_config_command)
assert windows_job.index(plugin_runtime_config_command) < windows_job.index('plugin_runtime_config_status=${PIPESTATUS[0]}')
assert windows_job.index('plugin_runtime_config_status=${PIPESTATUS[0]}') < windows_job.index(plugin_runtime_admission_command)
assert windows_job.index(plugin_runtime_admission_command) < windows_job.index('plugin_runtime_admission_status=${PIPESTATUS[0]}')
assert windows_job.index('plugin_runtime_admission_status=${PIPESTATUS[0]}') < windows_job.index("-p zeroclaw-gateway")
assert windows_job.index("-p zeroclaw-gateway") < windows_job.index('plugin_gateway_status=${PIPESTATUS[0]}')
assert windows_job.index('plugin_gateway_status=${PIPESTATUS[0]}') < windows_job.index("--bin zeroclaw")
assert windows_job.index("--bin zeroclaw") < windows_job.index('plugin_cli_status=${PIPESTATUS[0]}')
assert windows_job.index('plugin_cli_status=${PIPESTATUS[0]}') < windows_job.index(plugin_root_command)
assert windows_job.index(plugin_root_command) < windows_job.index('plugin_root_status=${PIPESTATUS[0]}')
scoped_case = windows_job.split("\n            scoped)\n", 1)[1].split(
    "\n              ;;\n", 1
)[0]
full_case = windows_job.split("\n            full)\n", 1)[1].split(
    "\n              ;;\n", 1
)[0]
for case, command in ((scoped_case, scoped_command), (full_case, full_command)):
    assert case.index("set +e") < case.index(command)
    assert case.index(command) < case.index("baseline_status=${PIPESTATUS[0]}")
    assert case.index("baseline_status=${PIPESTATUS[0]}") < case.index("set -e")
assert '\n          exit "$overall_status"' in windows_job
assert normalization in windows_job
assert extraction in windows_job
assert windows_job.index(normalization) < windows_job.index(extraction)
PY

echo "windows test scope contract tests: pass"
