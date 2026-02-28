#!/usr/bin/env python3
"""Validate release artifact contract completeness for multi-arch publishing."""

from __future__ import annotations

import argparse
import datetime as dt
import fnmatch
import json
import sys
from pathlib import Path

CONTRACT_SCHEMA = "zeroclaw.release-artifact-contract.v1"


def load_contract(path: Path) -> tuple[dict, list[str]]:
    violations: list[str] = []
    raw = json.loads(path.read_text(encoding="utf-8"))
    schema_version = raw.get("schema_version")

    if not isinstance(schema_version, str) or not schema_version.strip():
        violations.append("Contract field `schema_version` must be a non-empty string.")
        schema_version = ""
    else:
        schema_version = schema_version.strip()
        if schema_version != CONTRACT_SCHEMA:
            violations.append(
                f"Contract field `schema_version` must be `{CONTRACT_SCHEMA}`, got `{schema_version}`."
            )

    def ensure_list(name: str) -> list[str]:
        value = raw.get(name)
        if not isinstance(value, list) or not value:
            violations.append(f"Contract field `{name}` must be a non-empty array.")
            return []
        normalized: list[str] = []
        seen: set[str] = set()
        for item in value:
            if not isinstance(item, str) or not item.strip():
                violations.append(f"Contract field `{name}` contains an invalid entry.")
                continue
            text = item.strip()
            if text in seen:
                violations.append(f"Contract field `{name}` contains duplicate entry `{text}`.")
                continue
            normalized.append(text)
            seen.add(text)
        return normalized

    contract = {
        "schema_version": schema_version,
        "release_archive_patterns": ensure_list("release_archive_patterns"),
        "required_manifest_files": ensure_list("required_manifest_files"),
        "required_sbom_files": ensure_list("required_sbom_files"),
        "required_notice_files": ensure_list("required_notice_files"),
    }

    return contract, violations


def build_markdown(report: dict) -> str:
    lines: list[str] = []
    lines.append("# Release Artifact Guard Report")
    lines.append("")
    lines.append(f"- Generated at: `{report['generated_at']}`")
    lines.append(f"- Artifacts dir: `{report['artifacts_dir']}`")
    lines.append(f"- Contract file: `{report['contract_file']}`")
    lines.append(f"- Contract schema: `{report['contract_schema_version']}`")
    lines.append(f"- Ready: `{report['ready']}`")
    lines.append("")

    categories = report.get("categories", {})
    lines.append("## Category Summary")
    for category_name in ("release_archives", "manifest_files", "sbom_files", "notice_files"):
        row = categories.get(category_name, {})
        lines.append(
            f"- `{category_name}`: expected `{row.get('expected_count', 0)}`, "
            f"found `{row.get('found_count', 0)}`, missing `{row.get('missing_count', 0)}`, "
            f"extra `{row.get('extra_count', 0)}`"
        )
    lines.append("")

    for category_name in ("release_archives", "manifest_files", "sbom_files", "notice_files"):
        row = categories.get(category_name, {})
        lines.append(f"## {category_name.replace('_', ' ').title()}")
        found = row.get("found", [])
        missing = row.get("missing", [])
        extra = row.get("extra", [])
        if found:
            lines.append("- Found:")
            for item in found:
                lines.append(f"  - `{item}`")
        else:
            lines.append("- Found: none")
        if missing:
            lines.append("- Missing:")
            for item in missing:
                lines.append(f"  - `{item}`")
        else:
            lines.append("- Missing: none")
        if extra:
            lines.append("- Extra:")
            for item in extra:
                lines.append(f"  - `{item}`")
        else:
            lines.append("- Extra: none")
        lines.append("")

    if report["violations"]:
        lines.append("## Violations")
        for item in report["violations"]:
            lines.append(f"- {item}")
        lines.append("")

    return "\n".join(lines).rstrip() + "\n"


def collect_files(artifacts_dir: Path) -> list[str]:
    files: list[str] = []
    for path in sorted(artifacts_dir.rglob("*")):
        if path.is_file():
            files.append(path.relative_to(artifacts_dir).as_posix())
    return files


