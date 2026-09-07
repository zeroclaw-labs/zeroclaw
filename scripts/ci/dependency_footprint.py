#!/usr/bin/env python3
"""Deterministic, local Cargo dependency-footprint capture and comparison."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import secrets
import stat
import subprocess
import sys
import tomllib
from pathlib import Path
from typing import Any, NamedTuple


SCHEMA_VERSION = 1
SCRIPT_ROOT = Path(__file__).resolve().parents[2]
PROFILE_ID_RE = re.compile(r"^[a-z][a-z0-9-]*$")
PACKAGE_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]*$")
FEATURE_RE = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_:+./-]*$")
FEATURE_NAME_RE = re.compile(r"^[A-Za-z0-9_][A-Za-z0-9_:+.-]*$")
VERSION_RE = re.compile(r"^[0-9A-Za-z][0-9A-Za-z.+-]*$")
PACKAGE_FIELD_RE = re.compile(
    r"(?P<name>[A-Za-z0-9][A-Za-z0-9_-]*)"
    r"\s+v(?P<version>[0-9A-Za-z][0-9A-Za-z.+-]*)"
    r"(?:\s+\([^\t\r\n]+\))?"
)


class ToolError(Exception):
    pass


class PreparedOutput(NamedTuple):
    path: Path
    parent: Path
    parent_identity: tuple[int, int]


def fail(message: str) -> ToolError:
    return ToolError(message)


def reject_unknown(value: dict[str, Any], allowed: set[str], label: str) -> None:
    unknown = sorted(set(value) - allowed)
    if unknown:
        raise fail(f"{label}: unknown field(s): {', '.join(unknown)}")


def require_string(value: Any, label: str, pattern: re.Pattern[str] | None = None) -> str:
    if not isinstance(value, str) or not value:
        raise fail(f"{label}: expected a non-empty string")
    if pattern is not None and pattern.fullmatch(value) is None:
        raise fail(f"{label}: malformed value {value!r}")
    return value


def require_string_list(value: Any, label: str, allow_empty: bool = True) -> list[str]:
    if not isinstance(value, list) or (not allow_empty and not value):
        raise fail(f"{label}: expected a {'non-empty ' if not allow_empty else ''}list")
    result: list[str] = []
    for index, item in enumerate(value):
        result.append(require_string(item, f"{label}[{index}]", FEATURE_RE))
    if len(set(result)) != len(result):
        raise fail(f"{label}: duplicate values")
    return result


def require_feature_ref(value: Any, label: str) -> str:
    feature = require_string(value, label, FEATURE_RE)
    if "/" not in feature:
        return feature
    parts = feature.split("/")
    if len(parts) != 2 or PACKAGE_RE.fullmatch(parts[0]) is None or FEATURE_NAME_RE.fullmatch(parts[1]) is None:
        raise fail(f"{label}: malformed package/feature reference {feature!r}")
    return feature


def require_feature_ref_list(value: Any, label: str, allow_empty: bool = True) -> list[str]:
    if not isinstance(value, list) or (not allow_empty and not value):
        raise fail(f"{label}: expected a {'non-empty ' if not allow_empty else ''}list")
    result = [require_feature_ref(item, f"{label}[{index}]") for index, item in enumerate(value)]
    if len(set(result)) != len(result):
        raise fail(f"{label}: duplicate values")
    return result


def require_stable_context_string(value: Any, label: str) -> str:
    result = require_string(value, label)
    if (
        result.startswith(("/", "\\"))
        or re.match(r"^[A-Za-z]:[\\/]", result)
        or any(marker in result for marker in ("/tmp/", "/private/tmp/", "/var/folders/", "CARGO_HOME", "RUSTUP_HOME"))
    ):
        raise fail(f"{label}: absolute, temporary, or cache path is not allowed")
    return result


def load_policy(path: Path) -> tuple[dict[str, Any], str]:
    try:
        raw = path.read_bytes()
        policy = tomllib.loads(raw.decode("utf-8"))
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError) as exc:
        raise fail(f"could not read policy {path}: {exc}") from exc
    if not isinstance(policy, dict):
        raise fail("policy: expected a table")
    reject_unknown(policy, {"schema_version", "edge_kinds", "profiles", "contracts"}, "policy")
    if type(policy.get("schema_version")) is not int or policy["schema_version"] != SCHEMA_VERSION:
        raise fail("policy.schema_version: expected 1")
    edges = policy.get("edge_kinds")
    if (
        not isinstance(edges, list)
        or len(edges) != 2
        or any(not isinstance(item, str) or item not in {"normal", "build"} for item in edges)
        or sorted(edges) != ["build", "normal"]
    ):
        raise fail("policy.edge_kinds: expected exactly [\"normal\", \"build\"]")
    profiles_raw = policy.get("profiles")
    if not isinstance(profiles_raw, list) or not profiles_raw:
        raise fail("policy.profiles: expected a non-empty array")
    profiles: list[dict[str, Any]] = []
    profile_ids: set[str] = set()
    for index, raw_profile in enumerate(profiles_raw):
        label = f"policy.profiles[{index}]"
        if not isinstance(raw_profile, dict):
            raise fail(f"{label}: expected a table")
        reject_unknown(raw_profile, {"id", "package", "mode", "features", "selection"}, label)
        profile_id = require_string(raw_profile.get("id"), f"{label}.id", PROFILE_ID_RE)
        if profile_id in profile_ids:
            raise fail(f"{label}.id: duplicate id {profile_id!r}")
        profile_ids.add(profile_id)
        package = require_string(raw_profile.get("package"), f"{label}.package", PACKAGE_RE)
        mode = require_string(raw_profile.get("mode"), f"{label}.mode")
        if mode not in {"defaults", "no-default-features", "features", "selection"}:
            raise fail(f"{label}.mode: incompatible mode {mode!r}")
        has_features = "features" in raw_profile
        has_selection = "selection" in raw_profile
        if mode == "features":
            if has_selection:
                raise fail(f"{label}: features mode cannot also set selection")
            features = require_feature_ref_list(raw_profile.get("features"), f"{label}.features", False)
        elif has_features:
            raise fail(f"{label}.features: only valid in features mode")
        else:
            features = []
        if mode == "selection":
            if not has_selection:
                raise fail(f"{label}.selection: required in selection mode")
            selection = require_string(raw_profile.get("selection"), f"{label}.selection", PROFILE_ID_RE)
        elif has_selection:
            raise fail(f"{label}.selection: only valid in selection mode")
        else:
            selection = None
        profiles.append(
            {
                "id": profile_id,
                "package": package,
                "mode": mode,
                "features": features,
                "selection": selection,
            }
        )

    contracts_raw = policy.get("contracts", [])
    if not isinstance(contracts_raw, list):
        raise fail("policy.contracts: expected an array")
    contracts: list[dict[str, Any]] = []
    contract_ids: set[str] = set()
    for index, raw_contract in enumerate(contracts_raw):
        label = f"policy.contracts[{index}]"
        if not isinstance(raw_contract, dict):
            raise fail(f"{label}: expected a table")
        reject_unknown(raw_contract, {"id", "profile", "kind", "package", "feature", "expect"}, label)
        contract_id = require_string(raw_contract.get("id"), f"{label}.id", PROFILE_ID_RE)
        if contract_id in contract_ids:
            raise fail(f"{label}.id: duplicate id {contract_id!r}")
        contract_ids.add(contract_id)
        profile = require_string(raw_contract.get("profile"), f"{label}.profile", PROFILE_ID_RE)
        if profile not in profile_ids:
            raise fail(f"{label}.profile: unknown profile {profile!r}")
        kind = require_string(raw_contract.get("kind"), f"{label}.kind")
        package = require_string(raw_contract.get("package"), f"{label}.package", PACKAGE_RE)
        expect = require_string(raw_contract.get("expect"), f"{label}.expect")
        if kind == "package":
            if expect not in {"present", "absent"} or "feature" in raw_contract:
                raise fail(f"{label}: malformed package contract")
            feature = None
        elif kind == "feature":
            if expect not in {"enabled", "forbidden"}:
                raise fail(f"{label}.expect: expected enabled or forbidden")
            feature = require_string(raw_contract.get("feature"), f"{label}.feature", FEATURE_NAME_RE)
        else:
            raise fail(f"{label}.kind: unknown contract kind {kind!r}")
        contracts.append(
            {
                "id": contract_id,
                "profile": profile,
                "kind": kind,
                "package": package,
                "feature": feature,
                "expect": expect,
            }
        )
    normalized = {"schema_version": 1, "edge_kinds": ["normal", "build"], "profiles": profiles, "contracts": contracts}
    return normalized, hashlib.sha256(raw).hexdigest()


def reject_duplicate_json_pairs(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON field {key!r}")
        result[key] = value
    return result


def load_json(path: Path, label: str) -> Any:
    try:
        with path.open("r", encoding="utf-8") as handle:
            return json.load(handle, object_pairs_hook=reject_duplicate_json_pairs)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError, ValueError) as exc:
        raise fail(f"could not read {label} {path}: {exc}") from exc


def run_command_bytes(args: list[str], cwd: Path, label: str) -> bytes:
    try:
        completed = subprocess.run(args, cwd=cwd, capture_output=True, check=False)
    except OSError as exc:
        raise fail(f"{label}: could not start subprocess: {exc}") from exc
    if completed.returncode != 0:
        detail_bytes = completed.stderr.strip() or completed.stdout.strip()
        detail = detail_bytes.decode("utf-8", errors="replace") if detail_bytes else "no output"
        raise fail(f"{label}: subprocess failed with status {completed.returncode}: {detail}")
    return completed.stdout


def run_command(args: list[str], cwd: Path, label: str) -> str:
    output = run_command_bytes(args, cwd, label)
    try:
        return output.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise fail(f"{label}: subprocess output is not UTF-8") from exc


def resolve_selection(cargo: str, selection: str, repo_root: Path, target: str | None) -> list[str]:
    command = [
        cargo,
        "run",
        "--locked",
        "--quiet",
        "-p",
        "xtask",
        "--bin",
        "generate",
        "--",
        "features",
        "--selection",
        selection,
    ]
    if target:
        command.extend(["--target", target])
    output = run_command(command, repo_root, f"selection {selection}")
    lines = [line.strip() for line in output.splitlines() if line.strip()]
    if len(lines) != 1:
        raise fail(f"selection {selection}: expected one non-empty feature line")
    return require_feature_ref_list(lines[0].split(","), f"selection {selection}", True)


def profile_inputs(profile: dict[str, Any], resolved: dict[str, list[str]]) -> dict[str, Any]:
    mode = profile["mode"]
    features = list(profile["features"])
    selection = profile["selection"]
    if mode == "selection":
        if selection not in resolved:
            raise fail(f"profile {profile['id']}: missing resolved selection {selection!r}")
        features = list(resolved[selection])
    return {
        "mode": mode,
        "no_default_features": mode != "defaults",
        "features": sorted(features),
        "selection": selection,
    }


def parse_feature_field(value: str, label: str) -> list[str]:
    if not value:
        return []
    parts = value.split(",")
    if any(not part for part in parts):
        raise fail(f"{label}: malformed enabled feature list")
    for part in parts:
        require_string(part, f"{label}.feature", FEATURE_NAME_RE)
    if len(set(parts)) != len(parts):
        raise fail(f"{label}: duplicate enabled feature")
    return parts


def parse_tree(raw: str, profile_id: str, root_package: str) -> dict[str, Any]:
    if not raw.strip():
        raise fail(f"profile {profile_id}: Cargo output is empty")
    packages: dict[tuple[str, str], set[str]] = {}
    for number, line in enumerate(raw.splitlines(), start=1):
        if not line:
            continue
        fields = line.split("\t")
        if len(fields) != 2:
            raise fail(f"profile {profile_id}: malformed Cargo tree line {number}: {line!r}")
        package_field, feature_field = fields
        if not feature_field.startswith("features:"):
            raise fail(f"profile {profile_id}: malformed Cargo tree line {number}: {line!r}")
        feature_field = feature_field.removesuffix(" (*)")
        match = PACKAGE_FIELD_RE.fullmatch(package_field)
        if match is None:
            raise fail(f"profile {profile_id}: malformed Cargo tree line {number}: {line!r}")
        name = match.group("name")
        version = match.group("version")
        if VERSION_RE.fullmatch(version) is None:
            raise fail(f"profile {profile_id}: malformed package version on line {number}")
        features = parse_feature_field(
            feature_field.removeprefix("features:"),
            f"profile {profile_id} line {number}",
        )
        packages.setdefault((name, version), set()).update(features)
    if not packages:
        raise fail(f"profile {profile_id}: Cargo output contains no packages")
    if not any(name == root_package for name, _version in packages):
        raise fail(f"profile {profile_id}: selected root package {root_package!r} is missing")
    package_pairs = [{"name": name, "version": version} for name, version in sorted(packages)]
    enabled_features = [
        {"name": name, "version": version, "features": sorted(features)}
        for (name, version), features in sorted(packages.items())
        if features
    ]
    versions_by_name: dict[str, set[str]] = {}
    for name, version in packages:
        versions_by_name.setdefault(name, set()).add(version)
    counts = {
        "package_pairs": len(package_pairs),
        "unique_package_names": len(versions_by_name),
        "duplicate_version_names": sum(len(versions) > 1 for versions in versions_by_name.values()),
        "enabled_features": sum(len(item["features"]) for item in enabled_features),
    }
    return {
        "package_pairs": package_pairs,
        "enabled_features": enabled_features,
        "counts": counts,
    }


def capture_path(raw_dir: Path, profile_id: str) -> Path:
    candidates = [
        raw_dir / profile_id,
        raw_dir / f"{profile_id}.tree",
        raw_dir / f"{profile_id}.txt",
        raw_dir / f"{profile_id}.raw",
    ]
    existing = [candidate for candidate in candidates if candidate.is_file()]
    if len(existing) != 1:
        if not existing:
            raise fail(f"profile {profile_id}: missing capture in {raw_dir}")
        raise fail(f"profile {profile_id}: duplicate capture files in {raw_dir}")
    return existing[0]


def normalize_resolved_selections(value: Any, label: str) -> dict[str, list[str]]:
    if not isinstance(value, dict):
        raise fail(f"{label}: expected an object")
    normalized: dict[str, list[str]] = {}
    for selection, raw_features in value.items():
        require_string(selection, f"{label} key", PROFILE_ID_RE)
        features = require_feature_ref_list(raw_features, f"{label}.{selection}")
        normalized[selection] = sorted(features)
    return {selection: normalized[selection] for selection in sorted(normalized)}


def require_policy_selection_keys(
    selections: dict[str, list[str]],
    policy: dict[str, Any],
    label: str,
) -> None:
    expected = {profile["selection"] for profile in policy["profiles"] if profile["selection"] is not None}
    actual = set(selections)
    if actual == expected:
        return
    details = []
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    if missing:
        details.append(f"missing {', '.join(missing)}")
    if extra:
        details.append(f"extra {', '.join(extra)}")
    raise fail(f"{label}: keys do not match supplied policy ({'; '.join(details)})")


def load_context(path: Path) -> dict[str, Any]:
    context = load_json(path, "context")
    if not isinstance(context, dict):
        raise fail("context: expected a JSON object")
    reject_unknown(
        context,
        {
            "git_revision",
            "git_dirty",
            "git_worktree_digest_sha256",
            "cargo_lock_sha256",
            "cargo_version",
            "rustc_version",
            "rustc_host",
            "target",
            "resolved_selections",
        },
        "context",
    )
    for key in ("git_revision", "cargo_version", "rustc_version", "rustc_host", "target"):
        require_stable_context_string(context.get(key), f"context.{key}")
    if re.fullmatch(r"[0-9a-fA-F]{7,64}", context["git_revision"]) is None:
        raise fail("context.git_revision: malformed revision")
    if type(context.get("git_dirty")) is not bool:
        raise fail("context.git_dirty: expected a boolean")
    digest = context.get("git_worktree_digest_sha256")
    if not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
        raise fail("context.git_worktree_digest_sha256: malformed digest")
    lock_digest = context.get("cargo_lock_sha256")
    if not isinstance(lock_digest, str) or re.fullmatch(r"[0-9a-f]{64}", lock_digest) is None:
        raise fail("context.cargo_lock_sha256: malformed digest")
    result = dict(context)
    result["resolved_selections"] = normalize_resolved_selections(
        context.get("resolved_selections", {}),
        "context.resolved_selections",
    )
    return result


def validate_contracts(contracts: list[dict[str, Any]], records: dict[str, dict[str, Any]]) -> list[dict[str, str]]:
    passing: list[dict[str, str]] = []
    for contract in contracts:
        record = records[contract["profile"]]
        pair_names = {pair["name"] for pair in record["package_pairs"]}
        feature_set = {
            (item["name"], feature)
            for item in record["enabled_features"]
            for feature in item["features"]
        }
        if contract["kind"] == "package":
            actual = contract["package"] in pair_names
            expected = contract["expect"] == "present"
        else:
            actual = (contract["package"], contract["feature"]) in feature_set
            expected = contract["expect"] == "enabled"
        if actual != expected:
            raise fail(
                f"contract {contract['id']}: {contract['expect']} assertion failed for "
                f"{contract['package']}" + (f"/{contract['feature']}" if contract["feature"] else "")
            )
        passing.append({"id": contract["id"], "profile": contract["profile"], "status": "passed"})
    return sorted(passing, key=lambda item: item["id"])


def build_report(
    policy: dict[str, Any],
    policy_digest: str,
    context: dict[str, Any],
    captures: dict[str, str],
) -> dict[str, Any]:
    resolved = normalize_resolved_selections(
        context.get("resolved_selections", {}),
        "context.resolved_selections",
    )
    require_policy_selection_keys(resolved, policy, "context.resolved_selections")
    records: dict[str, dict[str, Any]] = {}
    for profile in policy["profiles"]:
        inputs = profile_inputs(profile, resolved)
        parsed = parse_tree(captures[profile["id"]], profile["id"], profile["package"])
        records[profile["id"]] = {
            "id": profile["id"],
            "package": profile["package"],
            "resolved_inputs": inputs,
            **parsed,
        }
    contracts = validate_contracts(policy["contracts"], records)
    return {
        "schema_version": SCHEMA_VERSION,
        "context": {
            key: context[key]
            for key in (
                "git_revision",
                "git_dirty",
                "git_worktree_digest_sha256",
                "cargo_lock_sha256",
                "cargo_version",
                "rustc_version",
                "rustc_host",
                "target",
            )
        }
        | {"resolved_selections": resolved},
        "evidence_units": {
            "cargo_package_name_version_pairs": "measured",
            "binary_bytes": "not measured",
            "runtime_memory": "not measured",
        },
        "policy": {"digest_sha256": policy_digest, "edge_kinds": policy["edge_kinds"]},
        "profiles": sorted(records.values(), key=lambda item: item["id"]),
        "contracts": contracts,
    }


def git_source_identity(
    repo_root: Path,
    excluded_untracked_paths: set[bytes] | None = None,
) -> dict[str, Any]:
    revision = run_command(["git", "rev-parse", "--verify", "HEAD"], repo_root, "git revision").strip()
    if not re.fullmatch(r"[0-9a-fA-F]{7,64}", revision):
        raise fail("git revision: malformed output")
    tracked_diff = run_command_bytes(
        [
            "git",
            "-c",
            "core.quotePath=true",
            "diff",
            "--binary",
            "--full-index",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--no-renames",
            "HEAD",
            "--",
        ],
        repo_root,
        "git tracked diff",
    )
    untracked_output = run_command_bytes(
        ["git", "ls-files", "--others", "--exclude-standard", "-z"],
        repo_root,
        "git untracked files",
    )
    excluded = excluded_untracked_paths or set()
    untracked_paths = sorted(
        path for path in untracked_output.split(b"\0") if path and path not in excluded
    )
    digest = hashlib.sha256()
    digest.update(len(tracked_diff).to_bytes(8, "big"))
    digest.update(tracked_diff)
    for raw_path in untracked_paths:
        path = repo_root / os.fsdecode(raw_path)
        try:
            metadata = path.lstat()
            if stat.S_ISREG(metadata.st_mode):
                kind = b"file"
                content = path.read_bytes()
            elif stat.S_ISLNK(metadata.st_mode):
                kind = b"symlink"
                content = os.fsencode(os.readlink(path))
            else:
                raise fail(f"git source identity: unsupported untracked file type {os.fsdecode(raw_path)!r}")
        except OSError as exc:
            raise fail(f"git source identity: could not read untracked file {os.fsdecode(raw_path)!r}: {exc}") from exc
        for field in (raw_path, kind, (metadata.st_mode & 0o111).to_bytes(2, "big"), content):
            digest.update(len(field).to_bytes(8, "big"))
            digest.update(field)
    return {
        "git_revision": revision,
        "git_dirty": bool(tracked_diff or untracked_paths),
        "git_worktree_digest_sha256": digest.hexdigest(),
    }


def output_identity_exclusions(repo_root: Path, output_path: Path) -> set[bytes]:
    try:
        relative = output_path.relative_to(repo_root)
    except ValueError:
        return set()
    relative_text = relative.as_posix()
    tracked = run_command_bytes(
        ["git", "--literal-pathspecs", "ls-files", "-z", "--", relative_text],
        repo_root,
        "git output ownership",
    )
    if tracked:
        raise fail(f"output path is tracked by Git: {relative_text}")
    return {os.fsencode(relative_text)}


def output_parent_flags() -> int:
    return (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_DIRECTORY", 0)
        | getattr(os, "O_NOFOLLOW", 0)
    )


def reject_output_symlink(path: Path, directory_fd: int) -> None:
    try:
        metadata = os.stat(path.name, dir_fd=directory_fd, follow_symlinks=False)
    except FileNotFoundError:
        return
    if stat.S_ISLNK(metadata.st_mode):
        raise fail(f"output path is an existing symlink: {path}")


def prepare_output(raw_path: str) -> PreparedOutput:
    path = Path(raw_path).absolute()
    try:
        parent = path.parent.resolve()
        if not parent.exists():
            raise fail(f"output parent does not exist: {path.parent}")
        directory_fd = os.open(parent, output_parent_flags())
        try:
            reject_output_symlink(path, directory_fd)
            metadata = os.fstat(directory_fd)
        finally:
            os.close(directory_fd)
    except (OSError, NotImplementedError) as exc:
        raise fail(f"could not inspect output path {path}: {exc}") from exc
    return PreparedOutput(path, parent, (metadata.st_dev, metadata.st_ino))


def context_from_capture(
    cargo: str,
    repo_root: Path,
    target: str | None,
    resolved: dict[str, list[str]],
    source_identity: dict[str, Any],
) -> dict[str, Any]:
    try:
        cargo_lock_digest = hashlib.sha256((repo_root / "Cargo.lock").read_bytes()).hexdigest()
    except OSError as exc:
        raise fail(f"Cargo.lock: could not read lockfile: {exc}") from exc
    cargo_version = run_command([cargo, "--version"], repo_root, "cargo version").strip()
    rustc_verbose = run_command(["rustc", "--version", "--verbose"], repo_root, "rustc version")
    rustc_lines = [line.strip() for line in rustc_verbose.splitlines() if line.strip()]
    rustc_version = next((line for line in rustc_lines if line.startswith("rustc ")), None)
    rustc_host = next((line.split(":", 1)[1].strip() for line in rustc_lines if line.startswith("host:")), None)
    if not rustc_version or not rustc_host:
        raise fail("rustc version: missing version or host")
    require_stable_context_string(cargo_version, "cargo version")
    require_stable_context_string(rustc_version, "rustc version")
    require_stable_context_string(rustc_host, "rustc host")
    require_stable_context_string(target or rustc_host, "target")
    return {
        **source_identity,
        "cargo_lock_sha256": cargo_lock_digest,
        "cargo_version": cargo_version,
        "rustc_version": rustc_version,
        "rustc_host": rustc_host,
        "target": target or rustc_host,
        "resolved_selections": resolved,
    }


def atomic_write(output: PreparedOutput, value: Any) -> None:
    text = json.dumps(value, ensure_ascii=True, indent=2, sort_keys=True) + "\n"
    directory_fd: int | None = None
    temporary_fd: int | None = None
    temporary_name: str | None = None
    try:
        current_parent = output.path.parent.resolve(strict=True)
        if current_parent != output.parent:
            raise fail(f"output parent changed before write: {output.path.parent}")
        directory_fd = os.open(current_parent, output_parent_flags())
        parent_metadata = os.fstat(directory_fd)
        current_identity = (parent_metadata.st_dev, parent_metadata.st_ino)
        if current_identity != output.parent_identity:
            raise fail(f"output parent changed before write: {output.path.parent}")
        reject_output_symlink(output.path, directory_fd)
        temporary_flags = (
            os.O_WRONLY
            | os.O_CREAT
            | os.O_EXCL
            | getattr(os, "O_CLOEXEC", 0)
            | getattr(os, "O_NOFOLLOW", 0)
        )
        for _attempt in range(100):
            candidate = f".{output.path.name}.{secrets.token_hex(12)}"
            try:
                temporary_fd = os.open(candidate, temporary_flags, 0o600, dir_fd=directory_fd)
            except FileExistsError:
                continue
            temporary_name = candidate
            break
        if temporary_fd is None or temporary_name is None:
            raise fail(f"could not create temporary output beside {output.path}")
        handle = os.fdopen(temporary_fd, "w", encoding="utf-8")
        temporary_fd = None
        with handle:
            handle.write(text)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(
            temporary_name,
            output.path.name,
            src_dir_fd=directory_fd,
            dst_dir_fd=directory_fd,
        )
        temporary_name = None
    except (OSError, NotImplementedError, TypeError) as exc:
        raise fail(f"could not atomically write {output.path}: {exc}") from exc
    finally:
        if temporary_fd is not None:
            try:
                os.close(temporary_fd)
            except OSError:
                pass
        if temporary_name is not None and directory_fd is not None:
            try:
                os.unlink(temporary_name, dir_fd=directory_fd)
            except (FileNotFoundError, OSError, NotImplementedError):
                pass
        if directory_fd is not None:
            try:
                os.close(directory_fd)
            except OSError:
                pass


def command_capture(args: argparse.Namespace) -> None:
    policy_path = Path(args.policy).resolve()
    repo_root = Path(args.repo_root).resolve() if args.repo_root else SCRIPT_ROOT
    output = prepare_output(args.output)
    cargo = require_string(args.cargo, "cargo command")
    policy, digest = load_policy(policy_path)
    identity_exclusions = output_identity_exclusions(repo_root, output.parent / output.path.name)
    source_identity = git_source_identity(repo_root, identity_exclusions)
    resolved: dict[str, list[str]] = {}
    for profile in policy["profiles"]:
        selection = profile["selection"]
        if selection and selection not in resolved:
            resolved[selection] = resolve_selection(cargo, selection, repo_root, args.target)
    context = context_from_capture(cargo, repo_root, args.target, resolved, source_identity)
    captures: dict[str, str] = {}
    for profile in policy["profiles"]:
        inputs = profile_inputs(profile, resolved)
        command = [
            cargo,
            "tree",
            "--locked",
            "--edges",
            ",".join(policy["edge_kinds"]),
            "--prefix",
            "none",
            "--format",
            "{p}\tfeatures:{f}",
            "-p",
            profile["package"],
        ]
        if inputs["no_default_features"]:
            command.append("--no-default-features")
        if inputs["features"]:
            command.extend(["--features", ",".join(inputs["features"])])
        if args.target:
            command.extend(["--target", args.target])
        captures[profile["id"]] = run_command(command, repo_root, f"profile {profile['id']}")
    if git_source_identity(repo_root, identity_exclusions) != source_identity:
        raise fail("git source identity: worktree changed during capture")
    atomic_write(output, build_report(policy, digest, context, captures))


def command_normalize(args: argparse.Namespace) -> None:
    policy_path = Path(args.policy).resolve()
    policy, digest = load_policy(policy_path)
    context = load_context(Path(args.context).resolve())
    captures: dict[str, str] = {}
    raw_dir = Path(args.raw_dir).resolve()
    for profile in policy["profiles"]:
        path = capture_path(raw_dir, profile["id"])
        try:
            captures[profile["id"]] = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as exc:
            raise fail(f"profile {profile['id']}: could not read capture: {exc}") from exc
    atomic_write(prepare_output(args.output), build_report(policy, digest, context, captures))


def validate_resolved_inputs(value: Any, label: str) -> None:
    if not isinstance(value, dict):
        raise fail(f"{label}: expected an object")
    expected_fields = {"mode", "no_default_features", "features", "selection"}
    reject_unknown(value, expected_fields, label)
    if set(value) != expected_fields:
        raise fail(f"{label}: missing field(s): {', '.join(sorted(expected_fields - set(value)))}")
    mode = require_string(value["mode"], f"{label}.mode")
    if mode not in {"defaults", "no-default-features", "features", "selection"}:
        raise fail(f"{label}.mode: incompatible mode {mode!r}")
    if type(value["no_default_features"]) is not bool:
        raise fail(f"{label}.no_default_features: expected a boolean")
    if value["no_default_features"] != (mode != "defaults"):
        raise fail(f"{label}: mode and no_default_features disagree")
    features = require_feature_ref_list(value["features"], f"{label}.features")
    if features != sorted(features):
        raise fail(f"{label}.features: expected sorted values")
    if mode == "features" and not features:
        raise fail(f"{label}.features: expected a non-empty list")
    if mode not in {"features", "selection"} and features:
        raise fail(f"{label}.features: incompatible with mode {mode!r}")
    if mode == "selection":
        require_string(value["selection"], f"{label}.selection", PROFILE_ID_RE)
    elif value["selection"] is not None:
        raise fail(f"{label}.selection: incompatible with mode {mode!r}")


def validate_profile(profile: Any, label: str) -> str:
    if not isinstance(profile, dict):
        raise fail(f"{label}: expected an object")
    expected_fields = {
        "id",
        "package",
        "resolved_inputs",
        "package_pairs",
        "enabled_features",
        "counts",
    }
    reject_unknown(profile, expected_fields, label)
    if set(profile) != expected_fields:
        raise fail(f"{label}: missing field(s): {', '.join(sorted(expected_fields - set(profile)))}")
    profile_id = require_string(profile["id"], f"{label}.id", PROFILE_ID_RE)
    require_string(profile["package"], f"{label}.package", PACKAGE_RE)
    validate_resolved_inputs(profile["resolved_inputs"], f"{label}.resolved_inputs")

    pairs = profile["package_pairs"]
    if not isinstance(pairs, list) or not pairs:
        raise fail(f"{label}.package_pairs: expected a non-empty array")
    normalized_pairs: list[tuple[str, str]] = []
    for index, pair in enumerate(pairs):
        pair_label = f"{label}.package_pairs[{index}]"
        if not isinstance(pair, dict) or set(pair) != {"name", "version"}:
            raise fail(f"{pair_label}: malformed package pair")
        name = require_string(pair["name"], f"{pair_label}.name", PACKAGE_RE)
        version = require_string(pair["version"], f"{pair_label}.version", VERSION_RE)
        normalized_pairs.append((name, version))
    if normalized_pairs != sorted(set(normalized_pairs)):
        raise fail(f"{label}.package_pairs: expected sorted unique pairs")
    versions_by_name: dict[str, set[str]] = {}
    for name, version in normalized_pairs:
        versions_by_name.setdefault(name, set()).add(version)
    if profile["package"] not in versions_by_name:
        raise fail(f"{label}.package_pairs: declared package {profile['package']!r} is absent")

    enabled = profile["enabled_features"]
    if not isinstance(enabled, list):
        raise fail(f"{label}.enabled_features: expected an array")
    normalized_enabled: list[tuple[str, str]] = []
    enabled_count = 0
    for index, item in enumerate(enabled):
        item_label = f"{label}.enabled_features[{index}]"
        if not isinstance(item, dict) or set(item) != {"name", "version", "features"}:
            raise fail(f"{item_label}: malformed enabled-feature record")
        name = require_string(item["name"], f"{item_label}.name", PACKAGE_RE)
        version = require_string(item["version"], f"{item_label}.version", VERSION_RE)
        features = require_string_list(item["features"], f"{item_label}.features", False)
        for feature_index, feature in enumerate(features):
            if FEATURE_NAME_RE.fullmatch(feature) is None:
                raise fail(f"{item_label}.features[{feature_index}]: malformed value {feature!r}")
        if features != sorted(features):
            raise fail(f"{item_label}.features: expected sorted values")
        if (name, version) not in normalized_pairs:
            raise fail(f"{item_label}: package pair is absent")
        normalized_enabled.append((name, version))
        enabled_count += len(features)
    if normalized_enabled != sorted(set(normalized_enabled)):
        raise fail(f"{label}.enabled_features: expected sorted unique package records")

    counts = profile["counts"]
    count_keys = {
        "package_pairs",
        "unique_package_names",
        "duplicate_version_names",
        "enabled_features",
    }
    if not isinstance(counts, dict) or set(counts) != count_keys:
        raise fail(f"{label}.counts: malformed fields")
    if any(type(counts[key]) is not int or counts[key] < 0 for key in count_keys):
        raise fail(f"{label}.counts: expected non-negative integers")
    expected_counts = {
        "package_pairs": len(normalized_pairs),
        "unique_package_names": len(versions_by_name),
        "duplicate_version_names": sum(len(versions) > 1 for versions in versions_by_name.values()),
        "enabled_features": enabled_count,
    }
    if counts != expected_counts:
        raise fail(f"{label}.counts: values do not match profile records")
    return profile_id


def validate_report(
    report: Any,
    label: str,
    expected_policy: dict[str, Any],
    expected_policy_digest: str,
) -> dict[str, Any]:
    if not isinstance(report, dict):
        raise fail(f"{label}: expected a JSON object")
    expected_fields = {"schema_version", "context", "evidence_units", "policy", "profiles", "contracts"}
    reject_unknown(report, expected_fields, label)
    if set(report) != expected_fields:
        raise fail(f"{label}: missing field(s): {', '.join(sorted(expected_fields - set(report)))}")
    if type(report["schema_version"]) is not int or report["schema_version"] != SCHEMA_VERSION:
        raise fail(f"{label}: incompatible schema version")

    context = report["context"]
    context_fields = {
        "git_revision",
        "git_dirty",
        "git_worktree_digest_sha256",
        "cargo_lock_sha256",
        "cargo_version",
        "rustc_version",
        "rustc_host",
        "target",
        "resolved_selections",
    }
    if not isinstance(context, dict) or set(context) != context_fields:
        raise fail(f"{label}.context: malformed fields")
    for key in ("git_revision", "cargo_version", "rustc_version", "rustc_host", "target"):
        require_stable_context_string(context[key], f"{label}.context.{key}")
    if re.fullmatch(r"[0-9a-fA-F]{7,64}", context["git_revision"]) is None:
        raise fail(f"{label}.context.git_revision: malformed revision")
    if type(context["git_dirty"]) is not bool:
        raise fail(f"{label}.context.git_dirty: expected a boolean")
    digest = context["git_worktree_digest_sha256"]
    if not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
        raise fail(f"{label}.context.git_worktree_digest_sha256: malformed digest")
    lock_digest = context["cargo_lock_sha256"]
    if not isinstance(lock_digest, str) or re.fullmatch(r"[0-9a-f]{64}", lock_digest) is None:
        raise fail(f"{label}.context.cargo_lock_sha256: malformed digest")
    resolved_selections = normalize_resolved_selections(
        context["resolved_selections"],
        f"{label}.context.resolved_selections",
    )
    if context["resolved_selections"] != resolved_selections:
        raise fail(f"{label}.context.resolved_selections: expected sorted feature values")
    require_policy_selection_keys(
        resolved_selections,
        expected_policy,
        f"{label}.context.resolved_selections",
    )

    if report["evidence_units"] != {
        "cargo_package_name_version_pairs": "measured",
        "binary_bytes": "not measured",
        "runtime_memory": "not measured",
    }:
        raise fail(f"{label}: malformed evidence units")
    policy_record = report["policy"]
    if not isinstance(policy_record, dict) or set(policy_record) != {"digest_sha256", "edge_kinds"}:
        raise fail(f"{label}.policy: malformed fields")
    if policy_record["edge_kinds"] != expected_policy["edge_kinds"]:
        raise fail(f"{label}.policy.edge_kinds: incompatible edge kinds")
    if policy_record["digest_sha256"] != expected_policy_digest:
        raise fail(f"{label}.policy.digest_sha256: does not match supplied policy")

    profiles = report["profiles"]
    if not isinstance(profiles, list) or not profiles:
        raise fail(f"{label}.profiles: expected a non-empty array")
    profile_ids = [validate_profile(profile, f"{label}.profiles[{index}]") for index, profile in enumerate(profiles)]
    expected_profiles = {profile["id"]: profile for profile in expected_policy["profiles"]}
    if profile_ids != sorted(expected_profiles):
        raise fail(f"{label}.profiles: ids do not match supplied policy")
    for profile in profiles:
        expected = expected_profiles[profile["id"]]
        inputs = profile["resolved_inputs"]
        if profile["package"] != expected["package"] or inputs["mode"] != expected["mode"]:
            raise fail(f"{label}.profiles.{profile['id']}: package or mode does not match supplied policy")
        if inputs["selection"] != expected["selection"]:
            raise fail(f"{label}.profiles.{profile['id']}: selection does not match supplied policy")
        if expected["mode"] == "selection":
            if inputs["features"] != resolved_selections[expected["selection"]]:
                raise fail(
                    f"{label}.profiles.{profile['id']}: features do not match captured selection"
                )
        elif inputs["features"] != sorted(expected["features"]):
            raise fail(f"{label}.profiles.{profile['id']}: features do not match supplied policy")

    contracts = report["contracts"]
    if not isinstance(contracts, list):
        raise fail(f"{label}.contracts: expected an array")
    proven_contracts = validate_contracts(
        expected_policy["contracts"],
        {profile["id"]: profile for profile in profiles},
    )
    if contracts != proven_contracts:
        raise fail(f"{label}.contracts: records do not match supplied policy")
    return report


def command_compare(args: argparse.Namespace) -> None:
    policy, policy_digest = load_policy(Path(args.policy).resolve())
    left = validate_report(
        load_json(Path(args.before).resolve(), "left report"),
        "left report",
        policy,
        policy_digest,
    )
    right = validate_report(
        load_json(Path(args.after).resolve(), "right report"),
        "right report",
        policy,
        policy_digest,
    )
    if left["schema_version"] != right["schema_version"]:
        raise fail("reports: incompatible schemas")
    if left["policy"] != right["policy"] or left["evidence_units"] != right["evidence_units"]:
        raise fail("reports: incompatible policy or evidence units")
    compatibility_context = ("cargo_version", "rustc_version", "rustc_host", "target")
    if any(left["context"][key] != right["context"][key] for key in compatibility_context):
        raise fail("reports: incompatible toolchain or target context")
    if left["contracts"] != right["contracts"]:
        raise fail("reports: incompatible contract results")
    left_profiles = {profile["id"]: profile for profile in left["profiles"]}
    right_profiles = {profile["id"]: profile for profile in right["profiles"]}
    if set(left_profiles) != set(right_profiles):
        raise fail("reports: incompatible profile sets")
    deltas: list[dict[str, Any]] = []
    for profile_id in sorted(left_profiles):
        before = left_profiles[profile_id]
        after = right_profiles[profile_id]
        if before["package"] != after["package"] or before["resolved_inputs"] != after["resolved_inputs"]:
            raise fail(f"reports: incompatible inputs for profile {profile_id}")
        before_pairs = {(item["name"], item["version"]) for item in before["package_pairs"]}
        after_pairs = {(item["name"], item["version"]) for item in after["package_pairs"]}
        before_features = {
            (item["name"], item["version"]): set(item["features"])
            for item in before["enabled_features"]
        }
        after_features = {
            (item["name"], item["version"]): set(item["features"])
            for item in after["enabled_features"]
        }
        feature_deltas = [
            {
                "name": name,
                "version": version,
                "added_features": sorted(
                    after_features.get((name, version), set())
                    - before_features.get((name, version), set())
                ),
                "removed_features": sorted(
                    before_features.get((name, version), set())
                    - after_features.get((name, version), set())
                ),
            }
            for name, version in sorted(before_pairs & after_pairs)
            if before_features.get((name, version), set()) != after_features.get((name, version), set())
        ]
        before_counts = before.get("counts")
        after_counts = after.get("counts")
        if not isinstance(before_counts, dict) or not isinstance(after_counts, dict):
            raise fail(f"reports: profile {profile_id} has malformed counts")
        deltas.append(
            {
                "id": profile_id,
                "added_package_pairs": [
                    {"name": name, "version": version}
                    for name, version in sorted(after_pairs - before_pairs)
                ],
                "removed_package_pairs": [
                    {"name": name, "version": version}
                    for name, version in sorted(before_pairs - after_pairs)
                ],
                "feature_deltas": feature_deltas,
                "count_deltas": {
                    key: after_counts[key] - before_counts[key]
                    for key in (
                        "package_pairs",
                        "unique_package_names",
                        "duplicate_version_names",
                        "enabled_features",
                    )
                },
            }
        )
    result = {
        "schema_version": SCHEMA_VERSION,
        "context": {
            "before": left["context"],
            "after": right["context"],
            "changed_fields": sorted(
                key for key in left["context"] if left["context"][key] != right["context"][key]
            ),
        },
        "policy": left["policy"],
        "profiles": deltas,
    }
    if args.output:
        atomic_write(prepare_output(args.output), result)
    else:
        sys.stdout.write(json.dumps(result, ensure_ascii=True, indent=2, sort_keys=True) + "\n")


def parser() -> argparse.ArgumentParser:
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    capture = commands.add_parser("capture")
    capture.add_argument("--policy", default=str(SCRIPT_ROOT / "dev/ci/dependency-footprint.toml"))
    capture.add_argument("--output", required=True)
    capture.add_argument("--repo-root")
    capture.add_argument("--target")
    capture.add_argument("--cargo", default=os.environ.get("CARGO", "cargo"))
    capture.set_defaults(function=command_capture)
    normalize = commands.add_parser("normalize")
    normalize.add_argument("--policy", required=True)
    normalize.add_argument("--raw-dir", required=True)
    normalize.add_argument("--context", required=True)
    normalize.add_argument("--output", required=True)
    normalize.set_defaults(function=command_normalize)
    compare = commands.add_parser("compare")
    compare.add_argument("before")
    compare.add_argument("after")
    compare.add_argument("--policy", default=str(SCRIPT_ROOT / "dev/ci/dependency-footprint.toml"))
    compare.add_argument("--output")
    compare.set_defaults(function=command_compare)
    return root


def main(argv: list[str] | None = None) -> int:
    try:
        args = parser().parse_args(argv)
        args.function(args)
    except ToolError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
