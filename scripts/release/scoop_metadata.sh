#!/usr/bin/env bash
set -euo pipefail

manifest_path="${1:-}"
version="${2:-}"

if [[ -z "$manifest_path" || ! -f "$manifest_path" ]]; then
  echo "Scoop manifest path must name an existing file." >&2
  exit 1
fi
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Scoop release version must use X.Y.Z format." >&2
  exit 1
fi

if ! canonical_url="$(
  jq -er '
    .autoupdate.architecture["64bit"].url
    | strings
    | select(
        length > 0
        and (contains("\n") | not)
        and (contains("\r") | not)
      )
  ' "$manifest_path"
)"; then
  echo "Scoop manifest must define a non-empty, single-line 64-bit autoupdate URL template." >&2
  exit 1
fi
if [[ "$canonical_url" != https://* ]]; then
  echo "Scoop 64-bit autoupdate URL template must use HTTPS." >&2
  exit 1
fi
if [[ "$canonical_url" != *'$version'* ]]; then
  echo 'Scoop 64-bit autoupdate URL template must contain $version.' >&2
  exit 1
fi

# Bash parameter expansion performs literal placeholder substitution. Do not
# evaluate the manifest-owned template as shell code.
zip_url="${canonical_url//\$version/$version}"
asset_name="${zip_url##*/}"
if [[ -z "$asset_name" || "$asset_name" == "$zip_url" ]]; then
  echo "Scoop 64-bit autoupdate URL template must end with an asset filename." >&2
  exit 1
fi
sums_url="${zip_url%/*}/SHA256SUMS"

jq -cn \
  --arg zip_url "$zip_url" \
  --arg asset_name "$asset_name" \
  --arg sums_url "$sums_url" \
  '{zip_url: $zip_url, asset_name: $asset_name, sums_url: $sums_url}'
