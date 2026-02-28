#!/usr/bin/env python3
"""Generate a release artifact manifest and deterministic SHA256 checksum file."""

from __future__ import annotations

import argparse
import datetime as dt
import fnmatch
import hashlib
import json
import sys
from pathlib import Path

DEFAULT_GLOBS = [
    "*.tar.gz",
    "*.zip",
    "*.cdx.json",
    "*.spdx.json",
    "LICENSE-APACHE",
    "LICENSE-MIT",
    "NOTICE",
]


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def should_include(rel_path: str, patterns: list[str]) -> bool:
    return any(fnmatch.fnmatch(rel_path, pattern) for pattern in patterns)


def build_markdown(report: dict) -> str:
    lines: list[str] = []
    lines.append("# Release Artifact Manifest")
    lines.append("")
    lines.append(f"- Generated at: `{report['generated_at']}`")
    lines.append(f"- Artifacts dir: `{report['artifacts_dir']}`")
    lines.append(f"- Release tag: `{report['release_tag'] or 'n/a'}`")
    lines.append(f"- Files: `{len(report['files'])}`")
    lines.append("")
    if not report["files"]:
        lines.append("No matching artifacts found.")
        lines.append("")
        return "\n".join(lines)

    lines.append("| File | Size (bytes) | SHA256 |")
    lines.append("| --- | ---:| --- |")
    for row in report["files"]:
        lines.append(
            f"| `{row['path']}` | {row['size_bytes']} | `{row['sha256']}` |"
        )
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Generate release artifact manifest + SHA256 checksums."
    )
    parser.add_argument("--artifacts-dir", required=True)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--checksums-path", required=True)
    parser.add_argument("--release-tag", default="")
    parser.add_argument("--include-glob", action="append", default=[])
    parser.add_argument("--fail-empty", action="store_true")
    args = parser.parse_args()

    artifacts_dir = Path(args.artifacts_dir).resolve()
    output_json = Path(args.output_json)
    output_md = Path(args.output_md)
    checksums_path = Path(args.checksums_path)

    if not artifacts_dir.exists() or not artifacts_dir.is_dir():
        print(f"artifacts dir does not exist: {artifacts_dir}", file=sys.stderr)
        return 2

    patterns = args.include_glob or DEFAULT_GLOBS
    files: list[dict[str, object]] = []

    for path in sorted(artifacts_dir.rglob("*")):
        if not path.is_file():
            continue
        rel = path.relative_to(artifacts_dir).as_posix()
        if not should_include(rel, patterns):
            continue
        digest = sha256_file(path)
        files.append(
            {
                "path": rel,
                "size_bytes": path.stat().st_size,
                "sha256": digest,
            }
        )

    report = {
        "schema_version": "zeroclaw.release-manifest.v1",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "artifacts_dir": str(artifacts_dir),
        "release_tag": args.release_tag or None,
        "include_globs": patterns,
        "files": files,
    }

    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_md.parent.mkdir(parents=True, exist_ok=True)
    checksums_path.parent.mkdir(parents=True, exist_ok=True)

    output_json.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    output_md.write_text(build_markdown(report), encoding="utf-8")

    checksum_lines = [f"{row['sha256']}  {row['path']}" for row in files]
    checksums_path.write_text("\n".join(checksum_lines) + ("\n" if checksum_lines else ""), encoding="utf-8")

    if args.fail_empty and not files:
        print("no release artifacts matched include globs", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
