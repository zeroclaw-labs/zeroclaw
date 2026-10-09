#!/usr/bin/env bash
# Build once locally, or exercise the exact binaries already built by CI.
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"
suite=core
bin_dir=
artifacts_dir=
while (($#)); do
  case "$1" in
    --suite|--bin-dir|--artifacts-dir)
      if (($# < 2)); then echo "missing value for $1" >&2; exit 2; fi
      case "$1" in
        --suite) suite="$2" ;;
        --bin-dir) bin_dir="$2" ;;
        --artifacts-dir) artifacts_dir="$2" ;;
      esac
      shift 2 ;;
    --help)
      echo 'Usage: runtime_acceptance.sh [--suite core|full] [--bin-dir DIR] [--artifacts-dir DIR]'
      exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$suite" in core|full) ;; *) echo "suite must be core or full" >&2; exit 2 ;; esac
if [[ "$(uname -s)" != Linux ]]; then echo 'Application acceptance requires Linux.' >&2; exit 2; fi
for dependency in python3 tmux openssl; do
  command -v "$dependency" >/dev/null || { echo "missing dependency: $dependency" >&2; exit 2; }
done
if [[ -z "$bin_dir" ]]; then
  target="$(rustc -vV | sed -n 's/^host: //p')"
  target_dir="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
  mkdir -p web/dist
  touch web/dist/.gitkeep
  cargo build --locked --profile ci --target "$target" -p zeroclaw -p zerocode --bins
  bin_dir="$target_dir/$target/ci"
fi
if [[ -z "$artifacts_dir" ]]; then artifacts_dir="$(mktemp -d "${TMPDIR:-/tmp}/zeroclaw-acceptance.XXXXXX")"; fi
echo "Application acceptance artifacts: $artifacts_dir"
exec python3 tests/system/runtime_acceptance/run.py --suite "$suite" --bin-dir "$bin_dir" --artifacts-dir "$artifacts_dir"
