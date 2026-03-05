#!/usr/bin/env bash
set -euo pipefail

pick_compiler() {
    if command -v cc >/dev/null 2>&1; then
        command -v cc
    elif command -v gcc >/dev/null 2>&1; then
        command -v gcc
    elif command -v clang >/dev/null 2>&1; then
        command -v clang
    else
        return 1
    fi
}

pick_cpp_compiler() {
    if command -v c++ >/dev/null 2>&1; then
        command -v c++
    elif command -v g++ >/dev/null 2>&1; then
        command -v g++
    elif command -v clang++ >/dev/null 2>&1; then
        command -v clang++
    else
        return 1
    fi
}

CC_PATH="$(pick_compiler || true)"
if [ -z "${CC_PATH}" ]; then
    echo "No C compiler found. Run scripts/ci/ensure_c_toolchain.sh first." >&2
    exit 1
fi

CXX_PATH="$(pick_cpp_compiler || true)"
if [ -z "${CXX_PATH}" ]; then
    echo "No C++ compiler found. Run scripts/ci/ensure_c_toolchain.sh first." >&2
    exit 1
fi

if [ -n "${GITHUB_ENV:-}" ] && [ -w "${GITHUB_ENV}" ]; then
    printf 'CC=%s\n' "${CC_PATH}" >>"${GITHUB_ENV}"
    printf 'CXX=%s\n' "${CXX_PATH}" >>"${GITHUB_ENV}"
fi

echo "Using C compiler: ${CC_PATH}"
echo "Using C++ compiler: ${CXX_PATH}"
"${CC_PATH}" --version | head -n 1 || true
"${CXX_PATH}" --version | head -n 1 || true
