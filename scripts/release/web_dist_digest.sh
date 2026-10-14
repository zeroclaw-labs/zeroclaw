#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

# web_dist_digest.sh: print one SHA-256 digest identifying a built dashboard tree.
#
# Usage: scripts/release/web_dist_digest.sh [web/dist]
#
# The crates.io preflight records this digest for the bundle it packaged and
# verified; the publish job recomputes it before uploading so the published
# tarball carries exactly those bytes. The digest covers every regular file's
# relative path and content, hidden files included, and ignores order and
# timestamps. Anything other than a directory or a regular file is rejected:
# an artifact round trip does not preserve symlinks, so one here would make the
# two jobs disagree about what the tree contains.

dir="${1:-web/dist}"
if [[ ! -d "$dir" ]]; then
  echo "error: $dir is not a directory." >&2
  exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
  sha256() { sha256sum "$@"; }
elif command -v shasum >/dev/null 2>&1; then
  sha256() { shasum -a 256 "$@"; }
else
  echo "error: sha256sum or shasum is required." >&2
  exit 1
fi

cd "$dir"

unexpected="$(find . ! -type d ! -type f -print)"
if [[ -n "$unexpected" ]]; then
  echo "error: $dir contains entries that are neither files nor directories:" >&2
  printf '%s\n' "$unexpected" >&2
  exit 1
fi

manifest="$(mktemp)"
trap 'rm -f "$manifest"' EXIT
count=0
while IFS= read -r -d '' path; do
  if [[ "$path" == *$'\n'* ]]; then
    echo "error: $dir contains a file name with a newline; refusing to digest it." >&2
    exit 1
  fi
  digest="$(sha256 "$path" | awk '{print $1}')"
  printf '%s  %s\n' "$digest" "${path#./}" >>"$manifest"
  count=$((count + 1))
done < <(find . -type f -print0 | sort -z)

if [[ $count -eq 0 ]]; then
  echo "error: $dir contains no files." >&2
  exit 1
fi

sha256 "$manifest" | awk '{print $1}'
