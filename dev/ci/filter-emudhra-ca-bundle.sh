#!/bin/sh
# Remove the incident roots from a PEM bundle. Fingerprints:
# https://emudhra.we-have-a-problem.com/roots.json
# A PEM bundle cannot distrust the compromised intermediates by key, so the
# roots they chain to are removed. That includes Sectigo's AAA Certificate
# Services, which cross-signed them. Debian's fix (DSA-6572-1, DLA-4845-1)
# removes the same AAA and emSign ECC G3 roots. Mozilla had already dropped
# AAA, so the pinned StageX input lacks it; the entry keeps later pins aligned.
# The caller supplies input and output paths; no host trust store is modified.
set -eu

input=$1
output=$2
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT HUP INT TERM

awk -v dir="$scratch" '
    /-----BEGIN CERTIFICATE-----/ { n++; active=1 }
    active { print > (dir "/cert-" n ".pem") }
    /-----END CERTIFICATE-----/ { close(dir "/cert-" n ".pem"); active=0 }
    END { if (active) exit 1 }
' "$input"

: > "$scratch/bundle"
for cert in "$scratch"/cert-*.pem; do
    sed '/-----/d' "$cert" > "$scratch/base64"
    base64 -d < "$scratch/base64" > "$scratch/der"
    digest=$(sha256sum "$scratch/der")
    digest=${digest%% *}
    case "$digest" in
        7dd78d5f4f13459a83dff9abbba62edbaf6f2d102bf257fd712f4d9f2746ed8d|\
        f2edaaff3d9b701fb4d14c9f285f8c0bad90fc3bcc1fa07d47f4718edaba34a5|\
        d7a7a0fb5d7e2731d771e9484ebcdef71d5f0c3e0a2948782bc83ee0ea699ef4|\
        86a1ecba089c4a8d3bbe2734c612ba341d813e043cf9e8a862cd5c57a36bbe6b)
            ;;
        *) cat "$cert" >> "$scratch/bundle" ;;
    esac
done
test -s "$scratch/bundle"
cp "$scratch/bundle" "$output"
