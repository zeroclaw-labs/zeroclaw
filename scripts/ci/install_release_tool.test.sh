#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
installer="${root_dir}/scripts/ci/install_release_tool.sh"
cross_platform_workflow="${root_dir}/.github/workflows/cross-platform-build-manual.yml"

assert_manifest() {
  local tool="$1"
  local os="$2"
  local arch="$3"
  local expected_version="$4"
  local expected_asset="$5"
  local expected_primary_binary="$6"
  local expected_sha256="$7"
  local expected_url="$8"
  local expected_binaries="$9"
  local manifest

  manifest="$(
    ZEROCLAW_RELEASE_TOOL_OS="$os" \
      ZEROCLAW_RELEASE_TOOL_ARCH="$arch" \
      bash "$installer" "$tool" --print-manifest
  )"

  grep -Fxq "tool=${tool}" <<<"$manifest"
  grep -Fxq "version=${expected_version}" <<<"$manifest"
  grep -Fxq "asset=${expected_asset}" <<<"$manifest"
  grep -Fxq "primary_binary=${expected_primary_binary}" <<<"$manifest"
  grep -Fxq "binaries=${expected_binaries}" <<<"$manifest"
  grep -Fxq "sha256=${expected_sha256}" <<<"$manifest"
  grep -Fxq "url=${expected_url}" <<<"$manifest"
}

assert_smoke_matrix_entry() {
  local name="$1"
  local os="$2"
  local tool="$3"

  awk -v expected_name="$name" -v expected_os="$os" -v expected_tool="$tool" '
    $0 == "          - name: " expected_name { in_entry = 1; found_name = 1; next }
    in_entry && $0 == "            os: " expected_os { found_os = 1; next }
    in_entry && $0 == "            tool: " expected_tool { found_tool = 1; next }
    in_entry && /^          - name:/ { exit }
    END { exit !(found_name && found_os && found_tool) }
  ' <<<"$smoke_job" || {
    echo "release-tool-smoke is missing matrix entry: $name / $os / $tool" >&2
    exit 1
  }
}

extract_job() {
  local job_name="$1"

  awk -v expected_job="$job_name" '
    $0 == "  " expected_job ":" { capture = 1 }
    capture && /^  [[:alnum:]_-]+:/ && $1 != expected_job ":" { exit }
    capture { print }
  ' "$cross_platform_workflow"
}

assert_manifest \
  cross Linux X64 \
  0.2.5 \
  cross-x86_64-unknown-linux-gnu.tar.gz \
  cross \
  642375d1bcf3bd88272c32ba90e999f3d983050adf45e66bd2d3887e8e838bad \
  https://github.com/cross-rs/cross/releases/download/v0.2.5/cross-x86_64-unknown-linux-gnu.tar.gz \
  cross,cross-util

assert_manifest \
  tauri-cli Linux X64 \
  2.11.4 \
  cargo-tauri-x86_64-unknown-linux-gnu.tgz \
  cargo-tauri \
  6864602a34292aa6f2ad40ae019eebe5c1064d6c623fe20696a8a8974067e60b \
  https://github.com/tauri-apps/tauri/releases/download/tauri-cli-v2.11.4/cargo-tauri-x86_64-unknown-linux-gnu.tgz \
  cargo-tauri

assert_manifest \
  tauri-cli macOS X64 \
  2.11.4 \
  cargo-tauri-x86_64-apple-darwin.zip \
  cargo-tauri \
  f10dfcc103ccb79248ca27cb9aff7b8a65499d1b0df79fe0465e8aa0a8e7cbef \
  https://github.com/tauri-apps/tauri/releases/download/tauri-cli-v2.11.4/cargo-tauri-x86_64-apple-darwin.zip \
  cargo-tauri

assert_manifest \
  tauri-cli macOS ARM64 \
  2.11.4 \
  cargo-tauri-aarch64-apple-darwin.zip \
  cargo-tauri \
  82bdcb9ae7f407882321680ae50750f11623fae22445f8b00b096e10f815d604 \
  https://github.com/tauri-apps/tauri/releases/download/tauri-cli-v2.11.4/cargo-tauri-aarch64-apple-darwin.zip \
  cargo-tauri

assert_manifest \
  tauri-cli Windows X64 \
  2.11.4 \
  cargo-tauri-x86_64-pc-windows-msvc.zip \
  cargo-tauri.exe \
  0743e30a661a35d63339b24cf63828f97ba5389a1d7f13b368a542794dd0a3f3 \
  https://github.com/tauri-apps/tauri/releases/download/tauri-cli-v2.11.4/cargo-tauri-x86_64-pc-windows-msvc.zip \
  cargo-tauri.exe

