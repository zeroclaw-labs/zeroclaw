#!/usr/bin/env bash
set -euo pipefail

# The sole production archive writer, extracted from Stable's existing Unix
# and Windows packaging steps. Optional companions remain optional here;
# stricter rehearsal completeness belongs to the evidence validator's caller.
target="${1:?target is required}"
artifact="${2:?main artifact name is required}"
ext="${3:?archive extension is required}"
runner_os="${4:?runner OS is required}"

mkdir -p staging/web
cp "target/$target/release/$artifact" staging/
extra=""
if [ "$runner_os" = Windows ]; then
  companions=(zerocode.exe zerorelay.exe)
else
  companions=(zerocode zerorelay)
fi
for b in "${companions[@]}"; do
  if [ -f "target/$target/release/$b" ]; then
    cp "target/$target/release/$b" staging/
    extra="$extra $b"
  fi
done
cp -r web/dist staging/web/dist
cd staging
if [ "$runner_os" = Windows ]; then
  7z a "../zeroclaw-$target.$ext" "$artifact" $extra web/dist
else
  tar czf "../zeroclaw-$target.$ext" "$artifact" $extra web/dist
fi
