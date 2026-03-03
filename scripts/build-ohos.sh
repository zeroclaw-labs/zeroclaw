#!/usr/bin/env bash
# HarmonyOS Build Script for ZeroClaw
#
# This script builds ZeroClaw for HarmonyOS devices using the HarmonyOS NDK.
#
# Prerequisites:
#   - HarmonyOS Command Line Tools installed
#   - OHOS_NDK_HOME environment variable set (or pass via --ndk)
#
# Usage:
#   ./scripts/build-ohos.sh                    # Build for aarch64-linux-ohos (default)
#   ./scripts/build-ohos.sh --target armv7     # Build for armv7-linux-ohos
#   ./scripts/build-ohos.sh --release          # Release build (default)
#   ./scripts/build-ohos.sh --debug            # Debug build
#   OHOS_NDK_HOME=/path/to/ndk ./scripts/build-ohos.sh

set -euo pipefail

# Default values
TARGET="aarch64-linux-ohos"
PROFILE="release"
OHOS_NDK_HOME="${OHOS_NDK_HOME:-}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --target)
            TARGET="$2-linux-ohos"
            shift 2
            ;;
        --release)
            PROFILE="release"
            shift
            ;;
        --debug)
            PROFILE="debug"
            shift
            ;;
        --ndk)
            OHOS_NDK_HOME="$2"
            shift 2
            ;;
        --help|-h)
            echo "Usage: $0 [OPTIONS]"
            echo ""
            echo "Options:"
            echo "  --target <arch>    Target architecture: aarch64 (default) or armv7"
            echo "  --release          Release build (default)"
            echo "  --debug            Debug build"
            echo "  --ndk <path>       Path to HarmonyOS NDK native directory"
            echo "  --help, -h         Show this help message"
            echo ""
            echo "Environment Variables:"
            echo "  OHOS_NDK_HOME      Path to HarmonyOS NDK native directory"
            echo ""
            echo "Example:"
            echo "  OHOS_NDK_HOME=/path/to/ndk ./scripts/build-ohos.sh --target aarch64 --release"
            exit 0
            ;;
        *)
            echo "Unknown option: $1"
            exit 1
            ;;
    esac
done

# Determine NDK path based on OS
if [[ -z "${OHOS_NDK_HOME}" ]]; then
    if [[ -d "/mnt/d/tools/command-line-tools/sdk/default/openharmony/native" ]]; then
        # WSL2 mounted Windows directory
        OHOS_NDK_HOME="/mnt/d/tools/command-line-tools/sdk/default/openharmony/native"
    elif [[ -d "D:/tools/commandline-tools-windows/sdk/default/openharmony/native" ]]; then
        # Windows with forward slashes (Git Bash / MSYS2)
        OHOS_NDK_HOME="D:/tools/commandline-tools-windows/sdk/default/openharmony/native"
    elif [[ -d "/opt/command-line-tools/sdk/default/openharmony/native" ]]; then
        # Linux default location
        OHOS_NDK_HOME="/opt/command-line-tools/sdk/default/openharmony/native"
    elif [[ -d "$HOME/command-line-tools/sdk/default/openharmony/native" ]]; then
        # User home directory
        OHOS_NDK_HOME="$HOME/command-line-tools/sdk/default/openharmony/native"
    else
        echo "Error: OHOS_NDK_HOME not set and NDK not found in default locations."
        echo "Please set OHOS_NDK_HOME to point to the HarmonyOS NDK native directory."
        echo ""
        echo "Checked locations:"
        echo "  - /mnt/d/tools/command-line-tools/sdk/default/openharmony/native (WSL2)"
        echo "  - D:/tools/commandline-tools-windows/sdk/default/openharmony/native (Windows)"
        echo "  - /opt/command-line-tools/sdk/default/openharmony/native (Linux)"
        echo "  - \$HOME/command-line-tools/sdk/default/openharmony/native (User home)"
        echo ""
        echo "Example: export OHOS_NDK_HOME=/path/to/command-line-tools/sdk/default/openharmony/native"
        exit 1
    fi
fi

# Verify NDK exists
if [[ ! -d "${OHOS_NDK_HOME}" ]]; then
    echo "Error: OHOS_NDK_HOME directory does not exist: ${OHOS_NDK_HOME}"
    exit 1
fi

OHOS_LLVM="${OHOS_NDK_HOME}/llvm"
OHOS_SYSROOT="${OHOS_NDK_HOME}/sysroot"

if [[ ! -d "${OHOS_LLVM}" ]]; then
    echo "Error: LLVM directory not found: ${OHOS_LLVM}"
    exit 1
fi

echo "========================================"
echo "Building ZeroClaw for HarmonyOS"
echo "========================================"
echo "Target:     ${TARGET}"
echo "Profile:    ${PROFILE}"
echo "NDK Home:   ${OHOS_NDK_HOME}"
echo "LLVM:       ${OHOS_LLVM}"
echo "Sysroot:    ${OHOS_SYSROOT}"
echo "========================================"