if ZEROCLAW_RELEASE_TOOL_OS=Linux ZEROCLAW_RELEASE_TOOL_ARCH=ARM64 \
  bash "$installer" cross --print-manifest >/dev/null 2>&1; then
  echo "expected cross on Linux/ARM64 to fail closed" >&2
  exit 1
fi

if bash "$installer" unknown-tool --print-manifest >/dev/null 2>&1; then
  echo "expected an unknown release tool to fail closed" >&2
  exit 1
fi

smoke_job="$(extract_job release-tool-smoke)"

release_tools_input="$({
  awk '
    /^      release_tools_only:/ { capture = 1 }
    capture && /^permissions:/ { exit }
    capture { print }
  ' "$cross_platform_workflow"
})"

workflow_permissions="$({
  awk '
    /^permissions:/ { capture = 1 }
    capture && /^env:/ { exit }
    capture { print }
  ' "$cross_platform_workflow"
})"

[[ "$workflow_permissions" == $'permissions:\n  contents: read' ]] || {
  echo "cross-platform workflow permissions must remain exactly contents: read" >&2
  exit 1
}

[[ -n "$smoke_job" ]] || {
  echo "cross-platform workflow must define release-tool-smoke" >&2
  exit 1
}

[[ "$release_tools_input" == $'      release_tools_only:\n        description: Skip release builds and run only the native release-tool smoke\n        required: false\n        type: boolean\n        default: false' ]] || {
  echo "release-tools-only dispatch must remain an optional boolean defaulting to false" >&2
  exit 1
}

for expected in \
  'uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd # v6.0.2' \
  'uses: dtolnay/rust-toolchain@631a55b12751854ce901bb631d5902ceb48146f7 # stable' \
  'runs-on: ${{ matrix.os }}' \
  'fail-fast: false' \
  'test "$RUNNER_ARCH" = "X64"' \
  'test "$(git rev-parse HEAD)" = "$GITHUB_SHA"' \
  'bash scripts/ci/install_release_tool.sh "${{ matrix.tool }}"' \
  'cargo_home="${CARGO_HOME:-${HOME}/.cargo}"' \
  'cargo_home="$(cygpath -u "$cargo_home")"' \
  'test "$cross_path" = "$expected_cross_path"' \
  'test "$cross_util_path" = "$expected_cross_util_path"' \
  "if: matrix.tool == 'cross'" \
  'command -v cross-util' \
  'cross --version' \
  "if: matrix.tool == 'tauri-cli'" \
  'test "$tauri_path" = "$expected_tauri_path"' \
  'cargo-tauri.exe --version' \
  'cargo tauri --version'; do
  grep -Fq "$expected" <<<"$smoke_job" || {
    echo "release-tool-smoke is missing: $expected" >&2
    exit 1
  }
done

if grep -Eq '^    (if|needs):' <<<"$smoke_job"; then
  echo "release-tool-smoke must remain independent and unconditional" >&2
  exit 1
fi

assert_smoke_matrix_entry "Linux cross" "ubuntu-22.04" "cross"
assert_smoke_matrix_entry "Windows Tauri CLI" "windows-latest" "tauri-cli"

if [[ "$(grep -c '^          - name:' <<<"$smoke_job")" -ne 2 ]]; then
  echo "release-tool-smoke must keep exactly two native matrix legs" >&2
  exit 1
fi

if [[ "$(grep -c '^      - uses:' <<<"$smoke_job")" -ne 2 ]]; then
  echo "release-tool-smoke must use only checkout and the Rust toolchain action" >&2
  exit 1
fi

for forbidden in \
  'permissions:' \
  'environment:' \
  '${{ secrets.' \
  'upload-artifact' \
  'download-artifact' \
  'gh release' \
  'docker' \
  'publish'; do
  if grep -Fqi "$forbidden" <<<"$smoke_job"; then
    echo "release-tool-smoke contains forbidden write or publishing surface: $forbidden" >&2
    exit 1
  fi
done

for job_name in web build; do
  job="$(extract_job "$job_name")"
  [[ -n "$job" ]] || {
    echo "cross-platform workflow must define $job_name" >&2
    exit 1
  }
  grep -Fxq '    if: ${{ !inputs.release_tools_only }}' <<<"$job" || {
    echo "release-tools-only dispatch must skip the $job_name job" >&2
    exit 1
  }
done

echo "release tool manifest tests passed"
