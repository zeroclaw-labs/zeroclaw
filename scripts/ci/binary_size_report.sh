#!/usr/bin/env bash

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
case "${1:-}" in
    measure|compare)
        exec python3 "${script_dir}/binary_size_report.py" "$@"
        ;;
    *)
        exec python3 "${script_dir}/binary_size_report.py" measure "$@"
        ;;
esac
