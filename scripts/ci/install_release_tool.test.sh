#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
installer="${root_dir}/scripts/ci/install_release_tool.sh"

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

echo "release tool manifest tests passed"
