#!/usr/bin/env bash
# scripts/android/build-ndk.sh — cross-compile zeroclaw-android-bridge
# (and optionally zeroclaw itself) to Android NDK targets, then place
# the resulting `.so` files into the Android Gradle project's `jniLibs`
# tree so an `assembleDebug` / `assembleRelease` packs them into the APK.
#
# This closes the E7 follow-up from `docs/audit-2026-05-03.md` — the
# bridge crate at `clients/android-bridge/` is `crate-type = ["cdylib"]`
# but no script existed to actually cross-compile it for the four
# Android ABIs an APK ships. Without that, the bridge fails at runtime
# with `dlopen` errors.
#
# What this script does:
#   1. Parses options:
#        --target <abi>   one of arm64, arm32, x86_64, x86 (default: arm64)
#        --release        cross-compile with --release
#        --all-abis       compile for all four ABIs (release implied)
#        --skip-rustup    don't auto-add the Rust target via rustup
#   2. Verifies prerequisites (rustup target, Android NDK paths).
#   3. Sets the NDK linker env (CC/AR/RANLIB/CARGO_TARGET_*_LINKER).
#   4. Runs `cargo build` for the bridge crate against the chosen target.
#   5. Copies the resulting `libzeroclaw_android.so` into
#        clients/android/app/src/main/jniLibs/<abi>/
#      so Android Studio / `gradlew assembleDebug` packs it into the APK.
#
# Prerequisites checked at runtime:
#   - $ANDROID_NDK_HOME or $NDK_HOME (or $ANDROID_HOME/ndk/<latest>)
#   - rustup with the target installed (auto-installed unless --skip-rustup)
#   - The bridge sources at clients/android-bridge/
#
# Usage examples:
#   bash scripts/android/build-ndk.sh                         # arm64 debug
#   bash scripts/android/build-ndk.sh --release               # arm64 release
#   bash scripts/android/build-ndk.sh --target x86_64         # for emulator
#   bash scripts/android/build-ndk.sh --all-abis              # ship-ready
#
# Per the audit, this is opt-in tooling: nothing in CI runs it
# automatically yet. Add a CI matrix job that calls --all-abis once
# the team is comfortable with the toolchain footprint.

set -euo pipefail

# ── ABI table ─────────────────────────────────────────────────────
# Maps the Android ABI name (used in jniLibs/) to the matching Rust
# target triple. The clang/clang++ name uses the API level suffix
# (we pin to API 24 = Android 7.0, our minSdk floor).
declare -A ABI_TO_RUST_TARGET=(
    [arm64]="aarch64-linux-android"
    [arm32]="armv7-linux-androideabi"
    [x86_64]="x86_64-linux-android"
    [x86]="i686-linux-android"
)

declare -A RUST_TARGET_TO_NDK_TRIPLE=(
    [aarch64-linux-android]="aarch64-linux-android"
    [armv7-linux-androideabi]="armv7a-linux-androideabi"  # NDK uses "armv7a-" prefix
    [x86_64-linux-android]="x86_64-linux-android"
    [i686-linux-android]="i686-linux-android"
)

NDK_API_LEVEL=24

# ── Args ──────────────────────────────────────────────────────────
ABIS=()
BUILD_MODE="debug"
SKIP_RUSTUP=0
ALL_ABIS=0

usage() {
    cat <<'EOF'
Usage: bash scripts/android/build-ndk.sh [options]

Options:
  --target <abi>      ABI to build for: arm64 | arm32 | x86_64 | x86
                      (default: arm64; can repeat --target for multiple)
  --all-abis          Build for all four ABIs (implies --release)
  --release           Use cargo --release (default: debug)
  --skip-rustup       Do not auto-install the Rust target
  -h, --help          Show this help

Required environment:
  ANDROID_NDK_HOME or NDK_HOME (or ANDROID_HOME/ndk/<version>)

Output:
  clients/android/app/src/main/jniLibs/<abi>/libzeroclaw_android.so
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --target)
            ABIS+=("$2")
            shift 2
            ;;
        --all-abis)
            ALL_ABIS=1
            shift
            ;;
        --release)
            BUILD_MODE="release"
            shift
            ;;
        --skip-rustup)
            SKIP_RUSTUP=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

if [[ "$ALL_ABIS" -eq 1 ]]; then
    ABIS=(arm64 arm32 x86_64 x86)
    BUILD_MODE="release"
elif [[ "${#ABIS[@]}" -eq 0 ]]; then
    ABIS=(arm64)
fi

# ── Resolve repo paths ────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BRIDGE_DIR="$REPO_ROOT/clients/android-bridge"
JNI_LIBS_DIR="$REPO_ROOT/clients/android/app/src/main/jniLibs"

if [[ ! -d "$BRIDGE_DIR" ]]; then
    echo "FATAL: bridge crate not found at $BRIDGE_DIR" >&2
    exit 1
fi

# ── Resolve NDK ───────────────────────────────────────────────────
NDK="${ANDROID_NDK_HOME:-${NDK_HOME:-}}"
if [[ -z "$NDK" && -n "${ANDROID_HOME:-}" && -d "$ANDROID_HOME/ndk" ]]; then
    # Pick the latest installed NDK version (lexicographic sort works
    # for the standard "27.0.12077973"-style version directories).
    NDK="$ANDROID_HOME/ndk/$(ls "$ANDROID_HOME/ndk" | sort -V | tail -n 1)"
