#!/usr/bin/env bash
set -euo pipefail

# The sole production archive writer, extracted from Stable's existing Unix
# and Windows packaging steps. Optional companions remain optional here;
# stricter rehearsal completeness belongs to the evidence validator's caller.
target="${1:?target is required}"
artifact="${2:?main artifact name is required}"
ext="${3:?archive extension is required}"
runner_os="${4:?runner OS is required}"

# Isolate each invocation so removed inputs cannot survive a repack. Build a
# new archive too: `7z a` would otherwise retain members from an older zip.
archive_output="$PWD/zeroclaw-$target.$ext"
package_tmp="$(mktemp -d "$PWD/.zeroclaw-package.XXXXXX")"
trap 'rm -rf -- "$package_tmp"' EXIT
mkdir -p "$package_tmp/staging/web"
cp "target/$target/release/$artifact" "$package_tmp/staging/"
extra=""
if [ "$runner_os" = Windows ]; then
  companions=(zerocode.exe zerorelay.exe)
else
  companions=(zerocode zerorelay)
fi
for b in "${companions[@]}"; do
  if [ -f "target/$target/release/$b" ]; then
    cp "target/$target/release/$b" "$package_tmp/staging/"
    extra="$extra $b"
  fi
done
cp -r web/dist "$package_tmp/staging/web/dist"
cd "$package_tmp/staging"
if [ "$runner_os" = Windows ]; then
  7z a "../archive.$ext" "$artifact" $extra web/dist
else
  tar czf "../archive.$ext" "$artifact" $extra web/dist
fi
mv "../archive.$ext" "$archive_output"
