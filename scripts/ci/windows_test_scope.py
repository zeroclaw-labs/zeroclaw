#!/usr/bin/env python3
"""Select the advisory Windows nextest scope from Git paths and Cargo metadata."""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from pathlib import PurePosixPath


DESKTOP_PACKAGE = "zeroclaw-desktop"
PLUGIN_HOST_PACKAGE = "zeroclaw-plugins"
PLUGIN_FEATURE_OWNER_PACKAGES = {"zeroclaw", "zeroclaw-gateway", "zeroclaw-providers"}
SAFE_PACKAGE_NAME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.+-]*$")
DOC_SUFFIXES = {".md", ".mdx", ".markdown", ".rst"}
FULL_PATHS = {
    ".github/workflows/ci.yml",
    "scripts/ci/windows_test_scope.py",
    "scripts/ci/windows_test_scope.test.sh",
    "Cargo.toml",
}
PLUGIN_HOST_PATH_PREFIXES = (
    "crates/zeroclaw-plugins/",
    "crates/zeroclaw-runtime/",
    "crates/zeroclaw-config/",
    "wit/",
)
PLUGIN_HOST_EXACT_PATHS = {
    ".github/workflows/ci.yml",
    "Cargo.lock",
    "Cargo.toml",
    "scripts/ci/windows_test_scope.py",
    "scripts/ci/windows_test_scope.test.sh",
    "tests/plugin_channel_runtime_e2e.rs",
}
PLUGIN_HOST_SCRIPT_PREFIX = "scripts/ci/plugin_backend_change_filter"
PLUGIN_HOST_CONTROL_PREFIXES = (
    ".cargo/",
    ".github/actions/",
)
DYNAMIC_TEST_FIXTURE_PREFIX = "crates/zeroclaw-plugins/tests/fixtures/"


@dataclass(frozen=True)
class Package:
    package_id: str
    name: str
    root: Path


@dataclass(frozen=True)
class Selection:
    mode: str
    packages: tuple[str, ...]
    reason: str
    needs_plugin_host: bool


def full(reason: str, needs_plugin_host: bool = False) -> Selection:
    return Selection("full", (), reason, needs_plugin_host)


def requires_plugin_host(changed_paths: list[str]) -> bool:
    return any(
        path in PLUGIN_HOST_EXACT_PATHS
        or path.startswith(PLUGIN_HOST_PATH_PREFIXES)
        or path.startswith(PLUGIN_HOST_CONTROL_PREFIXES)
        or (path.startswith(PLUGIN_HOST_SCRIPT_PREFIX) and path.endswith(".sh"))
        or PurePosixPath(path).name in {"rust-toolchain", "rust-toolchain.toml"}
        for path in changed_paths
    )


def normalize_git_path(raw_path: str) -> str | None:
    path = raw_path
    if not path or "\x00" in path or "\\" in path:
        return None
    pure = PurePosixPath(path)
    if pure.is_absolute() or ".." in pure.parts:
        return None
    return pure.as_posix()


def read_changed_paths(path: Path) -> tuple[list[str], bool]:
    try:
        raw_paths = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError):
        return [], False

    paths: list[str] = []
    for raw_path in raw_paths:
        normalized = normalize_git_path(raw_path)
        if normalized is None:
            return [], False
        if normalized:
            paths.append(normalized)
    return paths, True


def metadata_path(repo_root: Path, manifest_path: str) -> Path | None:
    manifest = Path(manifest_path)
    if not manifest.is_absolute():
        manifest = repo_root / manifest
    manifest = manifest.resolve()
    if manifest.name != "Cargo.toml":
        return None
    try:
        manifest.relative_to(repo_root)
    except ValueError:
        return None
    return manifest.parent