# Convert target to environment variable format (aarch64-linux-ohos -> AARCH64_LINUX_OHOS)
TARGET_UPPER=$(echo "${TARGET}" | tr '[:lower:]-' '[:upper:]_')
TARGET_ENV="CARGO_TARGET_${TARGET_UPPER}_LINKER"

# Set up environment
export PATH="${OHOS_LLVM}/bin:${PATH}"

# Set linker and compiler environment variables
if [[ "${TARGET}" == "aarch64-linux-ohos" ]]; then
    export CARGO_TARGET_AARCH64_LINUX_OHOS_LINKER="${OHOS_LLVM}/bin/clang"
    export CC_aarch64_linux_ohos="${OHOS_LLVM}/bin/clang"
    export CXX_aarch64_linux_ohos="${OHOS_LLVM}/bin/clang++"
    export AR_aarch64_linux_ohos="${OHOS_LLVM}/bin/llvm-ar"
    export CFLAGS_aarch64_linux_ohos="--target=aarch64-linux-ohos --sysroot=${OHOS_SYSROOT} -D__MUSL__"
    export CXXFLAGS_aarch64_linux_ohos="--target=aarch64-linux-ohos --sysroot=${OHOS_SYSROOT} -D__MUSL__"
    export LDFLAGS_aarch64_linux_ohos="--target=aarch64-linux-ohos --sysroot=${OHOS_SYSROOT}"
    # CMake toolchain for native dependencies (like aws-lc-sys)
    export CMAKE_TOOLCHAIN_FILE_aarch64_linux_ohos="${OHOS_NDK_HOME}/build/cmake/ohos.toolchain.cmake"
    # Use Ninja generator instead of Visual Studio for cross-compilation
    export CMAKE_GENERATOR_aarch64_linux_ohos="Ninja"
elif [[ "${TARGET}" == "armv7-linux-ohos" ]]; then
    export CARGO_TARGET_ARMV7_LINUX_OHOS_LINKER="${OHOS_LLVM}/bin/clang"
    export CC_armv7_linux_ohos="${OHOS_LLVM}/bin/clang"
    export CXX_armv7_linux_ohos="${OHOS_LLVM}/bin/clang++"
    export AR_armv7_linux_ohos="${OHOS_LLVM}/bin/llvm-ar"
    export CFLAGS_armv7_linux_ohos="--target=armv7-linux-ohos --sysroot=${OHOS_SYSROOT} -D__MUSL__"
    export CXXFLAGS_armv7_linux_ohos="--target=armv7-linux-ohos --sysroot=${OHOS_SYSROOT} -D__MUSL__"
    export LDFLAGS_armv7_linux_ohos="--target=armv7-linux-ohos --sysroot=${OHOS_SYSROOT}"
    # CMake toolchain for native dependencies
    export CMAKE_TOOLCHAIN_FILE_armv7_linux_ohos="${OHOS_NDK_HOME}/build/cmake/ohos.toolchain.cmake"
    # Use Ninja generator instead of Visual Studio for cross-compilation
    export CMAKE_GENERATOR_armv7_linux_ohos="Ninja"
else
    echo "Error: Unsupported target: ${TARGET}"
    exit 1
fi

# Change to repository root
cd "${REPO_ROOT}"

# Build with custom target specification
TARGET_JSON="${REPO_ROOT}/.cargo/ohos-targets/${TARGET}.json"

if [[ ! -f "${TARGET_JSON}" ]]; then
    echo "Error: Target specification file not found: ${TARGET_JSON}"
    exit 1
fi

echo ""
echo "Running cargo build..."
echo ""

# Build command - use --target with path to custom target JSON
# Use nightly toolchain with build-std for custom targets
BUILD_ARGS=(
    +nightly
    build
    -Zbuild-std=std,panic_abort
    -Zjson-target-spec
    --target "${TARGET_JSON}"
)

if [[ "${PROFILE}" == "release" ]]; then
    BUILD_ARGS+=("--release")
fi

cargo "${BUILD_ARGS[@]}"

# Verify output
OUTPUT_BINARY="target/${TARGET}/${PROFILE}/zeroclaw"
if [[ -f "${OUTPUT_BINARY}" ]]; then
    echo ""
    echo "========================================"
    echo "Build successful!"
    echo "Output: ${OUTPUT_BINARY}"
    echo "========================================"

    # Show file info (if file command available)
    if command -v file &> /dev/null; then
        file "${OUTPUT_BINARY}"
    fi

    # Show binary size
    ls -lh "${OUTPUT_BINARY}"
else
    echo "Error: Build output not found: ${OUTPUT_BINARY}"
    exit 1
fi
