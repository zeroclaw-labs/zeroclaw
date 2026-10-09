#!/usr/bin/env python3
"""Read cargo metadata on stdin; emit the complete release order or fail closed."""

import json
from pathlib import Path
import sys


def registry_visible(package, dependency):
    if dependency["kind"] != "dev" or dependency["req"] != "*":
        return True

    # Cargo retains versioned dev-dependencies, but metadata represents both an
    # omitted version and an explicit version="*" as req="*". Resolve that one
    # ambiguity from the manifest, including target tables and workspace aliases.
    import tomllib

    with open(package["manifest_path"], "rb") as source:
        manifest = tomllib.load(source)
    target = dependency["target"]
    table = manifest if target is None else manifest["target"][target]
    key = dependency["rename"] or dependency["name"]
    spec = table["dev-dependencies"][key]
    if isinstance(spec, dict) and spec.get("workspace") is True:
        with Path("Cargo.toml").open("rb") as source:
            spec = tomllib.load(source)["workspace"]["dependencies"][key]
    return isinstance(spec, str) or "version" in spec


def publish_order(meta, version):
    packages = {p["name"]: p for p in meta["packages"]}
    publishable = {
        name for name, package in packages.items()
        if package["publish"] is None and package["version"] == version
    }
    deps = {}
    for name in sorted(publishable):
        deps[name] = set()
        for dependency in packages[name]["dependencies"]:
            # Require the fields used to classify an edge even for external
            # dependencies; incomplete metadata must not silently drop edges.
            target = dependency["name"]
            kind, requirement = dependency["kind"], dependency["req"]
            if kind not in (None, "build", "dev") or not isinstance(requirement, str):
                raise ValueError(f"invalid dependency metadata: {name} -> {target}")
            if target not in packages or not registry_visible(packages[name], dependency):
                continue
            if packages[target]["publish"] is not None:
                raise ValueError(f"unpublishable workspace dependency: {name} -> {target}")
            if target in publishable:
                deps[name].add(target)

    order, state = [], {}

    def visit(name, trail=()):
        if state.get(name) == "done":
            return
        if state.get(name) == "visiting":
            raise ValueError("dependency cycle: " + " -> ".join(trail + (name,)))
        state[name] = "visiting"
        for dependency in sorted(deps[name]):
            visit(dependency, trail + (name,))
        state[name] = "done"
        order.append(name)

    for name in sorted(publishable):
        visit(name)
    return order


if __name__ == "__main__":
    try:
        order = publish_order(json.load(sys.stdin), sys.argv[1])
    except (ValueError, KeyError, TypeError, OSError, ImportError) as error:
        sys.exit(f"error: invalid publish graph: {error}")
    print("\n".join(order))
