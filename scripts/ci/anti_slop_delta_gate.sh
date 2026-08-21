#!/usr/bin/env bash

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

base_ref="${1:-}"
if [ -z "$base_ref" ]; then
    for candidate in upstream/master origin/master; do
        if git rev-parse --verify --quiet --end-of-options "${candidate}^{commit}" >/dev/null; then
            base_ref="$candidate"
            break
        fi
    done
fi

if [ -z "$base_ref" ]; then
    echo "anti-slop: no master tracking ref found; pass the PR base explicitly" >&2
    exit 2
fi

if ! git rev-parse --verify --quiet --end-of-options "${base_ref}^{commit}" >/dev/null; then
    echo "anti-slop: base is not a commit: ${base_ref}" >&2
    exit 2
fi

base_sha="$(git rev-parse --short=12 --end-of-options "${base_ref}^{commit}")"
echo "anti-slop: base ${base_ref} (${base_sha})"

if cargo run --locked --quiet -p zeroclaw-anti-slop -- \
    --changed-since "$base_ref"; then
    exit 0
else
    status=$?
fi

if [ "$status" -eq 1 ]; then
    exit 1
fi

echo "anti-slop: checker could not run (cargo exit ${status})" >&2
exit 2
