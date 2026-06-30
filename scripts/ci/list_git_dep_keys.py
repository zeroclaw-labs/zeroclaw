#!/usr/bin/env python3
"""Print a sorted JSON array of Cargo.lock git-dep keys (name-version).

Intended for CI drift-checking against nix/hashes.json.
"""

from __future__ import annotations

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
    if len(sys.argv) > 1:
        path = sys.argv[1]
    else:
        path = "Cargo.lock"

    with open(path, "rb") as f:
        data = tom.load(f)

    keys: list[str] = sorted(
        "{}-{}".format(pkg["name"], pkg["version"])
        for pkg in data.get("package", [])
        if (pkg.get("source") or "").startswith("git+")
    )

    print(json.dumps(keys))


if __name__ == "__main__":
    main()
