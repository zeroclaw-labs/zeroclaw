#!/usr/bin/env python3

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path


DOC_PATH_RE = re.compile(r"\.mdx?$")
URL_RE = re.compile(r"https?://[^\s<>'\"]+")
INLINE_LINK_RE = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
REF_LINK_RE = re.compile(r"^\s*\[[^\]]+\]:\s*(\S+)")
TRAILING_PUNCTUATION = ").,;:!?]}'\""


def run_git(args: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["git", *args], check=False, capture_output=True, text=True)


def commit_exists(rev: str) -> bool:
    if not rev:
        return False
    return run_git(["cat-file", "-e", f"{rev}^{{commit}}"]).returncode == 0


def normalize_docs_files(raw: str) -> list[str]:
    if not raw:
        return []
    files: list[str] = []
    for line in raw.splitlines():
        path = line.strip()
        if path:
            files.append(path)
    return files


def infer_base_sha(provided: str) -> str:
    if commit_exists(provided):
        return provided
    if run_git(["rev-parse", "--verify", "origin/main"]).returncode != 0:
        return ""
    proc = run_git(["merge-base", "origin/main", "HEAD"])
    candidate = proc.stdout.strip()
    return candidate if commit_exists(candidate) else ""


def infer_docs_files(base_sha: str, provided: list[str]) -> list[str]:
    if provided:
        return provided
    if not base_sha:
        return []
    diff = run_git(["diff", "--name-only", base_sha, "HEAD"])
    files: list[str] = []
    for line in diff.stdout.splitlines():
        path = line.strip()
        if not path:
            continue
        if DOC_PATH_RE.search(path) or path in {"LICENSE", ".github/pull_request_template.md"}:
            files.append(path)
    return files


def normalize_link_target(raw_target: str, source_path: str) -> str | None:
    target = raw_target.strip()
    if target.startswith("<") and target.endswith(">"):
        target = target[1:-1].strip()

    if not target:
        return None

    if " " in target:
        target = target.split()[0].strip()

    if not target or target.startswith("#"):
        return None

    lower = target.lower()
    if lower.startswith(("mailto:", "tel:", "javascript:")):
        return None

    if target.startswith(("http://", "https://")):
        return target.rstrip(TRAILING_PUNCTUATION)

    path_without_fragment = target.split("#", 1)[0].split("?", 1)[0]
    if not path_without_fragment:
        return None

    if path_without_fragment.startswith("/"):
        resolved = path_without_fragment.lstrip("/")
    else:
        resolved = os.path.normpath(
            os.path.join(os.path.dirname(source_path) or ".", path_without_fragment)
        )

    if not resolved or resolved == ".":
        return None

    return resolved


def extract_links(text: str, source_path: str) -> list[str]:
    links: list[str] = []
    for match in URL_RE.findall(text):
        url = match.rstrip(TRAILING_PUNCTUATION)
        if url:
            links.append(url)

    for match in INLINE_LINK_RE.findall(text):
        normalized = normalize_link_target(match, source_path)
        if normalized:
            links.append(normalized)

    ref_match = REF_LINK_RE.match(text)
    if ref_match:
        normalized = normalize_link_target(ref_match.group(1), source_path)
        if normalized:
            links.append(normalized)

    return links


def added_lines_for_file(base_sha: str, path: str) -> list[str]:
    if base_sha:
        diff = run_git(["diff", "--unified=0", base_sha, "HEAD", "--", path])
        lines: list[str] = []
        for raw_line in diff.stdout.splitlines():
            if raw_line.startswith("+++"):
                continue
            if raw_line.startswith("+"):
                lines.append(raw_line[1:])
        return lines

    file_path = Path(path)
    if not file_path.is_file():
        return []
    return file_path.read_text(encoding="utf-8", errors="ignore").splitlines()


def main() -> int:
    parser = argparse.ArgumentParser(description="Collect HTTP(S) links added in changed docs lines")
    parser.add_argument("--base", default="", help="Base commit SHA")
    parser.add_argument(
        "--docs-files",
        default="",
        help="Newline-separated docs files list",
    )
    parser.add_argument("--output", required=True, help="Output file for unique URLs")
    args = parser.parse_args()

    base_sha = infer_base_sha(args.base)
    docs_files = infer_docs_files(base_sha, normalize_docs_files(args.docs_files))

    existing_files = [path for path in docs_files if Path(path).is_file()]
    if not existing_files:
        Path(args.output).write_text("", encoding="utf-8")
        print("No docs files available for link collection.")
        return 0

    unique_urls: list[str] = []
    seen: set[str] = set()
    for path in existing_files:
        for line in added_lines_for_file(base_sha, path):
            for link in extract_links(line, path):
                if link not in seen:
                    seen.add(link)
                    unique_urls.append(link)

    Path(args.output).write_text("\n".join(unique_urls) + ("\n" if unique_urls else ""), encoding="utf-8")
    print(f"Collected {len(unique_urls)} added link(s) from {len(existing_files)} docs file(s).")
    return 0


if __name__ == "__main__":
    sys.exit(main())
