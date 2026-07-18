#!/usr/bin/env bash
set -euo pipefail

# prepare-kernel.sh: build (or reuse) the `zeroclaw` kernel binary and place it
# where the Tauri bundler expects a sidecar, so the desktop installer is
# self-contained — double-click, and the app starts its own daemon from the
# bundled kernel with nothing pre-installed.
#
# The desktop app already prefers a sibling `zeroclaw` binary at runtime
# (apps/tauri/src/daemon.rs::find_zeroclaw_binary), which is exactly where
# Tauri places externalBin sidecars. This script only produces the build-time
# input: apps/tauri/binaries/zeroclaw-<target-triple>[.exe].
#
# Usage:
#   scripts/desktop/prepare-kernel.sh                          # host triple
#   scripts/desktop/prepare-kernel.sh --target aarch64-apple-darwin
#   scripts/desktop/prepare-kernel.sh --target universal-apple-darwin
#       # builds both mac arches and fuses them with lipo
#   scripts/desktop/prepare-kernel.sh --target universal-apple-darwin \
#       --features embedded-web
#
# Environment:
#   ZEROCLAW_KERNEL_PATH   Reuse an existing kernel binary instead of building
#                          (single-target only; ignored for universal). Requested
#                          features must already be present in that binary.
#   CARGO_PROFILE          Cargo profile to build (default: release).
#
# Then bundle with the sidecar overlay:
#   cd apps/tauri && cargo tauri build --config tauri.bundled.conf.json

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT_DIR="$REPO_ROOT/apps/tauri/binaries"
PROFILE="${CARGO_PROFILE:-release}"

TARGET=""
FEATURES=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --target) TARGET="$2"; shift 2 ;;
    --features) FEATURES="$2"; shift 2 ;;
    -h|--help) sed -n '3,29p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

host_triple() {
  rustc -vV | sed -n 's/^host: //p'
}

# Fail fast when a prebuilt kernel's architecture doesn't match the triple it
# would be staged as — a mismatched sidecar produces an installer that can't
# start its own daemon.
check_arch() {
  local path="$1" triple="$2" want=""
  command -v file >/dev/null || return 0
  case "$triple" in
    aarch64-apple-darwin) want="arm64" ;;
    aarch64-*) want="aarch64" ;;
    x86_64-*) want="x86_64" ;;
    *) return 0 ;;
  esac
  if ! file "$path" | grep -q "$want"; then
    echo "prepare-kernel: ERROR: $path is not a $triple binary:" >&2
    file "$path" >&2
    exit 1
  fi
}

# Build the kernel for one triple and echo the path to the built binary.
build_kernel() {
  local triple="$1"
  local exe=""
  [[ "$triple" == *windows* ]] && exe=".exe"
  if [[ -n "${ZEROCLAW_KERNEL_PATH:-}" ]]; then
    echo "prepare-kernel: using prebuilt kernel: $ZEROCLAW_KERNEL_PATH" >&2
    check_arch "$ZEROCLAW_KERNEL_PATH" "$triple"
    echo "$ZEROCLAW_KERNEL_PATH"
    return
  fi
  if [[ -n "$FEATURES" ]]; then
    echo "prepare-kernel: cargo build --profile $PROFILE --bin zeroclaw --target $triple --features $FEATURES" >&2
    (cd "$REPO_ROOT" && cargo build --profile "$PROFILE" --bin zeroclaw --target "$triple" --features "$FEATURES")
  else
    echo "prepare-kernel: cargo build --profile $PROFILE --bin zeroclaw --target $triple" >&2
    (cd "$REPO_ROOT" && cargo build --profile "$PROFILE" --bin zeroclaw --target "$triple")
  fi
  local dir="release"
  [[ "$PROFILE" != "release" ]] && dir="$PROFILE"
  echo "$REPO_ROOT/target/$triple/$dir/zeroclaw$exe"
}

# Strip a copy of the kernel into place; re-sign on macOS (stripping
# invalidates the ad-hoc signature arm64 requires to exec).
place_stripped() {
  local src="$1" dest="$2"
  cp "$src" "$dest"
  case "$(uname -s)" in
    Darwin)
      strip -x "$dest" 2>/dev/null || true
      command -v codesign >/dev/null && codesign --force --sign - "$dest" 2>/dev/null || true
      ;;
    *)
      strip "$dest" 2>/dev/null || true
      ;;
  esac
}

mkdir -p "$OUT_DIR"

if [[ "$TARGET" == "universal-apple-darwin" ]]; then
  # Tauri's universal build expects binaries/zeroclaw-universal-apple-darwin.
  # A single prebuilt kernel can't serve both slices — always build per-arch.
  ZEROCLAW_KERNEL_PATH=""
  ARM_SRC="$(build_kernel aarch64-apple-darwin)"
  X86_SRC="$(build_kernel x86_64-apple-darwin)"
  place_stripped "$ARM_SRC" "$OUT_DIR/zeroclaw-aarch64-apple-darwin"
  place_stripped "$X86_SRC" "$OUT_DIR/zeroclaw-x86_64-apple-darwin"
  lipo -create \
    "$OUT_DIR/zeroclaw-aarch64-apple-darwin" \
    "$OUT_DIR/zeroclaw-x86_64-apple-darwin" \
    -output "$OUT_DIR/zeroclaw-universal-apple-darwin"
  DEST="$OUT_DIR/zeroclaw-universal-apple-darwin"
else
  TARGET="${TARGET:-$(host_triple)}"
  EXE=""
  [[ "$TARGET" == *windows* ]] && EXE=".exe"
  SRC="$(build_kernel "$TARGET")"
  DEST="$OUT_DIR/zeroclaw-$TARGET$EXE"
  place_stripped "$SRC" "$DEST"
fi

echo "prepare-kernel: sidecar ready at ${DEST#"$REPO_ROOT"/}"
ls -lh "$DEST" | awk '{print "prepare-kernel: size " $5}'
