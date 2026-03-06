#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" >/dev/null 2>&1 && pwd || pwd)"
WEB_DIR="$ROOT_DIR/web"

build_frontend=true
build_rust=true
release=false

usage() {
    cat <<'EOF'
Usage: ./build.sh [options]

Build helper for ZeroClaw developers.

By default this script:
  1. Rebuilds the embedded web dashboard in `web/dist`
  2. Builds the Rust workspace

Options:
  --no-frontend   Skip the frontend build step
  --frontend-only Build only the frontend bundle
  --release       Run `cargo build --release`
  -h, --help      Show this help text
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-frontend)
            build_frontend=false
            ;;
        --frontend-only)
            build_rust=false
            ;;
        --release)
            release=true
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            echo >&2
            usage >&2
            exit 1
            ;;
    esac
    shift
done

if [[ "$build_frontend" == true ]]; then
    if ! command -v npm >/dev/null 2>&1; then
        echo "npm is required to build the embedded web dashboard" >&2
        exit 1
    fi

    echo "==> Building frontend assets in web/dist"
    pushd "$WEB_DIR" >/dev/null
    if [[ ! -d node_modules ]]; then
        npm install
    fi
    npm run build
    popd >/dev/null
fi

if [[ "$build_rust" == true ]]; then
    echo "==> Building Rust workspace"
    pushd "$ROOT_DIR" >/dev/null
    if [[ "$release" == true ]]; then
        cargo build --release
    else
        cargo build
    fi
    popd >/dev/null
fi

if [[ "$build_frontend" == false && "$build_rust" == false ]]; then
    echo "Nothing to do. Try ./build.sh or ./build.sh --frontend-only" >&2
    exit 1
fi