def load_packages(
    metadata_file: Path, repo_root: Path
) -> tuple[list[Package], dict[str, set[str]], str | None]:
    try:
        metadata = json.loads(metadata_file.read_text(encoding="utf-8"))
        raw_packages = metadata["packages"]
        workspace_members = metadata["workspace_members"]
        raw_nodes = metadata["resolve"]["nodes"]
        if not isinstance(raw_packages, list) or not isinstance(workspace_members, list):
            raise ValueError("packages and workspace_members must be arrays")
        if not isinstance(raw_nodes, list):
            raise ValueError("resolve nodes must be an array")
        member_ids = {member for member in workspace_members if isinstance(member, str)}
        if len(member_ids) != len(workspace_members):
            raise ValueError("workspace member ids must be strings")
    except (OSError, UnicodeError, json.JSONDecodeError, KeyError, TypeError, ValueError) as error:
        return [], {}, f"Cargo metadata is malformed or unavailable ({type(error).__name__})."

    packages: list[Package] = []
    seen_roots: set[Path] = set()
    for raw_package in raw_packages:
        if not isinstance(raw_package, dict):
            return [], {}, "Cargo metadata is malformed or unavailable (package entry)."
        package_id = raw_package.get("id")
        name = raw_package.get("name")
        manifest_value = raw_package.get("manifest_path")
        if not isinstance(package_id, str) or not isinstance(name, str) or not isinstance(manifest_value, str):
            return [], {}, "Cargo metadata is malformed or unavailable (package fields)."
        if package_id not in member_ids:
            continue
        if not SAFE_PACKAGE_NAME.fullmatch(name):
            return [], {}, "Cargo metadata is malformed or unavailable (package name)."
        root = metadata_path(repo_root, manifest_value)
        if root is None:
            return [], {}, "Cargo metadata is malformed or unavailable (manifest path)."
        if root in seen_roots:
            return [], {}, "Cargo metadata is ambiguous (multiple packages share a root)."
        seen_roots.add(root)
        packages.append(Package(package_id, name, root))

    if not packages:
        return [], {}, "Cargo metadata is malformed or unavailable (no workspace packages)."

    packages_by_id = {package.package_id: package for package in packages}
    if set(packages_by_id) != member_ids:
        return [], {}, "Cargo metadata is malformed or unavailable (workspace package set)."

    reverse_dependents = {package.name: set() for package in packages}
    seen_nodes: set[str] = set()
    for raw_node in raw_nodes:
        if not isinstance(raw_node, dict):
            return [], {}, "Cargo metadata is malformed or unavailable (resolve node)."
        package_id = raw_node.get("id")
        raw_deps = raw_node.get("deps")
        if not isinstance(package_id, str) or not isinstance(raw_deps, list):
            return [], {}, "Cargo metadata is malformed or unavailable (resolve fields)."
        if package_id not in member_ids:
            continue
        seen_nodes.add(package_id)
        dependent = packages_by_id[package_id]
        for raw_dep in raw_deps:
            if not isinstance(raw_dep, dict) or not isinstance(raw_dep.get("pkg"), str):
                return [], {}, "Cargo metadata is malformed or unavailable (dependency edge)."
            dependency = packages_by_id.get(raw_dep["pkg"])
            if dependency is not None:
                reverse_dependents[dependency.name].add(dependent.name)

    if seen_nodes != member_ids:
        return [], {}, "Cargo metadata is malformed or unavailable (workspace resolve set)."
    return packages, reverse_dependents, None


def package_for(path: str, repo_root: Path, packages: list[Package]) -> Package | None:
    absolute_path = (repo_root / PurePosixPath(path)).resolve()
    matches: list[Package] = []
    for package in packages:
        try:
            absolute_path.relative_to(package.root)
        except ValueError:
            continue
        matches.append(package)
    if not matches:
        return None
    matches.sort(key=lambda package: len(package.root.parts), reverse=True)
    if len(matches) > 1 and len(matches[0].root.parts) == len(matches[1].root.parts):
        return None
    return matches[0]


def is_package_test_path(path: str, package: Package, repo_root: Path) -> bool:
    absolute_path = (repo_root / PurePosixPath(path)).resolve()
    try:
        relative_path = absolute_path.relative_to(package.root)
    except ValueError:
        return False
    if relative_path == Path("Cargo.toml"):
        return True
    if relative_path.parts and relative_path.parts[0] in {"src", "tests", "benches", "examples"}:
        return True
    return package.root != repo_root and relative_path.suffix.lower() == ".rs"


def is_obviously_irrelevant(path: str) -> bool:
    parts = PurePosixPath(path).parts
    is_test_area = any(part in {"src", "tests", "benches", "examples"} for part in parts)
    if path.startswith(("docs/", "web/")):
        return True
    if path.startswith(".github/actions/"):
        return False
    if path.startswith(".github/") and not path.startswith(".github/workflows/"):
        return True
    if path.startswith("scripts/") and not path.startswith("scripts/ci/"):
        return True
    if PurePosixPath(path).suffix.lower() in DOC_SUFFIXES and not is_test_area:
        return True
    if PurePosixPath(path).name.startswith("LICENSE") and not is_test_area:
        return True
    return parts == (".gitignore",)


def close_over_reverse_dependents(
    selected: set[str], reverse_dependents: dict[str, set[str]]
) -> set[str]:
    closure = set(selected)
    pending = list(selected)
    while pending:
        package = pending.pop()
        for dependent in reverse_dependents.get(package, set()):
            if dependent not in closure:
                closure.add(dependent)
                pending.append(dependent)
    closure.discard(DESKTOP_PACKAGE)
    return closure


