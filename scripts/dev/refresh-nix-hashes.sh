#!/usr/bin/env bash
# refresh-nix-hashes.sh — (re)compute git-dep NAR hashes for nix/hashes.json.
#
# Parses Cargo.lock directly (git deps are behind feature flags; cargo
# metadata won't resolve them), groups packages by git URL+rev, calls
# nix-prefetch-git once per group, and writes the flat outputHashes map
# to nix/hashes.json.
#
# Usage:
#   scripts/dev/refresh-nix-hashes.sh
#   cargo generate installers flake   # regenerate the sentinel zone
#
# Prerequisites: cargo, jq, python3 (3.11+), nix-prefetch-git

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
HASHES_FILE="$REPO_ROOT/nix/hashes.json"
LOCKFILE="$REPO_ROOT/Cargo.lock"

# ── Prerequisites ────────────────────────────────────────────────────
for cmd in cargo jq nix-prefetch-git; do
  command -v "$cmd" >/dev/null 2>&1 || { echo "error: $cmd not found"; exit 1; }
done

PYTHON=$(command -v python3 || command -v python || echo "")
if [[ -z "$PYTHON" ]]; then
  echo "error: python3 not found (needed for Cargo.lock TOML parsing)"
  exit 1
fi

if [[ ! -f "$LOCKFILE" ]]; then
  echo "error: $LOCKFILE not found"
  echo "  Run cargo update or cargo build first."
  exit 1
fi

# ── Parse git deps from Cargo.lock ───────────────────────────────────
echo "Reading git dependencies from Cargo.lock..."

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Python script reads Cargo.lock, outputs one JSON record per git dep:
#   { key: "crate-version", url: "https://...", rev: "<sha>" }
export LOCKFILE_PATH="$LOCKFILE"
"$PYTHON" -c '
import json, re, os

for mod in ("tomllib", "tomli", "toml"):
    try:
        tom = __import__(mod)
        break
    except ImportError:
        continue
else:
    print("error: no TOML parser found (install tomli or use Python 3.11+)", file=sys.stderr)
    exit(1)

with open(os.environ["LOCKFILE_PATH"], "rb") as f:
    data = tom.load(f)

entries = []
for pkg in data.get("package", []):
    src = pkg.get("source", "") or ""
    if not src.startswith("git+"):
        continue
    m = re.match(r"^git\+([^?]+)\?rev=([^#]+)", src)
    if not m:
        continue
    url, rev = m.groups()
    entries.append({
        "key": "%s-%s" % (pkg["name"], pkg["version"]),
        "url": url,
        "rev": rev,
    })

print(json.dumps(entries))
' > "$WORK/deps.json" 2>&1

PKG_COUNT=$(jq length "$WORK/deps.json")
echo "  Found $PKG_COUNT git-sourced package(s)."

if [[ "$PKG_COUNT" -eq 0 ]]; then
  echo "  Nothing to do — writing empty hashes file."
  echo "{}" > "$HASHES_FILE"
  exit 0
fi

# Group by url|rev using jq. Write a file per group so we avoid bash
# associative-array headaches.
jq -r '
  group_by(.url + "|" + .rev)
  | .[]
  | select(length > 0)
  | "\(.[0].url)|\(.[0].rev)\t\([.[].key] | join(","))"
' "$WORK/deps.json" > "$WORK/groups.txt"

GROUP_COUNT=$(wc -l < "$WORK/groups.txt")
echo "  Across $GROUP_COUNT unique URL+rev group(s)."

# ── Prefetch hashes ─────────────────────────────────────────────────
echo ""
declare -A HASH_MAP

while IFS=$'\t' read -r group crates_str; do
  url="${group%%|*}"
  rev="${group##*|}"

  echo "  nix-prefetch-git $url --rev $rev"
  echo "    crates: $crates_str"

  PREFETCH_OUT=$(nix-prefetch-git "$url" --rev "$rev" 2>/dev/null)
  HASH=$(echo "$PREFETCH_OUT" | jq -r '.hash // empty')

  if [[ -z "$HASH" ]]; then
    echo "    error: nix-prefetch-git produced no hash; aborting"
    exit 1
  fi

  echo "    hash:   $HASH"
  HASH_MAP["$group"]="$HASH"
done < "$WORK/groups.txt"

# ── Build and write nix/hashes.json ──────────────────────────────────
echo ""
echo "Writing $HASHES_FILE ..."

# Build a lookup: key -> group
declare -A KEY_GROUP
while IFS=$'\t' read -r group keys_str; do
  IFS=',' read -ra keys <<< "$keys_str"
  for key in "${keys[@]}"; do
    KEY_GROUP["$key"]="$group"
  done
done < "$WORK/groups.txt"

# Sort keys for deterministic output
ALL_KEYS=$(jq -r 'sort_by(.key) | .[].key' "$WORK/deps.json")

ENTRIES=""
SEP=""
while IFS= read -r key; do
  group="${KEY_GROUP[$key]}"
  hash="${HASH_MAP[$group]}"
  ENTRIES+="${SEP}  \"$key\": \"$hash\""
  SEP=",\n"
done < <(echo "$ALL_KEYS")

{
  echo "{"
  echo -e "$ENTRIES"
  echo "}"
} > "$HASHES_FILE"

ENTRY_COUNT=$(jq length "$HASHES_FILE")
echo "  wrote $ENTRY_COUNT hash entr$(if [[ "$ENTRY_COUNT" -eq 1 ]]; then echo "y"; else echo "ies"; fi)."
echo ""
echo "Done. Regenerate the flake with:"
echo "  cargo generate installers flake"
