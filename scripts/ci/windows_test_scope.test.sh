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
    "$repo_root/crates/zeroclaw-config" \
    "$repo_root/crates/zeroclaw-plugins" \
    "$repo_root/crates/zeroclaw-runtime" \
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
    {"id": "path+file://${repo_root}/crates/zeroclaw-config#zeroclaw-config 0.8.4", "name": "zeroclaw-config", "manifest_path": "crates/zeroclaw-config/Cargo.toml"},
    {"id": "path+file://${repo_root}/crates/zeroclaw-plugins#zeroclaw-plugins 0.8.4", "name": "zeroclaw-plugins", "manifest_path": "crates/zeroclaw-plugins/Cargo.toml"},
    {"id": "path+file://${repo_root}/crates/zeroclaw-runtime#zeroclaw-runtime 0.8.4", "name": "zeroclaw-runtime", "manifest_path": "crates/zeroclaw-runtime/Cargo.toml"},
    {"id": "path+file://${repo_root}/crates/zeroclaw-gateway#zeroclaw-gateway 0.8.4", "name": "zeroclaw-gateway", "manifest_path": "crates/zeroclaw-gateway/Cargo.toml"},
    {"id": "path+file://${repo_root}/crates/zeroclaw-providers#zeroclaw-providers 0.8.4", "name": "zeroclaw-providers", "manifest_path": "crates/zeroclaw-providers/Cargo.toml"},
    {"id": "path+file://${repo_root}/crates/zeroclaw-plugins/tests/fixtures/channel-fixture#zeroclaw-channel-plugin-fixture 0.1.0", "name": "zeroclaw-channel-plugin-fixture", "manifest_path": "crates/zeroclaw-plugins/tests/fixtures/channel-fixture/Cargo.toml"},
    {"id": "path+file://${repo_root}/apps/tauri#zeroclaw-desktop 0.8.4", "name": "zeroclaw-desktop", "manifest_path": "apps/tauri/Cargo.toml"}
  ],
  "workspace_members": [
    "path+file://${repo_root}#zeroclaw 0.8.4",
    "path+file://${repo_root}/crates/zeroclaw-api#zeroclaw-api 0.8.4",
    "path+file://${repo_root}/crates/zeroclaw-channels#zeroclaw-channels 0.8.4",
    "path+file://${repo_root}/crates/zeroclaw-config#zeroclaw-config 0.8.4",
    "path+file://${repo_root}/crates/zeroclaw-plugins#zeroclaw-plugins 0.8.4",
    "path+file://${repo_root}/crates/zeroclaw-runtime#zeroclaw-runtime 0.8.4",
    "path+file://${repo_root}/crates/zeroclaw-gateway#zeroclaw-gateway 0.8.4",
    "path+file://${repo_root}/crates/zeroclaw-providers#zeroclaw-providers 0.8.4",
    "path+file://${repo_root}/crates/zeroclaw-plugins/tests/fixtures/channel-fixture#zeroclaw-channel-plugin-fixture 0.1.0",
    "path+file://${repo_root}/apps/tauri#zeroclaw-desktop 0.8.4"
  ],
  "resolve": {
    "nodes": [
      {"id": "path+file://${repo_root}#zeroclaw 0.8.4", "deps": [{"pkg": "path+file://${repo_root}/crates/zeroclaw-api#zeroclaw-api 0.8.4"}, {"pkg": "path+file://${repo_root}/crates/zeroclaw-channels#zeroclaw-channels 0.8.4"}, {"pkg": "path+file://${repo_root}/crates/zeroclaw-config#zeroclaw-config 0.8.4"}, {"pkg": "path+file://${repo_root}/crates/zeroclaw-gateway#zeroclaw-gateway 0.8.4"}, {"pkg": "path+file://${repo_root}/crates/zeroclaw-providers#zeroclaw-providers 0.8.4"}, {"pkg": "path+file://${repo_root}/crates/zeroclaw-runtime#zeroclaw-runtime 0.8.4"}]},
      {"id": "path+file://${repo_root}/crates/zeroclaw-api#zeroclaw-api 0.8.4", "deps": []},
      {"id": "path+file://${repo_root}/crates/zeroclaw-channels#zeroclaw-channels 0.8.4", "deps": []},
      {"id": "path+file://${repo_root}/crates/zeroclaw-config#zeroclaw-config 0.8.4", "deps": []},
      {"id": "path+file://${repo_root}/crates/zeroclaw-plugins#zeroclaw-plugins 0.8.4", "deps": [{"pkg": "path+file://${repo_root}/crates/zeroclaw-api#zeroclaw-api 0.8.4"}]},
      {"id": "path+file://${repo_root}/crates/zeroclaw-runtime#zeroclaw-runtime 0.8.4", "deps": []},
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
    local expected_warning_path="${7:-}"
    local output
    local warning_file="${fixture_dir}/warning"
    : > "$warning_file"
    output="$(run_selector pull_request "$paths_file" "$metadata_file" 2> "$warning_file")"
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
    if [[ -n "$expected_warning_path" ]]; then
        grep -F "Unclassified changed path '${expected_warning_path}' selected the full Windows suite" "$warning_file" >/dev/null
    elif [[ -s "$warning_file" ]]; then
        echo "FAIL: unexpected selector warning for ${name}" >&2
        cat "$warning_file" >&2
        exit 1
    fi
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

printf '%s\n' 'crates/zeroclaw-channels/locales/en/cli.ftl' > "$paths_file"
assert_selection "package locale resource" scoped '["zeroclaw","zeroclaw-channels"]' '' "$paths_file"

printf '%s\n' 'crates/zeroclaw-channels/build.rs' > "$paths_file"
assert_selection "package build script" scoped '["zeroclaw","zeroclaw-channels"]' '' "$paths_file"

printf '%s\n' 'build.rs' > "$paths_file"
assert_selection "root build script" full '[]' '' "$paths_file"

printf '%s\n' 'crates/zeroclaw-runtime/locales/en/cli.ftl' > "$paths_file"
assert_selection "plugin-host package locale resource" scoped '["zeroclaw","zeroclaw-runtime"]' '' "$paths_file" true

printf '%s\n' 'locales/en/cli.ftl' > "$paths_file"
assert_selection "root package locale resource" full '[]' '' "$paths_file"

printf '%s\n' 'crates/zeroclaw-channels/assets/locales/en/cli.ftl' > "$paths_file"
assert_selection "nested locale-like resource" full '[]' '' "$paths_file" false 'crates/zeroclaw-channels/assets/locales/en/cli.ftl'

printf '%s\n' 'crates/zeroclaw-channels/fuzz/fuzz_targets/parser.rs' > "$paths_file"
assert_selection "unclassified member Rust path" full '[]' '' "$paths_file" false 'crates/zeroclaw-channels/fuzz/fuzz_targets/parser.rs'

printf '%s\n' 'crates/zeroclaw-channels/assets/generated.bin' > "$paths_file"
assert_selection "unclassified member asset" full '[]' '' "$paths_file" false 'crates/zeroclaw-channels/assets/generated.bin'

printf '%s\n' 'crates/zeroclaw-channels/src/lib.rs' 'crates/zeroclaw-runtime/assets/generated.bin' > "$paths_file"
assert_selection "unclassified plugin-host path with scoped path" full '[]' '' "$paths_file" true 'crates/zeroclaw-runtime/assets/generated.bin'

printf '%s\n' 'crates/zeroclaw-plugins/tests/fixtures/channel-fixture/src/lib.rs' > "$paths_file"
assert_selection "dynamically consumed plugin fixture" full '[]' 'Dynamically consumed plugin test fixtures require the full suite.' "$paths_file" true

for plugin_path in \
    'crates/zeroclaw-plugins/src/lib.rs' \
    'crates/zeroclaw-runtime/src/lib.rs' \
    'crates/zeroclaw-config/src/lib.rs' \
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
assert_selection "unknown path" full '[]' '' "$paths_file" false 'crates/unknown/src/lib.rs'

printf '%s\n' 'crates/zeroclaw-channels/config/ambiguous.yaml' > "$paths_file"
assert_selection "ambiguous package path" full '[]' '' "$paths_file" false 'crates/zeroclaw-channels/config/ambiguous.yaml'

printf '%s\n' 'Cargo.lock' > "$paths_file"
assert_selection "lockfile only" full '[]' 'Cargo.lock changes require the full suite.' "$paths_file" true

printf '%s\n' 'Cargo.lock' 'crates/zeroclaw-channels/Cargo.toml' > "$paths_file"
assert_selection "manifest plus lockfile" full '[]' 'Cargo.lock changes require the full suite.' "$paths_file" true

printf '%s\n' 'Cargo.lock' 'crates/zeroclaw-channels/Cargo.toml' 'crates/zeroclaw-providers/src/lib.rs' > "$paths_file"
assert_selection "lockfile with multiple packages" full '[]' 'Cargo.lock changes require the full suite.' "$paths_file" true

printf '%s\n' 'Cargo.lock' 'crates/zeroclaw-channels/src/lib.rs' > "$paths_file"
assert_selection "lockfile with source change" full '[]' 'Cargo.lock changes require the full suite.' "$paths_file" true

assert_selection "desktop exclusion" skip '[]' 'No covered Rust compilation or test paths changed.' <(printf '%s\n' 'apps/tauri/src/main.rs')
assert_selection "desktop config exclusion" skip '[]' 'No covered Rust compilation or test paths changed.' <(printf '%s\n' 'apps/tauri/tauri.conf.json')

printf '%s\n' '.cargo/config.toml' > "$paths_file"
assert_selection "cargo configuration" full '[]' '' "$paths_file" true

printf '%s\n' '.github/actions/rust-cache/action.yml' > "$paths_file"
assert_selection "workflow action" full '[]' '' "$paths_file" true

printf '%s\n' 'wit/zeroclaw-plugin.wit' > "$paths_file"
assert_selection "WIT interface" full '[]' '' "$paths_file" true

printf '%s\n' 'rust-toolchain.toml' > "$paths_file"
assert_selection "Rust toolchain" full '[]' '' "$paths_file" true

printf '%s\n' '.github/workflows/ci.yml' > "$paths_file"
assert_selection "workflow itself exercises plugin host path" full '[]' '' "$paths_file" true

printf '%s\n' '.github/workflows/windows-tests.yml' > "$paths_file"
assert_selection "label-gated workflow exercises plugin host path" full '[]' '' "$paths_file" true

printf '%s\n' '.github/workflows/pr-size-labeler.yml' > "$paths_file"
assert_selection "known independent workflow only" skip '[]' 'No covered Rust compilation or test paths changed.' "$paths_file"

printf '%s\n' '.github/workflows/pr-size-labeler.yml' 'crates/zeroclaw-channels/src/lib.rs' > "$paths_file"
assert_selection "known independent workflow with package source" scoped '["zeroclaw","zeroclaw-channels"]' '' "$paths_file"

printf '%s\n' '.github/workflows/new-reusable-workflow.yml' 'crates/zeroclaw-channels/src/lib.rs' > "$paths_file"
assert_selection "unknown workflow with package source remains full" full '[]' '' "$paths_file"

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

package_args_file="$repo_root/package-args"
python3 "$selector" --package-args-json '["zeroclaw","zeroclaw-channels"]' > "$package_args_file"
PACKAGE_ARGS_FILE="$package_args_file" python3 - <<'PY'
import os
from pathlib import Path

actual = Path(os.environ["PACKAGE_ARGS_FILE"]).read_bytes()
expected = b"-p\nzeroclaw\n-p\nzeroclaw-channels\n"
assert actual == expected, actual
PY

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

SELECTOR="$selector" METADATA="$metadata_file" REPO_ROOT="$repo_root" FIXTURE_DIR="$fixture_dir" python3 - <<'PY'
import copy
import json
import os
import subprocess
from pathlib import Path

selector = os.environ["SELECTOR"]
root = Path(os.environ["REPO_ROOT"])
fixtures = Path(os.environ["FIXTURE_DIR"])
metadata = json.loads(Path(os.environ["METADATA"]).read_text())
packages = metadata["packages"]
by_id = {package["id"]: package for package in packages}
for node in metadata["resolve"]["nodes"]:
    by_id[node["id"]]["dependencies"] = [
        {"name": by_id[edge["pkg"]]["name"], "path": str(root / Path(by_id[edge["pkg"]]["manifest_path"]).parent)}
        for edge in node["deps"]
    ]
metadata["resolve"] = None
for name, path in (("zeroclaw-spawn", "crates/zeroclaw-spawn"), ("zerocode", "apps/zerocode")):
    package = {"id": name, "name": name, "manifest_path": f"{path}/Cargo.toml", "dependencies": []}
    packages.append(package)
    metadata["workspace_members"].append(name)
by_name = {package["name"]: package for package in packages}
for dependent, dependency in (("zeroclaw-runtime", "zeroclaw-config"), ("zeroclaw-channels", "zeroclaw-config"), ("zeroclaw-config", "zeroclaw-api"), ("zeroclaw-runtime", "zeroclaw-spawn")):
    by_name[dependent]["dependencies"].append({
        "name": dependency, "rename": "renamed_dependency", "optional": True,
        "kind": "dev", "target": "cfg(windows)",
        "path": str(root / "crates" / dependency),
    })

flags = ("windows_root", "windows_voice_wake", "windows_recovery", "windows_service")
paths_file = fixtures / "required-paths"
metadata_file = fixtures / "no-deps.json"

def run(paths, expected, *, event="pull_request", graph=metadata):
    paths_file.write_text("\n".join(paths) + ("\n" if paths else ""))
    metadata_file.write_text(json.dumps(graph))
    command = ["python3", selector, "--required-jobs", "--event", event,
               "--changed-paths-file", str(paths_file), "--metadata-file", str(metadata_file),
               "--repo-root", str(root)]
    result = subprocess.run(command, capture_output=True, text=True, check=True)
    actual = dict(line.split("=", 1) for line in result.stdout.splitlines())
    assert actual == dict(zip(flags, ("true" if value else "false" for value in expected))), (paths, actual)

run(["apps/zerocode/src/chat.rs"], (False, False, False, False))
run(["apps/zerocode/Cargo.toml"], (False, False, False, False))
run(["apps/zerocode/build.rs"], (False, False, False, False))
run(["docs/README.md"], (False, False, False, False))
run(["src/main.rs"], (True, False, False, False))
run(["crates/zeroclaw-runtime/src/sop/engine.rs", "src/main.rs"], (True, False, False, False))
run(["crates/zeroclaw-gateway/Cargo.toml"], (True, False, False, False))
run(["crates/zeroclaw-gateway/build.rs"], (True, False, False, False))
run(["crates/zeroclaw-config/src/lib.rs"], (True, True, False, True))
run(["crates/zeroclaw-api/Cargo.toml"], (True, True, True, True))
run(["crates/zeroclaw-runtime/Cargo.toml"], (True, False, True, True))
run(["crates/zeroclaw-spawn/src/lib.rs"], (True, False, False, True))
for path in ("crates/zeroclaw-runtime/src/control_plane/authority.rs", "crates/zeroclaw-runtime/src/control_plane/task_registry.rs"):
    run([path], (True, False, True, False))
for path in ("crates/zeroclaw-runtime/src/service/mod.rs", "crates/zeroclaw-runtime/examples/windows_service_smoke_fixture.rs"):
    run([path], (True, False, False, True))
run(["crates/zeroclaw-runtime/src/lib.rs"], (True, False, True, True))
for path in ("Cargo.lock", "Cargo.toml", "build.rs", "rust-toolchain.toml", ".cargo/config.toml", ".github/actions/rust-cache/action.yml", ".github/workflows/ci.yml", "scripts/ci/windows_service_smoke.ps1", "crates/unknown/Cargo.toml", "../outside.rs"):
    run([path], (True, True, True, True))
run([], (True, True, True, True))
for event in ("push", "merge_group", "unknown"):
    run(["docs/README.md"], (True, True, True, True), event=event)
for graph in ({}, {"packages": [], "workspace_members": []}, None):
    run(["apps/zerocode/src/chat.rs"], (True, True, True, True), graph=graph)
for mutation in ("missing-list", "missing-owner", "unknown-edge", "non-path-edge", "invalid-path", "duplicate-name"):
    broken = copy.deepcopy(metadata)
    if mutation == "missing-list":
        del broken["packages"][0]["dependencies"]
    elif mutation == "missing-owner":
        broken["packages"] = [p for p in broken["packages"] if p["name"] != "zeroclaw-spawn"]
        broken["workspace_members"].remove("zeroclaw-spawn")
    elif mutation == "unknown-edge":
        broken["packages"][0]["dependencies"][0]["path"] = "/missing-package"
    elif mutation == "non-path-edge":
        del broken["packages"][0]["dependencies"][0]["path"]
    elif mutation == "invalid-path":
        broken["packages"][0]["dependencies"][0]["path"] = "/invalid\x00path"
    else:
        broken["packages"][-1]["name"] = "zeroclaw"
    run(["apps/zerocode/src/chat.rs"], (True, True, True, True), graph=broken)
for missing_input in ("--changed-paths-file", "--metadata-file"):
    command = ["python3", selector, "--required-jobs", "--event", "pull_request",
               "--changed-paths-file", str(paths_file), "--metadata-file", str(metadata_file),
               "--repo-root", str(root)]
    command[command.index(missing_input) + 1] = str(fixtures / "absent-evidence")
    result = subprocess.run(command, capture_output=True, text=True, check=True)
    assert dict(line.split("=", 1) for line in result.stdout.splitlines()) == dict.fromkeys(flags, "true")

# Advisory input semantics have not silently switched to the unresolved graph.
advisory = subprocess.run(["python3", selector, "--event", "pull_request", "--changed-paths-file", str(paths_file), "--metadata-file", str(metadata_file), "--repo-root", str(root)], capture_output=True, text=True, check=True)
assert "mode=full\n" in advisory.stdout
print("Required Windows package ownership: pass")
PY

WORKFLOW="$workflow" python3 - <<'PY'
import itertools
import json
import os
import subprocess
import textwrap
from pathlib import Path

workflow = Path(os.environ["WORKFLOW"]).read_text()
advisory = Path(os.environ["WORKFLOW"]).with_name("windows-tests.yml").read_text()
assert "\n  workflow_dispatch:" not in advisory
assert "\n  pull_request:" in advisory
assert "types: [opened, synchronize, reopened, labeled]" in advisory
assert "if: contains(github.event.pull_request.labels.*.name, 'ci:windows') && (github.event.action != 'labeled' || github.event.label.name == 'ci:windows')" in advisory
assert "github.event.action == 'labeled' && github.event.label.name != 'ci:windows' && github.run_id || 'selected'" in advisory
assert "\n  pull_request_target:" not in advisory
assert "\n  push:" not in advisory
assert "\n  schedule:" not in advisory
assert "\n  windows-test:" not in workflow
assert "save-if: false" in advisory
assert "persist-credentials: false" in advisory
assert "ref: ${{ github.sha }}" in advisory
assert "toolchain: 1.98.0\n          components: rustfmt" in advisory
plugin_backend_job = workflow.split("\n  check-plugin-backends:\n", 1)[1].split(
    "\n  msrv:\n", 1
)[0]
scope_job = advisory.split("\n  windows-test-scope:\n", 1)[1].split(
    "\n  windows-test:\n", 1
)[0]
windows_job = advisory.split("\n  windows-test:\n", 1)[1].split(
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

build = workflow.split("\n  build:\n", 1)[1].split("\n  windows-build:\n", 1)[0]
required = workflow.split("\n  windows-required-changes:\n", 1)[1].split("\n  windows-service-smoke:\n", 1)[0]
windows_build = workflow.split("\n  windows-build:\n", 1)[1].split("\n  windows-task-owner-recovery:\n", 1)[0]
gate = workflow.split("\n  gate:\n", 1)[1]
assert "windows-required-changes" not in build
assert "- os: windows-latest" not in build
assert "- os: blacksmith-8vcpu-ubuntu-2404" in build
assert "- os: macos-14" in build
assert "cargo metadata --locked --offline --no-deps --format-version 1" in required
assert 'git diff --name-only "$BASE_SHA" HEAD' in required
assert "windows-required-changes" in gate.split("\n    runs-on:", 1)[0]
assert "windows-build" in gate.split("\n    runs-on:", 1)[0]
assert "shared-key: build" in windows_build
assert "cargo check --profile ci --locked --target x86_64-pc-windows-msvc" in windows_build
assert "cargo check --locked -p zeroclaw-channels --no-default-features --features voice-wake --target x86_64-pc-windows-msvc" in windows_build
assert "if: needs.windows-required-changes.outputs.windows_root == 'true'" in windows_build
assert "if: needs.windows-required-changes.outputs.windows_voice_wake == 'true'" in windows_build

# Execute the workflow's actual consistency guard, not a second implementation.
program = textwrap.dedent(gate.split("python3 - <<'PY'\n", 1)[1].split("\n          PY", 1)[0])
flags = ("windows_root", "windows_voice_wake", "windows_service", "windows_recovery")
jobs = ("windows-build", "windows-service-smoke", "windows-task-owner-recovery")

def guard(needs, expected):
    result = subprocess.run(["python3", "-c", program], env={**os.environ, "NEEDS_JSON": json.dumps(needs)}, capture_output=True, text=True)
    assert (result.returncode == 0) == expected, (needs, result.stderr)

for bits in itertools.product((False, True), repeat=4):
    needs = {"windows-required-changes": {"result": "success", "outputs": dict(zip(flags, ("true" if value else "false" for value in bits)))}}
    selected = (bits[0] or bits[1], bits[2], bits[3])
    for job, run in zip(jobs, selected):
        needs[job] = {"result": "success" if run else "skipped"}
    guard(needs, True)
    for job, run in zip(jobs, selected):
        original = needs[job]["result"]
        for invalid in (("skipped", "failure", "cancelled") if run else ("success",)):
            needs[job]["result"] = invalid
            guard(needs, False)
        needs[job]["result"] = original
    for invalid in ("failure", "cancelled", "skipped"):
        needs["windows-required-changes"]["result"] = invalid
        guard(needs, False)
    needs["windows-required-changes"]["result"] = "success"
    for flag in flags:
        original = needs["windows-required-changes"]["outputs"].pop(flag)
        guard(needs, False)
        needs["windows-required-changes"]["outputs"][flag] = original
print("Required Windows gate consistency: pass")
PY

echo "windows test scope contract tests: pass"