fi

if [[ -z "$NDK" || ! -d "$NDK" ]]; then
    cat >&2 <<EOF
FATAL: Android NDK not found.
Set one of:
  ANDROID_NDK_HOME=/path/to/ndk
  NDK_HOME=/path/to/ndk
  ANDROID_HOME=/path/to/sdk      (script will pick \$ANDROID_HOME/ndk/<latest>)

Install via Android Studio: SDK Manager → SDK Tools → "NDK (Side by side)".
EOF
    exit 1
fi

# Locate the host prebuilt toolchain. NDK 23+ uses
# `<ndk>/toolchains/llvm/prebuilt/<host>/`.
HOST_TAG=""
case "$(uname -s)" in
    Linux*)   HOST_TAG="linux-x86_64" ;;
    Darwin*)  HOST_TAG="darwin-x86_64" ;;
    MINGW*|MSYS*|CYGWIN*) HOST_TAG="windows-x86_64" ;;
    *)
        echo "FATAL: unsupported host OS: $(uname -s)" >&2
        exit 1
        ;;
esac

TOOLCHAIN="$NDK/toolchains/llvm/prebuilt/$HOST_TAG"
if [[ ! -d "$TOOLCHAIN" ]]; then
    echo "FATAL: NDK toolchain not found at $TOOLCHAIN" >&2
    echo "Available host tags: $(ls "$NDK/toolchains/llvm/prebuilt/" 2>/dev/null || echo none)" >&2
    exit 1
fi

echo "==> Using NDK: $NDK ($HOST_TAG)"
echo "==> Build mode: $BUILD_MODE"
echo "==> ABIs: ${ABIS[*]}"
echo

# ── Build each ABI ────────────────────────────────────────────────
for ABI in "${ABIS[@]}"; do
    if [[ -z "${ABI_TO_RUST_TARGET[$ABI]:-}" ]]; then
        echo "FATAL: unknown ABI '$ABI'. Supported: arm64 | arm32 | x86_64 | x86" >&2
        exit 2
    fi

    RUST_TARGET="${ABI_TO_RUST_TARGET[$ABI]}"
    NDK_TRIPLE="${RUST_TARGET_TO_NDK_TRIPLE[$RUST_TARGET]}"
    CC_BIN="$TOOLCHAIN/bin/${NDK_TRIPLE}${NDK_API_LEVEL}-clang"
    AR_BIN="$TOOLCHAIN/bin/llvm-ar"
    RANLIB_BIN="$TOOLCHAIN/bin/llvm-ranlib"

    if [[ ! -x "$CC_BIN" ]]; then
        echo "FATAL: NDK C compiler not found: $CC_BIN" >&2
        echo "Hint: ensure the NDK is at least r23 and supports API $NDK_API_LEVEL" >&2
        exit 1
    fi

    if [[ "$SKIP_RUSTUP" -eq 0 ]]; then
        if ! rustup target list --installed 2>/dev/null | grep -q "^$RUST_TARGET\$"; then
            echo "==> Installing Rust target: $RUST_TARGET"
            rustup target add "$RUST_TARGET"
        fi
    fi

    # Cargo env vars for cross-compilation. Cargo's per-target linker
    # variable name is uppercase + underscore form of the triple.
    CARGO_LINKER_VAR="CARGO_TARGET_$(echo "$RUST_TARGET" | tr 'a-z-' 'A-Z_')_LINKER"
    CC_VAR="CC_${RUST_TARGET//-/_}"
    AR_VAR="AR_${RUST_TARGET//-/_}"
    RANLIB_VAR="RANLIB_${RUST_TARGET//-/_}"

    echo
    echo "==> Building $ABI ($RUST_TARGET)"

    CARGO_FLAGS=()
    if [[ "$BUILD_MODE" == "release" ]]; then
        CARGO_FLAGS=(--release)
    fi

    (
        cd "$BRIDGE_DIR"
        env "$CARGO_LINKER_VAR=$CC_BIN" \
            "$CC_VAR=$CC_BIN" \
            "$AR_VAR=$AR_BIN" \
            "$RANLIB_VAR=$RANLIB_BIN" \
            cargo build --target "$RUST_TARGET" "${CARGO_FLAGS[@]}"
    )

    SO_PATH="$BRIDGE_DIR/target/$RUST_TARGET/$BUILD_MODE/libzeroclaw_android.so"
    if [[ ! -f "$SO_PATH" ]]; then
        echo "FATAL: cargo did not produce $SO_PATH" >&2
        exit 1
    fi

    DEST_DIR="$JNI_LIBS_DIR/$ABI"
    mkdir -p "$DEST_DIR"
    cp "$SO_PATH" "$DEST_DIR/libzeroclaw_android.so"
    echo "==> Packed $ABI → $DEST_DIR/libzeroclaw_android.so ($(du -h "$SO_PATH" | cut -f1))"
done

echo
echo "==> Done. Run \`./gradlew assembleDebug\` (or assembleRelease) inside"
echo "    clients/android/ to produce an APK that includes the native libs."
