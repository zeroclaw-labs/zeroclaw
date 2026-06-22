#!/usr/bin/env bash

set -euo pipefail

echo "==> zerocode gate: no zeroclaw-* crate dependency"

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

manifest="apps/zerocode/Cargo.toml"

offending="$(
    python3 - "$manifest" <<'PY'
import sys
import tomllib

with open(sys.argv[1], "rb") as handle:
    manifest = tomllib.load(handle)

own_name = manifest.get("package", {}).get("name", "")

dep_tables = []
for key in ("dependencies", "dev-dependencies", "build-dependencies"):
    table = manifest.get(key)
    if isinstance(table, dict):
        dep_tables.append(table)

target = manifest.get("target")
if isinstance(target, dict):
    for cfg in target.values():
        if not isinstance(cfg, dict):
            continue
        for key in ("dependencies", "dev-dependencies", "build-dependencies"):
            table = cfg.get(key)
            if isinstance(table, dict):
                dep_tables.append(table)

found = set()


def flag(label):
    if label.startswith("zeroclaw-") or label.startswith("zeroclaw_"):
        found.add(label)


for table in dep_tables:
    for name, spec in table.items():
        if name == own_name:
            continue
        flag(name)
        # Cargo renamed dependencies declare the real crate under `package`
        # while the table key is an arbitrary local alias. Inspect both so a
        # rename like `x = { package = "zeroclaw-providers" }` cannot slip past.
        if isinstance(spec, dict):
            package = spec.get("package")
            if isinstance(package, str):
                flag(package)

for name in sorted(found):
    print(name)
PY
)"

if [ -n "$offending" ]; then
    echo "::error file=${manifest}::zerocode must not depend on any zeroclaw-* crate; found:"
    while IFS= read -r dep; do
        echo "  - ${dep}"
    done <<<"$offending"
    echo "zerocode is an RPC-only surface: everything it knows must come over the wire, not by linking backend crates."
    exit 1
fi

echo "zerocode gate passed: no zeroclaw-* dependencies."
