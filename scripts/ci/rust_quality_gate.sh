#!/usr/bin/env bash

set -euo pipefail

MODE="correctness"
if [ "${1:-}" = "--strict" ]; then
    MODE="strict"
fi

ensure_toolchain_bin_on_path() {
    local toolchain_bin=""

    if [ -n "${CARGO:-}" ]; then
        toolchain_bin="$(dirname "${CARGO}")"
    elif [ -n "${RUSTC:-}" ]; then
        toolchain_bin="$(dirname "${RUSTC}")"
    fi

    if [ -z "$toolchain_bin" ] || [ ! -d "$toolchain_bin" ]; then
        return 0
    fi

    case ":$PATH:" in
        *":${toolchain_bin}:"*) ;;
        *) export PATH="${toolchain_bin}:$PATH" ;;
    esac
}

ensure_toolchain_bin_on_path

run_cargo_tool() {
    local subcommand="$1"
    shift

    if [ -n "${RUSTUP_TOOLCHAIN:-}" ] && command -v rustup >/dev/null 2>&1; then
        rustup run "${RUSTUP_TOOLCHAIN}" cargo "$subcommand" "$@"
    else
        cargo "$subcommand" "$@"
    fi
}

ensure_cargo_subcommand_component() {
    local subcommand="$1"
    local toolchain="${RUSTUP_TOOLCHAIN:-}"
    local component="$subcommand"

    if [ "$subcommand" = "fmt" ]; then
        component="rustfmt"
    fi

    if run_cargo_tool "$subcommand" --version >/dev/null 2>&1; then
        return 0
    fi

    if ! command -v rustup >/dev/null 2>&1; then
        echo "::error::cargo ${subcommand} is unavailable and rustup is not installed."
        return 1
    fi

    echo "==> rust quality: installing missing rust component '${component}'"
    if [ -n "$toolchain" ]; then
        rustup component add "$component" --toolchain "$toolchain"
    else
        rustup component add "$component"
    fi
}

ensure_cargo_subcommand_component "fmt"
echo "==> rust quality: cargo fmt --all -- --check"
run_cargo_tool fmt --all -- --check

ensure_cargo_subcommand_component "clippy"
if [ "$MODE" = "strict" ]; then
    echo "==> rust quality: cargo clippy --locked --all-targets -- -D warnings"
    run_cargo_tool clippy --locked --all-targets -- -D warnings
else
    echo "==> rust quality: cargo clippy --locked --all-targets -- -D clippy::correctness"
    run_cargo_tool clippy --locked --all-targets -- -D clippy::correctness
fi
