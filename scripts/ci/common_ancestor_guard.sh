#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 2 ]]; then
  echo "usage: $0 <base-ref> <target-ref>" >&2
  exit 2
fi

base_ref="$1"
target_ref="$2"

if ! git rev-parse --verify --quiet "${base_ref}^{commit}" >/dev/null; then
  echo "::error::Base ref '$base_ref' is not an available commit." >&2
  exit 1
fi

if ! git rev-parse --verify --quiet "${target_ref}^{commit}" >/dev/null; then
  echo "::error::Target ref '$target_ref' is not an available commit." >&2
  exit 1
fi

if ! common_ancestor="$(git merge-base "$base_ref" "$target_ref")" || [[ -z "$common_ancestor" ]]; then
  echo "::error::This PR has no common ancestor with $base_ref; refusing a grafted second root." >&2
  exit 1
fi

echo "Common ancestor with $base_ref: $common_ancestor"
