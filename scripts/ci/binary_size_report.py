#!/usr/bin/env python3
"""Measure release binary bytes per dependency-footprint policy profile and compare two reports."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shlex
import stat
import subprocess
import sys
from pathlib import Path
from typing import Any, TextIO

if sys.version_info < (3, 11):
    raise SystemExit("error: binary_size_report.py requires Python 3.11 or newer (tomllib)")

import tomllib

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

from dependency_footprint import (
    PACKAGE_RE,
    PROFILE_ID_RE,
    SCRIPT_ROOT,
    PreparedOutput,
    ToolError,
    atomic_write,
    context_from_capture,
    fail,
    git_source_identity,
    load_json,
    load_policy,
    normalize_resolved_selections,
    output_identity_exclusions,
    prepare_output,
    profile_inputs,
    reject_unknown,
    require_stable_context_string,
    require_string,
    resolve_selection,
    validate_resolved_inputs,
)


SCHEMA_VERSION = 1
REPORT_KIND = "binary_size"
COMPARISON_KIND = "binary_size_comparison"
PACKAGE = "zeroclaw"
BIN = "zeroclaw"
CARGO_PROFILE = "release"
MIB = 1024 * 1024
MAX_BYTES = 2**63 - 1
MAX_SETTINGS_TABLE_DEPTH = 3
TARGET_RE = re.compile(r"[A-Za-z0-9_][A-Za-z0-9_.-]*")
REVISION_RE = re.compile(r"[0-9a-fA-F]{7,64}")
DIGEST_RE = re.compile(r"[0-9a-f]{64}")
EVIDENCE_UNITS = {
    "binary_bytes": "measured",
    "cargo_package_name_version_pairs": "not measured",
    "runtime_memory": "not measured",
}
CONTEXT_FIELDS = frozenset(
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
        "build_env",
    }
)
TOOLCHAIN_FIELDS = ("cargo_version", "rustc_version", "rustc_host", "target")
REPORT_FIELDS = frozenset(
    {
        "schema_version",
        "kind",
        "context",
        "evidence_units",
        "policy",
        "cargo_profile",
        "bin",
        "measurements",
    }
)
MEASUREMENT_FIELDS = frozenset(
    {"id", "package", "resolved_inputs", "target_triple", "path", "bytes", "sha256"}
)
CARGO_PROFILE_FIELDS = frozenset({"name", "settings"})
REFUSED_COMPILER_OVERRIDES = ("RUSTC", "CARGO_BUILD_RUSTC")


def require_fields(value: Any, fields: frozenset[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise fail(f"{label}: expected an object")
    reject_unknown(value, set(fields), label)
    missing = sorted(fields - set(value))
    if missing:
        raise fail(f"{label}: missing field(s): {', '.join(missing)}")
    return value


def require_digest(value: Any, label: str) -> str:
    if not isinstance(value, str) or DIGEST_RE.fullmatch(value) is None:
        raise fail(f"{label}: malformed digest")
    return value


def resolve_path(raw: str | Path, label: str) -> Path:
    # Python 3.11 and 3.12 raise RuntimeError instead of OSError on a symlink loop.
    try:
        return Path(raw).resolve()
    except (OSError, RuntimeError) as exc:
        raise fail(f"{label}: could not resolve {raw}: {exc}") from exc


def prepare_report_output(raw: str) -> PreparedOutput:
    try:
        is_directory = Path(raw).is_dir()
    except OSError as exc:
        raise fail(f"--output: could not inspect {raw}: {exc}") from exc
    if is_directory:
        raise fail(f"--output: {raw} is a directory")
    try:
        return prepare_output(raw)
    except RuntimeError as exc:
        raise fail(f"--output: could not resolve {raw}: {exc}") from exc


def write_report(output: PreparedOutput, value: Any) -> None:
    try:
        atomic_write(output, value)
    except RuntimeError as exc:
        raise fail(f"could not write {output.path}: {exc}") from exc


def read_policy(raw: str) -> tuple[dict[str, Any], str]:
    path = resolve_path(raw, "--policy")
    try:
        return load_policy(path)
    except RecursionError as exc:
        raise fail(f"could not read policy {path}: nesting is too deep") from exc


def read_report(raw: str, label: str) -> Any:
    path = resolve_path(raw, label)
    try:
        return load_json(path, label)
    except RecursionError as exc:
        raise fail(f"could not read {label} {path}: nesting is too deep") from exc


def require_settings_table(value: Any, label: str, depth: int = 1) -> None:
    if not isinstance(value, dict):
        raise fail(f"{label}: expected a table")
    if depth > MAX_SETTINGS_TABLE_DEPTH:
        raise fail(f"{label}: Cargo profile tables nest at most {MAX_SETTINGS_TABLE_DEPTH} levels")
    for key, item in value.items():
        if not isinstance(key, str) or not key:
            raise fail(f"{label}: expected non-empty string keys")
        item_label = f"{label}.{key}"
        if isinstance(item, dict):
            require_settings_table(item, item_label, depth + 1)
        elif isinstance(item, list):
            for index, element in enumerate(item):
                if not isinstance(element, (str, int)):
                    raise fail(f"{item_label}[{index}]: unsupported value type {type(element).__name__}")
        elif not isinstance(item, (str, int)):
            raise fail(f"{item_label}: unsupported value type {type(item).__name__}")


def release_profile_identity(repo_root: Path) -> tuple[dict[str, Any], list[str]]:
    """Return the release Cargo profile identity and the names of the other declared Cargo profiles."""
    manifest_path = repo_root / "Cargo.toml"
    try:
        manifest = tomllib.loads(manifest_path.read_bytes().decode("utf-8"))
    except (OSError, UnicodeDecodeError, tomllib.TOMLDecodeError, RecursionError) as exc:
        raise fail(f"could not read {manifest_path}: {exc}") from exc
    tables = manifest.get("profile", {})
    if not isinstance(tables, dict):
        raise fail(f"{manifest_path}: [profile] is not a table")
    # Cargo refuses `inherits` in its built-in release profile, so this table
    # holds every manifest override applied on top of Cargo's release defaults.
    settings = tables.get(CARGO_PROFILE, {})
    require_settings_table(settings, f"{manifest_path}: profile.{CARGO_PROFILE}")
    other_profiles = sorted(name for name in tables if name != CARGO_PROFILE)
    return {"name": CARGO_PROFILE, "settings": settings}, other_profiles


def cargo_profile_env_prefix(profile: str) -> str:
    return f"CARGO_PROFILE_{profile.upper().replace('-', '_')}_"


def reject_environment_overrides(other_profiles: list[str]) -> None:
    compiler_overrides = ", ".join(name for name in REFUSED_COMPILER_OVERRIDES if name in os.environ)
    if compiler_overrides:
        raise fail(
            f"{compiler_overrides} set: the report records the rustc found on PATH; "
            f"unset {compiler_overrides} before measuring"
        )
    other_prefixes = tuple(cargo_profile_env_prefix(name) for name in other_profiles)
    profile_overrides = sorted(
        name
        for name in os.environ
        if name.startswith(cargo_profile_env_prefix(CARGO_PROFILE)) and not name.startswith(other_prefixes)
    )
    if profile_overrides:
        raise fail(
            f"environment overrides the {CARGO_PROFILE} Cargo profile ({', '.join(profile_overrides)}); "
            "the report records Cargo.toml, so unset them before measuring"
        )


def build_env_names(triple: str) -> tuple[str, ...]:
    target_key = re.sub(r"[-.]", "_", triple.upper())
    return (
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_BUILD_RUSTFLAGS",
        f"CARGO_TARGET_{target_key}_RUSTFLAGS",
        f"CARGO_TARGET_{target_key}_LINKER",
        "ZEROCLAW_BUILD_ID",
    )


def require_ignored_target_dir(repo_root: Path, directory: Path) -> None:
    try:
        relative = resolve_path(directory, "--target-dir").relative_to(repo_root)
    except ValueError:
        return
    if not relative.parts:
        raise fail("--target-dir: a policy profile directory must not be the repository root")
    pathspec = f"{relative.as_posix()}/"
    try:
        completed = subprocess.run(
            ["git", "check-ignore", "-q", "--", pathspec],
            cwd=repo_root,
            capture_output=True,
            check=False,
        )
    except OSError as exc:
        raise fail(f"--target-dir: could not start git check-ignore: {exc}") from exc
    if completed.returncode == 1:
        raise fail(
            f"--target-dir: {pathspec} is inside the repository but not ignored by Git; "
            "build outputs there would change the recorded source identity"
        )
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", errors="replace").strip() or "no output"
        raise fail(f"--target-dir: git check-ignore failed with status {completed.returncode}: {detail}")


def select_profiles(policy: dict[str, Any], requested: list[str] | None) -> list[dict[str, Any]]:
    if not requested:
        selected = [profile for profile in policy["profiles"] if profile["package"] == PACKAGE]
        if not selected:
            raise fail(f"policy: no policy profile builds package {PACKAGE!r}")
        return selected
    for profile_id in requested:
        require_string(profile_id, "--profile", PROFILE_ID_RE)
    duplicates = sorted({profile_id for profile_id in requested if requested.count(profile_id) > 1})
    if duplicates:
        raise fail(f"--profile: duplicate policy profile id(s): {', '.join(duplicates)}")
    by_id = {profile["id"]: profile for profile in policy["profiles"]}
    unknown = sorted(set(requested) - set(by_id))
    if unknown:
        raise fail(f"--profile: unknown policy profile id(s) for the supplied policy: {', '.join(unknown)}")
    foreign = sorted(
        f"{profile_id} (package {by_id[profile_id]['package']})"
        for profile_id in set(requested)
        if by_id[profile_id]["package"] != PACKAGE
    )
    if foreign:
        raise fail(
            f"--profile: only policy profiles of package {PACKAGE} build the {BIN} binary; "
            f"refusing {', '.join(foreign)}"
        )
    return [profile for profile in policy["profiles"] if profile["id"] in requested]


def binary_relative_path(profile_id: str, explicit_target: str | None, triple: str) -> str:
    parts = [profile_id]
    if explicit_target is not None:
        parts.append(explicit_target)
    parts.append(CARGO_PROFILE)
    parts.append(f"{BIN}.exe" if "-windows" in triple else BIN)
    return "/".join(parts)


def build_command(
    cargo: str,
    inputs: dict[str, Any],
    profile_target_dir: Path,
    target: str | None,
) -> list[str]:
    command = [
        cargo,
        "build",
        "--release",
        "--locked",
        "--message-format=json-render-diagnostics",
        "-p",
        PACKAGE,
        "--bin",
        BIN,
        "--target-dir",
        str(profile_target_dir),
    ]
    if inputs["no_default_features"]:
        command.append("--no-default-features")
    if inputs["features"]:
        command.extend(["--features", ",".join(inputs["features"])])
    if target:
        command.extend(["--target", target])
    return command


def run_build(command: list[str], repo_root: Path, profile_id: str) -> bytes:
    # Release builds take minutes: Cargo's progress and rendered diagnostics
    # stream to stderr, while stdout carries the JSON artifact messages.
    print(f"building policy profile {profile_id}: {shlex.join(command)}", file=sys.stderr, flush=True)
    try:
        completed = subprocess.run(command, cwd=repo_root, stdout=subprocess.PIPE, check=False)
    except OSError as exc:
        raise fail(f"policy profile {profile_id}: could not start cargo build: {exc}") from exc
    if completed.returncode != 0:
        raise fail(f"policy profile {profile_id}: cargo build failed with status {completed.returncode}")
    return completed.stdout


def reported_executable(stdout: bytes, profile_id: str) -> Path:
    executables: set[str] = set()
    for line in stdout.splitlines():
        try:
            message = json.loads(line)
        except (ValueError, RecursionError):
            continue
        if not isinstance(message, dict) or message.get("reason") != "compiler-artifact":
            continue
        target = message.get("target")
        if not isinstance(target, dict) or target.get("name") != BIN:
            continue
        kinds = target.get("kind")
        if not isinstance(kinds, list) or "bin" not in kinds:
            continue
        executable = message.get("executable")
        if not isinstance(executable, str) or not executable:
            raise fail(f"policy profile {profile_id}: cargo reported the {BIN} binary without an executable path")
        executables.add(executable)
    if not executables:
        raise fail(f"policy profile {profile_id}: cargo did not report a {BIN} binary")
    if len(executables) > 1:
        raise fail(
            f"policy profile {profile_id}: cargo reported several {BIN} binaries: {', '.join(sorted(executables))}"
        )
    return Path(executables.pop())


def measure_binary(path: Path, profile_id: str) -> tuple[int, str]:
    try:
        handle = path.open("rb")
    except FileNotFoundError as exc:
        raise fail(f"policy profile {profile_id}: binary not found at {path}") from exc
    except OSError as exc:
        raise fail(f"policy profile {profile_id}: could not open binary {path}: {exc}") from exc
    with handle:
        try:
            metadata = os.fstat(handle.fileno())
            if not stat.S_ISREG(metadata.st_mode):
                raise fail(f"policy profile {profile_id}: binary is not a regular file: {path}")
            digest = hashlib.sha256()
            read = 0
            while chunk := handle.read(1 << 20):
                digest.update(chunk)
                read += len(chunk)
        except OSError as exc:
            raise fail(f"policy profile {profile_id}: could not read binary {path}: {exc}") from exc
    if read != metadata.st_size:
        raise fail(f"policy profile {profile_id}: binary changed while it was measured: {path}")
    return metadata.st_size, digest.hexdigest()


def validate_build_env(value: Any, label: str, triple: str) -> None:
    if not isinstance(value, dict):
        raise fail(f"{label}: expected an object")
    unexpected = sorted(set(value) - set(build_env_names(triple)))
    if unexpected:
        raise fail(f"{label}: unexpected variable(s) for target {triple}: {', '.join(unexpected)}")
    for name, item in value.items():
        if not isinstance(item, str):
            raise fail(f"{label}.{name}: expected a string")


def validate_context(context: Any, label: str) -> dict[str, list[str]]:
    require_fields(context, CONTEXT_FIELDS, label)
    for key in ("git_revision", "cargo_version", "rustc_version", "rustc_host", "target"):
        require_stable_context_string(context[key], f"{label}.{key}")
    if REVISION_RE.fullmatch(context["git_revision"]) is None:
        raise fail(f"{label}.git_revision: malformed revision")
    if type(context["git_dirty"]) is not bool:
        raise fail(f"{label}.git_dirty: expected a boolean")
    for key in ("git_worktree_digest_sha256", "cargo_lock_sha256"):
        require_digest(context[key], f"{label}.{key}")
    for key in ("rustc_host", "target"):
        require_string(context[key], f"{label}.{key}", TARGET_RE)
    validate_build_env(context["build_env"], f"{label}.build_env", context["target"])
    selections = normalize_resolved_selections(
        context["resolved_selections"],
        f"{label}.resolved_selections",
    )
    if context["resolved_selections"] != selections:
        raise fail(f"{label}.resolved_selections: expected sorted feature values")
    return selections


def validate_cargo_profile(value: Any, label: str) -> None:
    require_fields(value, CARGO_PROFILE_FIELDS, label)
    if value["name"] != CARGO_PROFILE:
        raise fail(f"{label}.name: expected {CARGO_PROFILE!r}")
    require_settings_table(value["settings"], f"{label}.settings")


def validate_measurement(
    measurement: Any,
    label: str,
    policy_profiles: dict[str, dict[str, Any]],
    selections: dict[str, list[str]],
    context: dict[str, Any],
) -> str:
    require_fields(measurement, MEASUREMENT_FIELDS, label)
    profile_id = require_string(measurement["id"], f"{label}.id", PROFILE_ID_RE)
    expected = policy_profiles.get(profile_id)
    if expected is None:
        raise fail(f"{label}.id: unknown policy profile {profile_id!r} for the supplied policy")
    if expected["package"] != PACKAGE:
        raise fail(
            f"{label}.id: policy profile {profile_id!r} builds package {expected['package']!r}, not {PACKAGE!r}"
        )
    package = require_string(measurement["package"], f"{label}.package", PACKAGE_RE)
    if package != expected["package"]:
        raise fail(f"{label}.package: does not match the supplied policy")
    validate_resolved_inputs(measurement["resolved_inputs"], f"{label}.resolved_inputs")
    inputs = measurement["resolved_inputs"]
    if inputs["mode"] != expected["mode"]:
        raise fail(f"{label}.resolved_inputs.mode: does not match the supplied policy")
    if inputs["selection"] != expected["selection"]:
        raise fail(f"{label}.resolved_inputs.selection: does not match the supplied policy")
    if expected["mode"] == "selection":
        if expected["selection"] not in selections:
            raise fail(f"{label}: selection {expected['selection']!r} is missing from context.resolved_selections")
        if inputs["features"] != selections[expected["selection"]]:
            raise fail(f"{label}.resolved_inputs.features: do not match the resolved selection")
    elif inputs["features"] != sorted(expected["features"]):
        raise fail(f"{label}.resolved_inputs.features: do not match the supplied policy")
    triple = context["target"]
    if measurement["target_triple"] != triple:
        raise fail(f"{label}.target_triple: does not match context.target")
    path = require_string(measurement["path"], f"{label}.path")
    allowed_paths = {binary_relative_path(profile_id, triple, triple)}
    if triple == context["rustc_host"]:
        allowed_paths.add(binary_relative_path(profile_id, None, triple))
    if path not in allowed_paths:
        raise fail(f"{label}.path: does not match the policy profile, target, and {BIN} binary")
    size = measurement["bytes"]
    if type(size) is not int or not 0 <= size <= MAX_BYTES:
        raise fail(f"{label}.bytes: expected an integer from 0 to {MAX_BYTES}")
    require_digest(measurement["sha256"], f"{label}.sha256")
    return profile_id


def validate_report(
    report: Any,
    label: str,
    policy: dict[str, Any],
    policy_digest: str,
) -> dict[str, Any]:
    require_fields(report, REPORT_FIELDS, label)
    if type(report["schema_version"]) is not int or report["schema_version"] != SCHEMA_VERSION:
        raise fail(f"{label}: incompatible schema version")
    if report["kind"] != REPORT_KIND:
        raise fail(f"{label}.kind: expected {REPORT_KIND!r}")
    context = report["context"]
    selections = validate_context(context, f"{label}.context")
    if report["evidence_units"] != EVIDENCE_UNITS:
        raise fail(f"{label}: malformed evidence units")
    policy_record = require_fields(report["policy"], frozenset({"digest_sha256", "edge_kinds"}), f"{label}.policy")
    require_digest(policy_record["digest_sha256"], f"{label}.policy.digest_sha256")
    if policy_record["digest_sha256"] != policy_digest:
        raise fail(f"{label}.policy.digest_sha256: does not match the supplied policy")
    if policy_record["edge_kinds"] != policy["edge_kinds"]:
        raise fail(f"{label}.policy.edge_kinds: incompatible edge kinds")
    validate_cargo_profile(report["cargo_profile"], f"{label}.cargo_profile")
    if report["bin"] != BIN:
        raise fail(f"{label}.bin: expected {BIN!r}")
    measurements = report["measurements"]
    if not isinstance(measurements, list) or not measurements:
        raise fail(f"{label}.measurements: expected a non-empty array")
    policy_profiles = {profile["id"]: profile for profile in policy["profiles"]}
    ids = [
        validate_measurement(measurement, f"{label}.measurements[{index}]", policy_profiles, selections, context)
        for index, measurement in enumerate(measurements)
    ]
    if len(set(ids)) != len(ids):
        raise fail(f"{label}.measurements: duplicate policy profile ids")
    if ids != sorted(ids):
        raise fail(f"{label}.measurements: expected measurements sorted by id")
    used_selections = {policy_profiles[profile_id]["selection"] for profile_id in ids} - {None}
    unused = sorted(set(selections) - used_selections)
    if unused:
        raise fail(
            f"{label}.context.resolved_selections: selection(s) not used by any measured policy profile: "
            f"{', '.join(unused)}"
        )
    return report


def delta_percent(delta: int, before: int) -> float | None:
    if before == 0:
        return None
    value = round(delta * 100 / before, 2)
    return 0.0 if value == 0 else value


def changed_inputs(old: dict[str, Any], new: dict[str, Any]) -> dict[str, Any]:
    changes: dict[str, Any] = {}
    old_inputs = old["resolved_inputs"]
    new_inputs = new["resolved_inputs"]
    if old_inputs["features"] != new_inputs["features"]:
        changes["features"] = {
            "added": sorted(set(new_inputs["features"]) - set(old_inputs["features"])),
            "removed": sorted(set(old_inputs["features"]) - set(new_inputs["features"])),
        }
    for key in sorted(set(old_inputs) - {"features"}):
        if old_inputs[key] != new_inputs[key]:
            changes[key] = {"before": old_inputs[key], "after": new_inputs[key]}
    for key in ("package", "path"):
        if old[key] != new[key]:
            changes[key] = {"before": old[key], "after": new[key]}
    return changes


def describe_changes(changes: dict[str, Any]) -> str:
    parts = []
    for key, change in changes.items():
        if key == "features":
            names = [f"+{name}" for name in change["added"]] + [f"-{name}" for name in change["removed"]]
            parts.append(f"features {' '.join(names)}")
        else:
            parts.append(f"{key} {change['before']} -> {change['after']}")
    return "; ".join(parts)


def compare_measurement(old: dict[str, Any], new: dict[str, Any]) -> dict[str, Any]:
    changes = changed_inputs(old, new)
    comparable = not changes
    delta = new["bytes"] - old["bytes"]
    return {
        "id": old["id"],
        "comparable": comparable,
        "changed_inputs": changes,
        "before_bytes": old["bytes"],
        "after_bytes": new["bytes"],
        "delta_bytes": delta if comparable else None,
        "delta_percent": delta_percent(delta, old["bytes"]) if comparable else None,
        "before_sha256": old["sha256"],
        "after_sha256": new["sha256"],
    }


def write_measure_table(measurements: list[dict[str, Any]], stream: TextIO) -> None:
    width = max([len("profile"), *(len(item["id"]) for item in measurements)])
    stream.write(f"{'profile':<{width}}  {'bytes':>12}  {'MiB':>8}  sha256\n")
    for item in measurements:
        stream.write(
            f"{item['id']:<{width}}  {item['bytes']:>12}  {item['bytes'] / MIB:>8.2f}  {item['sha256'][:12]}\n"
        )


def write_compare_table(result: dict[str, Any], stream: TextIO) -> None:
    for side in ("before", "after"):
        context = result["context"][side]
        state = "dirty" if context["git_dirty"] else "clean"
        stream.write(f"{side:<6}  {context['git_revision'][:12]}  {state}\n")
    rows = result["measurements"]
    width = max([len("profile"), *(len(row["id"]) for row in rows)])
    stream.write(
        f"{'profile':<{width}}  {'before':>12}  {'after':>12}  {'delta':>11}  {'delta %':>8}  binary\n"
    )
    for row in rows:
        prefix = f"{row['id']:<{width}}  {row['before_bytes']:>12}  {row['after_bytes']:>12}  "
        if not row["comparable"]:
            stream.write(
                f"{prefix}{'-':>11}  {'-':>8}  not comparable: {describe_changes(row['changed_inputs'])}\n"
            )
            continue
        percent = "n/a" if row["delta_percent"] is None else f"{row['delta_percent']:+.2f}%"
        binary = "same" if row["before_sha256"] == row["after_sha256"] else "changed"
        stream.write(f"{prefix}{row['delta_bytes']:>+11}  {percent:>8}  {binary}\n")


def command_measure(args: argparse.Namespace) -> None:
    output = prepare_report_output(args.output)
    policy, policy_digest = read_policy(args.policy)
    repo_root = resolve_path(args.repo_root, "--repo-root") if args.repo_root else SCRIPT_ROOT
    cargo = require_string(args.cargo, "--cargo")
    target = None if args.target is None else require_string(args.target, "--target", TARGET_RE)
    target_root = resolve_path(args.target_dir, "--target-dir")
    profiles = select_profiles(policy, args.profiles)
    cargo_profile, other_profiles = release_profile_identity(repo_root)
    reject_environment_overrides(other_profiles)
    for profile in profiles:
        require_ignored_target_dir(repo_root, target_root / profile["id"])
    identity_exclusions = output_identity_exclusions(repo_root, output.parent / output.path.name)
    source_identity = git_source_identity(repo_root, identity_exclusions)
    if source_identity["git_dirty"]:
        print(
            f"warning: {repo_root} has uncommitted or untracked changes; the report marks the source as dirty",
            file=sys.stderr,
            flush=True,
        )
    resolved: dict[str, list[str]] = {}
    for profile in profiles:
        selection = profile["selection"]
        if selection and selection not in resolved:
            print(
                f"resolving feature selection {selection} (builds xtask on first use)",
                file=sys.stderr,
                flush=True,
            )
            resolved[selection] = resolve_selection(cargo, selection, repo_root, target)
    context = context_from_capture(cargo, repo_root, target, resolved, source_identity)
    context["resolved_selections"] = normalize_resolved_selections(resolved, "resolved selections")
    triple = require_string(context["target"], "target triple", TARGET_RE)
    context["build_env"] = {name: os.environ[name] for name in build_env_names(triple) if name in os.environ}
    measurements: list[dict[str, Any]] = []
    for profile in profiles:
        inputs = profile_inputs(profile, resolved)
        # Each policy profile builds into its own directory under --target-dir.
        # The policy profiles differ in features and the release profile links
        # with fat LTO, so every policy profile is a separate whole-program
        # link; separate directories keep each binary at its own stable path
        # after the run, where one shared directory would hold only the last.
        command = build_command(cargo, inputs, target_root / profile["id"], target)
        stdout = run_build(command, repo_root, profile["id"])
        relative = binary_relative_path(profile["id"], target, triple)
        expected_path = target_root / relative
        observed_path = reported_executable(stdout, profile["id"])
        if observed_path != expected_path:
            raise fail(
                f"policy profile {profile['id']}: cargo built {BIN} at {observed_path}, expected {expected_path}; "
                "a build target from CARGO_BUILD_TARGET or a build.target setting needs an explicit --target"
            )
        size, digest = measure_binary(observed_path, profile["id"])
        measurements.append(
            {
                "id": profile["id"],
                "package": profile["package"],
                "resolved_inputs": inputs,
                "target_triple": triple,
                "path": relative,
                "bytes": size,
                "sha256": digest,
            }
        )
    if git_source_identity(repo_root, identity_exclusions) != source_identity:
        raise fail("git source identity: worktree changed during measurement")
    report = {
        "schema_version": SCHEMA_VERSION,
        "kind": REPORT_KIND,
        "context": context,
        "evidence_units": dict(EVIDENCE_UNITS),
        "policy": {"digest_sha256": policy_digest, "edge_kinds": policy["edge_kinds"]},
        "cargo_profile": cargo_profile,
        "bin": BIN,
        "measurements": sorted(measurements, key=lambda item: item["id"]),
    }
    validate_report(report, "measured report", policy, policy_digest)
    write_report(output, report)
    write_measure_table(report["measurements"], sys.stdout)


def command_compare(args: argparse.Namespace) -> None:
    policy, policy_digest = read_policy(args.policy)
    output = prepare_report_output(args.output) if args.output else None
    before = validate_report(read_report(args.before, "before report"), "before report", policy, policy_digest)
    after = validate_report(read_report(args.after, "after report"), "after report", policy, policy_digest)
    toolchain = [key for key in TOOLCHAIN_FIELDS if before["context"][key] != after["context"][key]]
    if toolchain:
        raise fail(f"reports: incompatible toolchain or target context (differing: {', '.join(toolchain)})")
    before_env = before["context"]["build_env"]
    after_env = after["context"]["build_env"]
    environment = sorted(
        name for name in set(before_env) | set(after_env) if before_env.get(name) != after_env.get(name)
    )
    if environment:
        raise fail(f"reports: incompatible build environment (differing: {', '.join(environment)})")
    if before["cargo_profile"] != after["cargo_profile"]:
        raise fail(f"reports: incompatible Cargo profile ({CARGO_PROFILE} settings differ)")
    before_items = {item["id"]: item for item in before["measurements"]}
    after_items = {item["id"]: item for item in after["measurements"]}
    if set(before_items) != set(after_items):
        details = []
        only_before = sorted(set(before_items) - set(after_items))
        only_after = sorted(set(after_items) - set(before_items))
        if only_before:
            details.append(f"only before: {', '.join(only_before)}")
        if only_after:
            details.append(f"only after: {', '.join(only_after)}")
        raise fail(f"reports: incompatible policy profile sets ({'; '.join(details)})")
    rows = [
        compare_measurement(before_items[profile_id], after_items[profile_id])
        for profile_id in sorted(before_items)
    ]
    result = {
        "schema_version": SCHEMA_VERSION,
        "kind": COMPARISON_KIND,
        "context": {
            "before": before["context"],
            "after": after["context"],
            "changed_fields": sorted(
                key for key in before["context"] if before["context"][key] != after["context"][key]
            ),
        },
        "policy": before["policy"],
        "cargo_profile": before["cargo_profile"],
        "bin": BIN,
        "measurements": rows,
    }
    if output is not None:
        write_report(output, result)
        write_compare_table(result, sys.stdout)
    else:
        sys.stdout.write(json.dumps(result, ensure_ascii=True, indent=2, sort_keys=True) + "\n")
        write_compare_table(result, sys.stderr)
    not_comparable = [row["id"] for row in rows if not row["comparable"]]
    if not_comparable:
        print(
            f"note: no delta for policy profile(s) {', '.join(not_comparable)}: "
            "their resolved inputs or binary paths differ between the reports",
            file=sys.stderr,
        )


def parser() -> argparse.ArgumentParser:
    default_policy = str(SCRIPT_ROOT / "dev/ci/dependency-footprint.toml")
    root = argparse.ArgumentParser(description=__doc__)
    commands = root.add_subparsers(dest="command", required=True)
    measure = commands.add_parser(
        "measure",
        help=f"build each policy profile with the {CARGO_PROFILE} Cargo profile and record its binary",
        description=(
            f"Build the {BIN} binary of each selected policy profile with the {CARGO_PROFILE} Cargo profile "
            "and record its bytes and SHA-256."
        ),
    )
    measure.add_argument(
        "--policy",
        default=default_policy,
        help="footprint policy that defines the policy profiles (default: this repository's policy)",
    )
    measure.add_argument("--output", required=True, help="path of the JSON report to write")
    measure.add_argument("--repo-root", help="source tree to build (default: this repository)")
    measure.add_argument("--target", help="target triple to build for (default: the rustc host)")
    measure.add_argument(
        "--target-dir",
        required=True,
        help="root build directory; each policy profile builds in <target-dir>/<profile-id>",
    )
    measure.add_argument(
        "--profile",
        action="append",
        dest="profiles",
        metavar="ID",
        help=f"policy profile to measure, repeatable (default: every {PACKAGE} policy profile)",
    )
    measure.add_argument(
        "--cargo",
        default=os.environ.get("CARGO", "cargo"),
        help="cargo command (default: $CARGO or cargo)",
    )
    measure.set_defaults(function=command_measure)
    compare = commands.add_parser(
        "compare",
        help="compare two measure reports",
        description="Compare two measure reports built with the same toolchain, target, and build environment.",
    )
    compare.add_argument("before", help="report measured first")
    compare.add_argument("after", help="report measured second")
    compare.add_argument(
        "--policy",
        default=default_policy,
        help="footprint policy both reports must match (default: this repository's policy)",
    )
    compare.add_argument("--output", help="write the comparison JSON here instead of stdout")
    compare.set_defaults(function=command_compare)
    return root


def main(argv: list[str] | None = None) -> int:
    if os.name == "nt":
        print("error: binary_size_report.py runs on Linux or macOS only", file=sys.stderr)
        return 1
    try:
        args = parser().parse_args(argv)
        args.function(args)
    except ToolError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
