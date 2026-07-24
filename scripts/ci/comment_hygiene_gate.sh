#!/usr/bin/env bash

# Comment hygiene gate for Rust, TOML, shell, Python, and Nix sources.
# Rejects issue/PR refs, review-process leakage, dated notes, and mechanical
# sweep artifacts in comments. It does not judge architectural vocabulary.
# Optional args: paths to scan (defaults to the supported files in the repo).

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

if ! command -v rg >/dev/null 2>&1; then
    echo "FATAL: ripgrep (rg) is not installed; the comment hygiene gate requires it." >&2
    exit 2
fi

scan_roots=("${@:-.}")
script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Paths exempt from the gate. Keep this list short; additions are reviewed via
# the diff on this script.
skip_paths=(
    "scripts/check-pr-title.test.sh"
    "scripts/ci/comment_hygiene_gate.py"
    "scripts/ci/comment_hygiene_gate.sh"
    "scripts/ci/comment_hygiene_gate.test.sh"
    ".cargo/audit.toml"
    "deny.toml"
)

globs=(--hidden -g '*.rs' -g '*.toml' -g '*.sh' -g '*.py' -g '*.nix'
    -g '!.git/' -g '!target/' -g '!web/' -g '!docs/book/' -g '!.claude/')
for path in "${skip_paths[@]}"; do
    globs+=(-g "!${path}")
done

file_list="$(mktemp)"
trap 'rm -f "$file_list"' EXIT

if ! rg --files -0 "${globs[@]}" -- "${scan_roots[@]}" >"$file_list"; then
    echo "FATAL: ripgrep failed while enumerating comment-hygiene inputs." >&2
    exit 2
fi

set +e
python3 "${script_dir}/comment_hygiene_gate.py" "$file_list"
status=$?
set -e

case "$status" in
    0|1) exit "$status" ;;
    *)
        echo "FATAL: comment filter failed (exit ${status})." >&2
        exit 2
        ;;
esac
