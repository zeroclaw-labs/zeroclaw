#!/usr/bin/env python3
"""Produce a reproducible unsafe debt audit report for Rust source files."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass
from pathlib import Path

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:
    import tomli as tomllib  # type: ignore


@dataclass(frozen=True)
class PatternSpec:
    id: str
    category: str
    severity: str
    description: str
    regex: re.Pattern[str]


PATTERNS: tuple[PatternSpec, ...] = (
    PatternSpec(
        id="unsafe_block",
        category="unsafe",
        severity="high",
        description="Unsafe block expression (`unsafe { ... }`).",
        regex=re.compile(r"\bunsafe\s*\{"),
    ),
    PatternSpec(
        id="unsafe_fn",
        category="unsafe",
        severity="high",
        description="Unsafe function declaration (`unsafe fn ...`).",
        regex=re.compile(r"\bunsafe\s+fn\b"),
    ),
    PatternSpec(
        id="unsafe_impl",
        category="unsafe",
        severity="high",
        description="Unsafe impl declaration (`unsafe impl ...`).",
        regex=re.compile(r"\bunsafe\s+impl\b"),
    ),
    PatternSpec(
        id="unsafe_trait",
        category="unsafe",
        severity="high",
        description="Unsafe trait declaration (`unsafe trait ...`).",
        regex=re.compile(r"\bunsafe\s+trait\b"),
    ),
    PatternSpec(
        id="mem_transmute",
        category="risky",
        severity="high",
        description="Memory transmute usage.",
        regex=re.compile(r"\b(?:std|core)::mem::transmute(?:_copy)?\b"),
    ),
    PatternSpec(
        id="slice_from_raw_parts",
        category="risky",
        severity="high",
        description="Raw slice construction from raw parts.",
        regex=re.compile(r"\b(?:std|core)::slice::from_raw_parts(?:_mut)?\b"),
    ),
    PatternSpec(
        id="ffi_libc_call",
        category="risky",
        severity="medium",
        description="Direct libc symbol usage.",
        regex=re.compile(r"\blibc::[A-Za-z_][A-Za-z0-9_]*\b"),
    ),
)

DEFAULT_INCLUDE_PATHS: tuple[str, ...] = ("src", "crates", "tests", "benches", "fuzz")
CRATE_UNSAFE_GUARD_RE = re.compile(r"#!\s*\[(?:forbid|deny)\s*\(\s*unsafe_code\s*\)\s*\]")
DEFAULT_POLICY_RELATIVE_PATH = Path("scripts/ci/config/unsafe_debt_policy.toml")


@dataclass(frozen=True)
class AuditPolicy:
    include_paths: list[str] | None
    ignore_paths: list[str]
    ignore_pattern_ids: list[str]
    enforce_crate_unsafe_guard: bool
    fail_on_excluded_crate_roots: bool
    source_path: str | None


def normalize_prefix(raw: str) -> str:
    normalized = Path(raw).as_posix().strip()
    if normalized in ("", "."):
        return ""
    return normalized.strip("/")


def is_included(path: str, include_paths: list[str]) -> bool:
    if not include_paths:
        return True
    for prefix in include_paths:
        if not prefix:
            return True
        if path == prefix or path.startswith(prefix + "/"):
            return True
    return False


def is_ignored(path: str, ignore_paths: list[str]) -> bool:
    if not ignore_paths:
        return False
    return is_included(path, ignore_paths)


def git_stdout(repo_root: Path, args: list[str]) -> str | None:
    proc = subprocess.run(
        ["git", "-C", str(repo_root), *args],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        return None
    return proc.stdout


def list_rust_files(repo_root: Path, include_paths: list[str]) -> tuple[list[str], str]:
    files: list[str] = []
    source_mode = "filesystem_walk"
    git_files = git_stdout(repo_root, ["ls-files", "--", "*.rs"])
    if git_files is not None:
        source_mode = "git_ls_files"
        for raw_line in git_files.splitlines():
            rel_path = raw_line.strip()
            if not rel_path:
                continue
            if is_included(rel_path, include_paths):
                files.append(rel_path)
    else:
        for file_path in repo_root.rglob("*.rs"):
            rel_path = file_path.relative_to(repo_root).as_posix()
            if is_included(rel_path, include_paths):
                files.append(rel_path)

    files = sorted(set(files))
    return files, source_mode


def list_cargo_manifests(repo_root: Path) -> list[str]:
    manifests: list[str] = []
    source_mode = "filesystem_walk"
    git_files = git_stdout(repo_root, ["ls-files", "--", "Cargo.toml", "**/Cargo.toml"])
    if git_files is not None:
        source_mode = "git_ls_files"
        for raw_line in git_files.splitlines():
            rel_path = raw_line.strip()
            if rel_path:
                manifests.append(rel_path)
    else:
        for file_path in repo_root.rglob("Cargo.toml"):
            manifests.append(file_path.relative_to(repo_root).as_posix())

    # Keep deterministic output and preserve source mode for observability.
    _ = source_mode
    return sorted(set(manifests))


def add_target_if_exists(
    targets: set[str],
    *,
    repo_root: Path,
    crate_dir: Path,
    rel_target: str,
) -> None:
    target_path = (crate_dir / rel_target).resolve()
    try:
        rel_repo = target_path.relative_to(repo_root).as_posix()
    except ValueError:
        return
    if target_path.is_file():
        targets.add(rel_repo)


def list_crate_roots(repo_root: Path) -> list[str]:
    crate_roots: set[str] = set()
    for manifest_rel in list_cargo_manifests(repo_root):
        manifest_path = (repo_root / manifest_rel).resolve()
        try:
            manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
        except (FileNotFoundError, tomllib.TOMLDecodeError):
            continue

        package = manifest.get("package")
        if not isinstance(package, dict):
            continue

        crate_dir = manifest_path.parent

        lib_section = manifest.get("lib")
        if isinstance(lib_section, dict):
            lib_path = lib_section.get("path")
            if isinstance(lib_path, str) and lib_path.strip():
                add_target_if_exists(
                    crate_roots,
                    repo_root=repo_root,
                    crate_dir=crate_dir,
                    rel_target=lib_path,
                )
            else:
                add_target_if_exists(
                    crate_roots,
                    repo_root=repo_root,
                    crate_dir=crate_dir,
                    rel_target="src/lib.rs",
                )
        else:
            add_target_if_exists(
                crate_roots,
                repo_root=repo_root,
                crate_dir=crate_dir,
                rel_target="src/lib.rs",
            )

        bins = manifest.get("bin")
        if isinstance(bins, list) and bins:
            for item in bins:
                if not isinstance(item, dict):
                    continue
                bin_path = item.get("path")
                if isinstance(bin_path, str) and bin_path.strip():
                    add_target_if_exists(
                        crate_roots,
                        repo_root=repo_root,
                        crate_dir=crate_dir,
                        rel_target=bin_path,
                    )
                else:
                    add_target_if_exists(
                        crate_roots,
                        repo_root=repo_root,
                        crate_dir=crate_dir,
                        rel_target="src/main.rs",
                    )
        elif package.get("autobins", True):
            add_target_if_exists(
                crate_roots,
                repo_root=repo_root,
                crate_dir=crate_dir,
                rel_target="src/main.rs",
            )

    return sorted(crate_roots)


def load_policy(repo_root: Path, policy_file_arg: str | None) -> AuditPolicy:
    policy_path: Path | None = None
    if policy_file_arg:
        policy_path = (repo_root / policy_file_arg).resolve()
    else:
        default_policy = (repo_root / DEFAULT_POLICY_RELATIVE_PATH).resolve()
        if default_policy.is_file():
            policy_path = default_policy

    if policy_path is None:
        return AuditPolicy(
            include_paths=None,
            ignore_paths=[],
            ignore_pattern_ids=[],
            enforce_crate_unsafe_guard=True,
            fail_on_excluded_crate_roots=False,
            source_path=None,
        )

    raw = tomllib.loads(policy_path.read_text(encoding="utf-8"))
    audit = raw.get("audit")
    if not isinstance(audit, dict):
        raise ValueError("policy must contain [audit] table")

    include_paths: list[str] | None = None
    include_raw = audit.get("include_paths")
    if include_raw is not None:
        if not isinstance(include_raw, list) or not all(isinstance(i, str) for i in include_raw):
            raise ValueError("[audit].include_paths must be an array of strings")
        include_paths = [normalize_prefix(i) for i in include_raw]

    ignore_raw = audit.get("ignore_paths", [])
    if not isinstance(ignore_raw, list) or not all(isinstance(i, str) for i in ignore_raw):
        raise ValueError("[audit].ignore_paths must be an array of strings")

    ignore_pattern_raw = audit.get("ignore_pattern_ids", [])
    if not isinstance(ignore_pattern_raw, list) or not all(
        isinstance(i, str) for i in ignore_pattern_raw
    ):
        raise ValueError("[audit].ignore_pattern_ids must be an array of strings")

    enforce_guard = audit.get("enforce_crate_unsafe_guard", True)
    if not isinstance(enforce_guard, bool):
        raise ValueError("[audit].enforce_crate_unsafe_guard must be a boolean")

    fail_on_excluded = audit.get("fail_on_excluded_crate_roots", False)
    if not isinstance(fail_on_excluded, bool):
        raise ValueError("[audit].fail_on_excluded_crate_roots must be a boolean")

    return AuditPolicy(
        include_paths=include_paths,
        ignore_paths=[normalize_prefix(i) for i in ignore_raw],
        ignore_pattern_ids=sorted(set(ignore_pattern_raw)),
        enforce_crate_unsafe_guard=enforce_guard,
        fail_on_excluded_crate_roots=fail_on_excluded,
        source_path=policy_path.relative_to(repo_root).as_posix(),
    )


def current_revision(repo_root: Path) -> str:
    revision = git_stdout(repo_root, ["rev-parse", "HEAD"])
    if revision is None:
        return ""
    return revision.strip()


def build_input_digest(repo_root: Path, files: list[str]) -> str:
    digest = hashlib.sha256()
    for rel_path in files:
        file_path = repo_root / rel_path
        digest.update(rel_path.encode("utf-8"))
        digest.update(b"\0")
        digest.update(file_path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


def scan_files(repo_root: Path, files: list[str]) -> list[dict[str, object]]:
    findings: list[dict[str, object]] = []
    for rel_path in files:
        file_path = repo_root / rel_path
        text = file_path.read_text(encoding="utf-8", errors="replace")
        for line_number, line in enumerate(text.splitlines(), start=1):
            for pattern in PATTERNS:
                for match in pattern.regex.finditer(line):
                    findings.append(
                        {
                            "path": rel_path,
                            "line": line_number,
                            "column": match.start() + 1,
                            "pattern_id": pattern.id,
                            "category": pattern.category,
                            "severity": pattern.severity,
                            "match": match.group(0),
                            "line_text": line.strip(),
                        }
                    )

    findings.sort(
        key=lambda item: (
            str(item["path"]),
            int(item["line"]),
            int(item["column"]),
            str(item["pattern_id"]),
        )
    )
    return findings


def scan_crate_roots_for_guard(repo_root: Path, crate_roots: list[str]) -> list[dict[str, object]]:
    findings: list[dict[str, object]] = []
    for rel_path in crate_roots:
        file_path = repo_root / rel_path
        text = file_path.read_text(encoding="utf-8", errors="replace")
        if CRATE_UNSAFE_GUARD_RE.search(text):
            continue
        findings.append(
            {
                "path": rel_path,
                "line": 1,
                "column": 1,
                "pattern_id": "missing_crate_unsafe_guard",
                "category": "policy",
                "severity": "high",
                "match": "<missing-crate-unsafe-guard>",
                "line_text": "crate root is missing #![forbid(unsafe_code)] or #![deny(unsafe_code)]",
            }
        )
    return findings


def filter_findings(
    findings: list[dict[str, object]],
    *,
    ignore_paths: list[str],
    ignore_pattern_ids: set[str],
) -> list[dict[str, object]]:
    filtered: list[dict[str, object]] = []
    for finding in findings:
        path = str(finding.get("path", ""))
        pattern_id = str(finding.get("pattern_id", ""))
        if pattern_id in ignore_pattern_ids:
            continue
        if is_ignored(path, ignore_paths):
            continue
        filtered.append(finding)
    return filtered


def sorted_counter(counter: Counter[str]) -> dict[str, int]:
    return {key: counter[key] for key in sorted(counter)}


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Audit Rust unsafe/risky patterns and emit reproducible JSON findings."
    )
    parser.add_argument("--repo-root", default=".")
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--include-path", action="append")
    parser.add_argument("--ignore-path", action="append")
    parser.add_argument("--ignore-pattern-id", action="append")
    parser.add_argument("--policy-file")
    parser.add_argument("--fail-on-findings", action="store_true")
    parser.add_argument("--fail-on-excluded-crate-roots", action="store_true")
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    policy = load_policy(repo_root, args.policy_file)

    include_source = (
        args.include_path
        if args.include_path
        else (policy.include_paths if policy.include_paths is not None else list(DEFAULT_INCLUDE_PATHS))
    )
    include_paths = [
        normalize_prefix(path)
        for path in include_source
    ]
    ignore_paths = sorted(
        set(
            [normalize_prefix(path) for path in policy.ignore_paths]
            + [normalize_prefix(path) for path in (args.ignore_path or [])]
        )
    )
    ignore_pattern_ids = sorted(
        set(policy.ignore_pattern_ids) | set(args.ignore_pattern_id or [])
    )
    fail_on_excluded_roots = (
        args.fail_on_excluded_crate_roots or policy.fail_on_excluded_crate_roots
    )

    files, source_mode = list_rust_files(repo_root, include_paths)
    files = [path for path in files if not is_ignored(path, ignore_paths)]

    all_crate_roots = list_crate_roots(repo_root)
    crate_roots = [
        path
        for path in all_crate_roots
        if is_included(path, include_paths) and not is_ignored(path, ignore_paths)
    ]
    excluded_crate_roots = sorted(set(all_crate_roots) - set(crate_roots))

    findings = scan_files(repo_root, files)
    if policy.enforce_crate_unsafe_guard:
        findings.extend(scan_crate_roots_for_guard(repo_root, crate_roots))
    findings = filter_findings(
        findings,
        ignore_paths=ignore_paths,
        ignore_pattern_ids=set(ignore_pattern_ids),
    )
    findings.sort(
        key=lambda item: (
            str(item["path"]),
            int(item["line"]),
            int(item["column"]),
            str(item["pattern_id"]),
        )
    )

    by_pattern = Counter(str(item["pattern_id"]) for item in findings)
    by_category = Counter(str(item["category"]) for item in findings)
    by_severity = Counter(str(item["severity"]) for item in findings)

    report = {
        "schema_version": "zeroclaw.audit.v1",
        "event_type": "unsafe_debt_audit",
        "script_version": "3",
        "source": {
            "revision": current_revision(repo_root),
            "mode": source_mode,
            "include_paths": include_paths,
            "ignore_paths": ignore_paths,
            "ignore_pattern_ids": ignore_pattern_ids,
            "policy_file": policy.source_path,
            "inputs_sha256": build_input_digest(repo_root, files),
            "files_scanned": len(files),
            "crate_roots_total": len(all_crate_roots),
            "crate_roots_scanned": len(crate_roots),
            "crate_roots_excluded": len(excluded_crate_roots),
            "excluded_crate_roots": excluded_crate_roots,
        },
        "patterns": [  # Static regex patterns plus semantic policy detector.
            *[
                {
                    "id": pattern.id,
                    "category": pattern.category,
                    "severity": pattern.severity,
                    "description": pattern.description,
                    "regex": pattern.regex.pattern,
                }
                for pattern in PATTERNS
            ],
            {
                "id": "missing_crate_unsafe_guard",
                "category": "policy",
                "severity": "high",
                "description": (
                    "Crate root missing `#![forbid(unsafe_code)]` or `#![deny(unsafe_code)]`."
                ),
                "regex": CRATE_UNSAFE_GUARD_RE.pattern,
            },
        ],
        "summary": {
            "total_findings": len(findings),
            "by_pattern": sorted_counter(by_pattern),
            "by_category": sorted_counter(by_category),
            "by_severity": sorted_counter(by_severity),
        },
        "findings": findings,
    }

    output_json = Path(args.output_json)
    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    if fail_on_excluded_roots and excluded_crate_roots:
        print(
            f"excluded crate roots detected: {len(excluded_crate_roots)}; "
            "use --include-path or policy include_paths to cover them",
            file=sys.stderr,
        )
        return 4

    if args.fail_on_findings and findings:
        print(f"unsafe debt findings detected: {len(findings)}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