def match_expected(
    files: list[str],
    expected_patterns: list[str],
    *,
    allow_unmatched_extra: bool,
) -> tuple[list[str], list[str], list[str], list[str]]:
    found: list[str] = []
    missing: list[str] = []
    matched_files: set[str] = set()
    expected_to_found: dict[str, str] = {}

    for pattern in expected_patterns:
        if "/" in pattern:
            matches = sorted([f for f in files if fnmatch.fnmatch(f, pattern)])
        else:
            matches = sorted([f for f in files if fnmatch.fnmatch(Path(f).name, pattern)])
        if not matches:
            missing.append(pattern)
            continue
        expected_to_found[pattern] = matches[0]
        found.append(matches[0])
        matched_files.add(matches[0])

    unmatched = sorted([f for f in files if f not in matched_files])
    extras = [] if allow_unmatched_extra else unmatched
    return found, missing, unmatched, extras


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate release artifact contract and emit auditable reports.")
    parser.add_argument("--artifacts-dir", required=True)
    parser.add_argument("--contract-file", required=True)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--allow-extra-archives", action="store_true")
    parser.add_argument("--allow-extra-manifest-files", action="store_true")
    parser.add_argument("--allow-extra-sbom-files", action="store_true")
    parser.add_argument("--allow-extra-notice-files", action="store_true")
    parser.add_argument("--skip-manifest-files", action="store_true")
    parser.add_argument("--skip-sbom-files", action="store_true")
    parser.add_argument("--skip-notice-files", action="store_true")
    parser.add_argument("--fail-on-violation", action="store_true")
    args = parser.parse_args()

    artifacts_dir = Path(args.artifacts_dir).resolve()
    contract_file = Path(args.contract_file).resolve()
    output_json = Path(args.output_json)
    output_md = Path(args.output_md)

    violations: list[str] = []
    warnings: list[str] = []

    if not artifacts_dir.exists() or not artifacts_dir.is_dir():
        print(f"artifacts dir does not exist: {artifacts_dir}", file=sys.stderr)
        return 2
    if not contract_file.exists() or not contract_file.is_file():
        print(f"contract file does not exist: {contract_file}", file=sys.stderr)
        return 2

    contract, contract_violations = load_contract(contract_file)
    violations.extend(contract_violations)
    files = collect_files(artifacts_dir)

    release_found, release_missing, release_unmatched, release_extra = match_expected(
        files,
        contract["release_archive_patterns"],
        allow_unmatched_extra=args.allow_extra_archives,
    )

    if args.skip_manifest_files:
        manifest_found = []
        manifest_missing = []
        manifest_unmatched = []
        manifest_extra = []
    else:
        manifest_found, manifest_missing, manifest_unmatched, manifest_extra = match_expected(
            files,
            contract["required_manifest_files"],
            allow_unmatched_extra=args.allow_extra_manifest_files,
        )

    if args.skip_sbom_files:
        sbom_found = []
        sbom_missing = []
        sbom_unmatched = []
        sbom_extra = []
    else:
        sbom_found, sbom_missing, sbom_unmatched, sbom_extra = match_expected(
            files,
            contract["required_sbom_files"],
            allow_unmatched_extra=args.allow_extra_sbom_files,
        )

    if args.skip_notice_files:
        notice_found = []
        notice_missing = []
        notice_unmatched = []
        notice_extra = []
    else:
        notice_found, notice_missing, notice_unmatched, notice_extra = match_expected(
            files,
            contract["required_notice_files"],
            allow_unmatched_extra=args.allow_extra_notice_files,
        )

    if release_missing:
        violations.append(
            f"Missing release archives: {', '.join(release_missing)}."
        )
    if release_extra:
        violations.append(
            f"Unexpected release archive files: {', '.join(release_extra)}."
        )

    if not args.skip_manifest_files and manifest_missing:
        violations.append(
            f"Missing required manifest files: {', '.join(manifest_missing)}."
        )
    if not args.skip_manifest_files and manifest_extra:
        warnings.append(
            f"Extra manifest files present: {', '.join(manifest_extra)}."
        )

    if not args.skip_sbom_files and sbom_missing:
        violations.append(
            f"Missing required SBOM files: {', '.join(sbom_missing)}."
        )
    if not args.skip_sbom_files and sbom_extra:
        warnings.append(
            f"Extra SBOM files present: {', '.join(sbom_extra)}."
        )

    if not args.skip_notice_files and notice_missing:
        violations.append(
            f"Missing required notice/license files: {', '.join(notice_missing)}."
        )
    if not args.skip_notice_files and notice_extra:
        warnings.append(
            f"Extra notice/license files present: {', '.join(notice_extra)}."
        )

    report = {
        "schema_version": "zeroclaw.release-artifact-guard.v1",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "artifacts_dir": str(artifacts_dir),
        "contract_file": str(contract_file),
        "contract_schema_version": contract.get("schema_version"),
        "ready": not violations,
        "categories": {
            "release_archives": {
                "expected": contract["release_archive_patterns"],
                "expected_count": len(contract["release_archive_patterns"]),
                "found": release_found,
                "found_count": len(release_found),
                "missing": release_missing,
                "missing_count": len(release_missing),
                "extra": release_extra,
                "extra_count": len(release_extra),
            },
            "manifest_files": {
                "expected": contract["required_manifest_files"],
                "expected_count": len(contract["required_manifest_files"]),
                "found": manifest_found,
                "found_count": len(manifest_found),
                "missing": manifest_missing,
                "missing_count": len(manifest_missing),
                "extra": manifest_extra,
                "extra_count": len(manifest_extra),
                "skipped": args.skip_manifest_files,
            },
            "sbom_files": {
                "expected": contract["required_sbom_files"],
                "expected_count": len(contract["required_sbom_files"]),
                "found": sbom_found,
                "found_count": len(sbom_found),
                "missing": sbom_missing,
                "missing_count": len(sbom_missing),
                "extra": sbom_extra,
                "extra_count": len(sbom_extra),
                "skipped": args.skip_sbom_files,
            },
            "notice_files": {
                "expected": contract["required_notice_files"],
                "expected_count": len(contract["required_notice_files"]),
                "found": notice_found,
                "found_count": len(notice_found),
                "missing": notice_missing,
                "missing_count": len(notice_missing),
                "extra": notice_extra,
                "extra_count": len(notice_extra),
                "skipped": args.skip_notice_files,
            },
        },
        "warnings": warnings,
        "violations": violations,
    }

    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_md.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    output_md.write_text(build_markdown(report), encoding="utf-8")

    if args.fail_on_violation and violations:
        print("release artifact guard violations found:", file=sys.stderr)
        for item in violations:
            print(f"- {item}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
