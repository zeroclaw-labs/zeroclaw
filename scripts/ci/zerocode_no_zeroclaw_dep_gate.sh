#!/usr/bin/env bash

set -euo pipefail

echo "==> zerocode gate: zeroclaw-api is the only allowed zeroclaw-* dependency"

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

manifest="apps/zerocode/Cargo.toml"

offending="$(
    python3 - "$manifest" <<'PY'
import sys
import tomllib

with open(sys.argv[1], "rb") as handle:
    manifest = tomllib.load(handle)

def dependency_tables(candidate):
    tables = []
    for key in ("dependencies", "dev-dependencies", "build-dependencies"):
        table = candidate.get(key)
        if isinstance(table, dict):
            tables.append(table)

    target = candidate.get("target")
    if isinstance(target, dict):
        for cfg in target.values():
            if not isinstance(cfg, dict):
                continue
            for key in ("dependencies", "dev-dependencies", "build-dependencies"):
                table = cfg.get(key)
                if isinstance(table, dict):
                    tables.append(table)
    return tables


def find_offending(candidate):
    own_name = candidate.get("package", {}).get("name", "")
    found = set()

    def flag(label):
        if (
            label.startswith("zeroclaw-") or label.startswith("zeroclaw_")
        ) and label not in allowed:
            found.add(label)

    for table in dependency_tables(candidate):
        for name, spec in table.items():
            if name == own_name:
                continue
            flag(name)
            # Cargo renamed dependencies declare the real crate under `package`
            # while the table key is an arbitrary local alias. Inspect both so a
            # rename cannot hide an implementation dependency.
            if isinstance(spec, dict):
                package = spec.get("package")
                if isinstance(package, str):
                    flag(package)
    return sorted(found)


allowed = {"zeroclaw-api", "zeroclaw_api"}

# Keep the exception narrow even when a dependency is renamed or target-gated.
assert find_offending({"dependencies": {"zeroclaw-api": {"workspace": True}}}) == []
assert find_offending({"dependencies": {"zeroclaw_api": {"workspace": True}}}) == []
assert find_offending({"dependencies": {"zeroclaw-runtime": {"workspace": True}}}) == [
    "zeroclaw-runtime"
]
assert find_offending(
    {"target": {"cfg(unix)": {"dependencies": {"backend": {"package": "zeroclaw-config"}}}}}
) == ["zeroclaw-config"]

for name in find_offending(manifest):
    print(name)
PY
)"

if [ -n "$offending" ]; then
    echo "::error file=${manifest}::zerocode may depend on zeroclaw-api only; found implementation dependencies:"
    while IFS= read -r dep; do
        echo "  - ${dep}"
    done <<<"$offending"
    echo "zerocode is an RPC-only surface: shared contracts may come from zeroclaw-api, while runtime/config/channel/tool behavior must come over the wire."
    exit 1
fi

echo "zerocode gate passed: no zeroclaw-* implementation dependencies."