def select_pull_request(
    changed_paths: list[str],
    repo_root: Path,
    packages: list[Package],
    reverse_dependents: dict[str, set[str]],
) -> Selection:
    if not changed_paths:
        return Selection("skip", (), "No covered Rust compilation or test paths changed.", False)

    needs_plugin_host = requires_plugin_host(changed_paths)
    selected: set[str] = set()
    lockfile_changed = False

    for path in changed_paths:
        if path == "Cargo.lock":
            lockfile_changed = True
            continue
        if path in FULL_PATHS or path == ".cargo" or path.startswith(".cargo/"):
            return full(
                "Workspace-wide or ambiguous Rust-affecting change requires the full suite.",
                needs_plugin_host,
            )
        if path.startswith(".github/workflows/") or path.startswith("scripts/ci/"):
            return full(
                "Workspace-wide or ambiguous Rust-affecting change requires the full suite.",
                needs_plugin_host,
            )
        if PurePosixPath(path).name in {"rust-toolchain", "rust-toolchain.toml"}:
            return full(
                "Workspace-wide or ambiguous Rust-affecting change requires the full suite.",
                needs_plugin_host,
            )
        if path.startswith(DYNAMIC_TEST_FIXTURE_PREFIX):
            return full(
                "Dynamically consumed plugin test fixtures require the full suite.",
                needs_plugin_host,
            )
        if is_obviously_irrelevant(path):
            continue

        package = package_for(path, repo_root, packages)
        if package is None:
            return full(
                "Workspace-wide or ambiguous Rust-affecting change requires the full suite.",
                needs_plugin_host,
            )
        if package.name == DESKTOP_PACKAGE:
            continue
        if not is_package_test_path(path, package, repo_root):
            return full(
                "Workspace-wide or ambiguous Rust-affecting change requires the full suite.",
                needs_plugin_host,
            )
        selected.add(package.name)

    if lockfile_changed:
        return full("Cargo.lock changes require the full suite.", needs_plugin_host)

    if not selected:
        return Selection("skip", (), "No covered Rust compilation or test paths changed.", needs_plugin_host)
    needs_plugin_host = needs_plugin_host or bool(selected & PLUGIN_FEATURE_OWNER_PACKAGES)
    selected = close_over_reverse_dependents(selected, reverse_dependents)
    needs_plugin_host = needs_plugin_host or PLUGIN_HOST_PACKAGE in selected
    return Selection(
        "scoped", tuple(sorted(selected)), "Changed paths map to workspace packages.", needs_plugin_host
    )


def select(event: str, changed_file: Path | None, metadata_file: Path | None, repo_root: Path) -> Selection:
    if event != "pull_request":
        return full("Non-pull-request events use the full suite.", True)
    if changed_file is None:
        return full("Changed paths or Cargo metadata are unavailable; selecting full is safer.", True)
    changed_paths, paths_ok = read_changed_paths(changed_file)
    if not paths_ok:
        return full("Changed paths are malformed or unavailable; selecting full is safer.", True)
    if metadata_file is None:
        return full("Changed paths or Cargo metadata are unavailable; selecting full is safer.", True)
    packages, reverse_dependents, metadata_error = load_packages(metadata_file, repo_root)
    if metadata_error is not None:
        return full(metadata_error, True)
    return select_pull_request(changed_paths, repo_root, packages, reverse_dependents)


def emit(selection: Selection) -> None:
    print(f"mode={selection.mode}")
    print(f"packages={json.dumps(list(selection.packages), ensure_ascii=True, separators=(',', ':'))}")
    print(f"reason={selection.reason}")
    print(f"needs_plugin_host={'true' if selection.needs_plugin_host else 'false'}")


def emit_package_args(raw_packages: str) -> int:
    try:
        packages = json.loads(raw_packages)
    except json.JSONDecodeError:
        return 2
    if not isinstance(packages, list) or not packages:
        return 2
    if any(not isinstance(package, str) or not SAFE_PACKAGE_NAME.fullmatch(package) for package in packages):
        return 2
    if len(set(packages)) != len(packages):
        return 2
    for package in packages:
        print("-p")
        print(package)
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--event")
    parser.add_argument("--changed-paths-file", "--changed-paths", dest="changed_file", type=Path)
    parser.add_argument("--metadata-file", "--metadata", dest="metadata_file", type=Path)
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    parser.add_argument("--package-args-json")
    args = parser.parse_args()
    if args.package_args_json is not None:
        return emit_package_args(args.package_args_json)
    if args.event is None:
        parser.error("--event is required unless --package-args-json is used")
    emit(select(args.event, args.changed_file, args.metadata_file, args.repo_root.resolve()))
    return 0


if __name__ == "__main__":
    sys.exit(main())
