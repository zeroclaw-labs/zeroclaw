#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
case "${1:-}" in
    capture|normalize|compare)
        exec python3 "${script_dir}/dependency_footprint.py" "$@"
        ;;
    *)
        exec python3 "${script_dir}/dependency_footprint.py" capture "$@"
        ;;
esac
