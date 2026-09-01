#!/usr/bin/env python3
"""Fail if the temporary lru advisory exception expands beyond Nostr 0.44."""

from pathlib import Path
import tomllib


FIXED_VERSION = (0, 18, 2)
EXPECTED_VULNERABLE_VERSION = "0.16.4"
EXPECTED_PARENTS = {
    ("nostr-database", "0.44.0"),
    ("nostr-relay-pool", "0.44.3"),
}


def is_vulnerable_lru(version: str) -> bool:
    core_and_build = version.split("+", 1)[0]
    release, prerelease_separator, _ = core_and_build.partition("-")
    parts = release.split(".")
    if len(parts) < 3 or not all(part.isdigit() for part in parts[:3]):
        raise ValueError(f"unsupported lru version: {version}")
    release_version = tuple(int(part) for part in parts[:3])
    return bool(prerelease_separator) or release_version < FIXED_VERSION


def validate_version_boundary() -> None:
    cases = {
        "0.16.4": True,
        "0.18.2-alpha.1": True,
        "0.18.2": False,
        "0.18.3-alpha.1": True,
    }
    for version, expected in cases.items():
        actual = is_vulnerable_lru(version)
        if actual != expected:
            raise SystemExit(
                f"lru version boundary error for {version}: "
                f"expected vulnerable={expected}, got {actual}"
            )


def depends_on_exact_lru(package: dict, version: str) -> bool:
    target = f"lru {version}"
    return any(
        dependency == target or dependency.startswith(f"{target} ")
        for dependency in package.get("dependencies", [])
    )


def main() -> None:
    validate_version_boundary()
    repo_root = Path(__file__).resolve().parents[2]
    with (repo_root / "Cargo.lock").open("rb") as lock_file:
        packages = tomllib.load(lock_file)["package"]

    vulnerable_versions = {
        package["version"]
        for package in packages
        if package["name"] == "lru"
        and is_vulnerable_lru(package["version"])
    }
    expected_versions = {EXPECTED_VULNERABLE_VERSION}
    if vulnerable_versions != expected_versions:
        raise SystemExit(
            "unexpected vulnerable lru versions: "
            f"expected {sorted(expected_versions)}, got {sorted(vulnerable_versions)}"
        )

    parents = {
        (package["name"], package["version"])
        for package in packages
        if depends_on_exact_lru(package, EXPECTED_VULNERABLE_VERSION)
    }
    if parents != EXPECTED_PARENTS:
        raise SystemExit(
            "unexpected parents of lru 0.16.4: "
            f"expected {sorted(EXPECTED_PARENTS)}, got {sorted(parents)}"
        )

    print("lru advisory scope: constrained to Nostr 0.44 cache packages")


if __name__ == "__main__":
    main()
