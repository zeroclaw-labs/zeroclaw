#!/usr/bin/env bash

set -euo pipefail

# compare reads legacy reports too, but unknown build inputs retain only raw
# sizes/hashes. New reports need Cargo artifact feature evidence and an inspected
# configuration-free build before a delta can be called comparable.

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
case "${1:-}" in
    measure|compare)
        exec python3 "${script_dir}/binary_size_report.py" "$@"
        ;;
    *)
        exec python3 "${script_dir}/binary_size_report.py" measure "$@"
        ;;
esac
