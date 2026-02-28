#!/usr/bin/env python3
"""Generate release-notes preface with supply-chain provenance and SBOM references."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sys
from pathlib import Path
from urllib.parse import quote

REQUIRED_REFERENCES = {
    "release_manifest_json": "release-manifest.json",
    "release_manifest_markdown": "release-manifest.md",
    "checksums": "SHA256SUMS",
    "sbom_cyclonedx": "zeroclaw.cdx.json",
    "sbom_spdx": "zeroclaw.spdx.json",
    "checksums_provenance": "zeroclaw.sha256sums.intoto.json",
    "checksums_provenance_audit_event": "audit-event-release-sha256sums-provenance.json",
    "release_trigger_guard": "release-trigger-guard.json",
    "release_trigger_guard_audit_event": "audit-event-release-trigger-guard.json",
    "release_artifact_guard_publish": "release-artifact-guard.publish.json",
    "release_artifact_guard_publish_audit_event": "audit-event-release-artifact-guard-publish.json",
}

OPTIONAL_REFERENCES = {
    "checksums_signature": "SHA256SUMS.sig",
    "checksums_certificate": "SHA256SUMS.pem",
    "checksums_sigstore_bundle": "SHA256SUMS.sigstore.json",
}


def collect_files(artifacts_dir: Path) -> list[str]:
    files: list[str] = []
    for path in sorted(artifacts_dir.rglob("*")):
        if path.is_file():
            files.append(path.relative_to(artifacts_dir).as_posix())
    return files


def find_by_basename(files: list[str], basename: str) -> list[str]:
    return [entry for entry in files if Path(entry).name == basename]


def release_asset_url(repository: str, release_tag: str, asset_name: str) -> str:
    return (
        f"https://github.com/{repository}/releases/download/{quote(release_tag, safe='')}/"
        f"{quote(asset_name, safe='')}"
    )


def resolve_reference(
    files: list[str],
    *,
    basename: str,
    key: str,
    repository: str,
    release_tag: str,
    required: bool,
) -> tuple[dict[str, object], list[str], list[str]]:
    warnings: list[str] = []
    violations: list[str] = []
    matches = find_by_basename(files, basename)

    if not matches:
        ref = {
            "key": key,
            "required": required,
            "basename": basename,
            "found": False,
            "path": None,
            "asset_name": None,
            "url": None,
        }
        if required:
            violations.append(f"Missing required release-notes reference file `{basename}`.")
        return ref, warnings, violations

    chosen = sorted(matches)[0]
    if len(matches) > 1:
        warnings.append(
            f"Multiple files matched `{basename}` ({len(matches)}); using `{chosen}` for release-note link generation."
        )

    asset_name = Path(chosen).name
    ref = {
        "key": key,
        "required": required,
        "basename": basename,
        "found": True,
        "path": chosen,
        "asset_name": asset_name,
        "url": release_asset_url(repository, release_tag, asset_name),
    }
    return ref, warnings, violations


def link(ref: dict[str, object]) -> str:
    if not ref.get("found"):
        return f"`{ref['basename']}` (missing)"
    return f"[`{ref['basename']}`]({ref['url']})"


def build_markdown(report: dict[str, object]) -> str:
    refs = report["references"]
    lines: list[str] = []
    lines.append("## Supply-Chain Evidence")
    lines.append("")
    lines.append(f"- Generated at: `{report['generated_at']}`")
    lines.append(f"- Repository: `{report['repository']}`")
    lines.append(f"- Release tag: `{report['release_tag']}`")
    lines.append(f"- Ready: `{report['ready']}`")
    lines.append("")

    lines.append("### Manifest + Integrity")
    lines.append(f"- {link(refs['release_manifest_json'])}")
    lines.append(f"- {link(refs['release_manifest_markdown'])}")
    lines.append(f"- {link(refs['checksums'])}")
    lines.append("")

    lines.append("### SBOM")
    lines.append(f"- {link(refs['sbom_cyclonedx'])}")
    lines.append(f"- {link(refs['sbom_spdx'])}")
    lines.append("")

    lines.append("### Provenance")
    lines.append(f"- {link(refs['checksums_provenance'])}")
    lines.append(f"- {link(refs['checksums_provenance_audit_event'])}")
    if refs["checksums_signature"].get("found"):
        lines.append(f"- {link(refs['checksums_signature'])}")
    if refs["checksums_certificate"].get("found"):
        lines.append(f"- {link(refs['checksums_certificate'])}")
    if refs["checksums_sigstore_bundle"].get("found"):
        lines.append(f"- {link(refs['checksums_sigstore_bundle'])}")
    lines.append("")

    lines.append("### Release Gate Audits")
    lines.append(f"- {link(refs['release_trigger_guard'])}")
    lines.append(f"- {link(refs['release_trigger_guard_audit_event'])}")
    lines.append(f"- {link(refs['release_artifact_guard_publish'])}")
    lines.append(f"- {link(refs['release_artifact_guard_publish_audit_event'])}")
    lines.append("")

    if report["warnings"]:
        lines.append("### Warnings")
        for item in report["warnings"]:
            lines.append(f"- {item}")
        lines.append("")

    if report["violations"]:
        lines.append("### Violations")
        for item in report["violations"]:
            lines.append(f"- {item}")
        lines.append("")

    lines.append("## Automated Commit Notes")
    lines.append("")
    lines.append("The sections below are generated automatically by GitHub from the validated release commit window.")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Generate release notes preface with provenance and SBOM links."
    )
    parser.add_argument("--artifacts-dir", required=True)
    parser.add_argument("--repository", required=True)
    parser.add_argument("--release-tag", required=True)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--fail-on-missing", action="store_true")
    args = parser.parse_args()

    artifacts_dir = Path(args.artifacts_dir).resolve()
    output_json = Path(args.output_json)
    output_md = Path(args.output_md)

    if not artifacts_dir.exists() or not artifacts_dir.is_dir():
        print(f"artifacts dir does not exist: {artifacts_dir}", file=sys.stderr)
        return 2

    files = collect_files(artifacts_dir)
    warnings: list[str] = []
    violations: list[str] = []
    references: dict[str, dict[str, object]] = {}

    for key, basename in REQUIRED_REFERENCES.items():
        ref, ref_warnings, ref_violations = resolve_reference(
            files,
            basename=basename,
            key=key,
            repository=args.repository,
            release_tag=args.release_tag,
            required=True,
        )
        references[key] = ref
        warnings.extend(ref_warnings)
        violations.extend(ref_violations)

    for key, basename in OPTIONAL_REFERENCES.items():
        ref, ref_warnings, ref_violations = resolve_reference(
            files,
            basename=basename,
            key=key,
            repository=args.repository,
            release_tag=args.release_tag,
            required=False,
        )
        references[key] = ref
        warnings.extend(ref_warnings)
        violations.extend(ref_violations)

    report: dict[str, object] = {
        "schema_version": "zeroclaw.release-notes-supply-chain.v1",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "artifacts_dir": str(artifacts_dir),
        "repository": args.repository,
        "release_tag": args.release_tag,
        "ready": not violations,
        "references": references,
        "warnings": warnings,
        "violations": violations,
    }

    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_md.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    output_md.write_text(build_markdown(report), encoding="utf-8")

    if args.fail_on_missing and violations:
        print("release notes supply-chain reference violations found:", file=sys.stderr)
        for item in violations:
            print(f"- {item}", file=sys.stderr)
        return 3

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
