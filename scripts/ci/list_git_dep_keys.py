#!/usr/bin/env python3
"""Print a sorted JSON array of Cargo.lock git-dep keys (name-version).

Intended for CI drift-checking against nix/hashes.json.
"""

from __future__ import annotations

import argparse
import json
import sys

for mod in ("tomllib", "tomli", "toml"):
    try:
        tom = __import__(mod)
        break
    except ImportError:
        continue
else:
    print("error: no TOML parser found (install tomli or use Python 3.11+)", file=sys.stderr)
    sys.exit(1)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("lockfile", nargs="?", default="Cargo.lock")
    parser.add_argument("--check-hashes", metavar="PATH", help="Check for missing or stale hash keys")
    args = parser.parse_args()

    with open(args.lockfile, "rb") as f:
        data = tom.load(f)

    keys: list[str] = sorted(
        "{}-{}".format(pkg["name"], pkg["version"])
        for pkg in data.get("package", [])
        if (pkg.get("source") or "").startswith("git+")
    )

    if args.check_hashes is None:
        print(json.dumps(keys))
        return

    with open(args.check_hashes, encoding="utf-8") as f:
        hashes = json.load(f)
    if not isinstance(hashes, dict):
        parser.error("hashes must be a JSON object")

    missing = sorted(set(keys) - hashes.keys())
    stale = sorted(hashes.keys() - set(keys))
    if missing or stale:
        if missing:
            print("Missing hash keys: " + ", ".join(missing), file=sys.stderr)
        if stale:
            print("Stale hash keys: " + ", ".join(stale), file=sys.stderr)
        print("Run scripts/dev/refresh-nix-hashes.sh and commit the result.", file=sys.stderr)
        sys.exit(1)
    print(f"Hash keys match all {len(keys)} Git dependencies in {args.lockfile}.")


if __name__ == "__main__":
    main()
