#!/usr/bin/env python3

"""Check authored Markdown links against the repository's docs source tree."""

from __future__ import annotations

import argparse
import os
import re
import sys
from pathlib import Path


INLINE_LINK_RE = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
REFERENCE_LINK_RE = re.compile(r"^\s*\[[^\]]+\]:\s*(\S+)")
FENCE_RE = re.compile(r"^\s*(`{3,}|~{3,})")
INCLUDE_RE = re.compile(r"\{\{#include\s+([^}\s]+)")


def _strip_target(raw: str) -> str:
    target = raw.strip()
    if target.startswith("<") and target.endswith(">"):
        target = target[1:-1].strip()
    if " " in target:
        target = target.split(" ", 1)[0]
    return target


def _local_target(raw: str, source: Path) -> str | None:
    target = _strip_target(raw)
    if not target or target.startswith("#"):
        return None
    if target.lower().startswith(("http://", "https://", "mailto:", "tel:", "javascript:")):
        return None
    if target.startswith("/"):
        return None

    path = target.split("#", 1)[0].split("?", 1)[0]
    if not path:
        return None
    resolved = os.path.normpath(os.path.join(source.parent.as_posix(), path))
    return resolved if resolved != "." else None


def _links_in_file(path: Path) -> list[tuple[int, str]]:
    links: list[tuple[int, str]] = []
    in_fence = False
    for line_number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if FENCE_RE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        for match in INLINE_LINK_RE.finditer(line):
            links.append((line_number, match.group(1)))
        reference = REFERENCE_LINK_RE.match(line)
        if reference:
            links.append((line_number, reference.group(1)))
    return links


def _include_contexts(root: Path, snippet: Path) -> list[Path]:
    """Return authored pages whose mdBook include renders ``snippet``."""
    contexts: list[Path] = []
    for candidate in sorted(root.rglob("*.md")) + sorted(root.rglob("*.mdx")):
        if candidate == snippet or not candidate.is_file():
            continue
        try:
            content = candidate.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        for raw_include in INCLUDE_RE.findall(content):
            included = (candidate.parent / raw_include).resolve()
            if included == snippet.resolve():
                contexts.append(candidate)
                break
    return contexts


def find_broken_links(root: Path) -> list[tuple[str, int, str]]:
    broken: list[tuple[str, int, str]] = []
    seen: set[tuple[str, int, str]] = set()
    generated_targets = {
        (root / "reference/cli.md").as_posix(),
        (root / "reference/config.md").as_posix(),
    }
    for path in sorted(root.rglob("*.md")) + sorted(root.rglob("*.mdx")):
        if not path.is_file():
            continue
        source = path.as_posix()
        source_contexts = _include_contexts(root, path) if "_snippets" in path.parts else [path]
        if not source_contexts:
            continue
        for line_number, raw_target in _links_in_file(path):
            for context in source_contexts:
                resolved = _local_target(raw_target, context)
                if resolved is None or resolved in generated_targets:
                    continue
                if not Path(resolved).exists():
                    entry = (source, line_number, resolved)
                    if entry not in seen:
                        seen.add(entry)
                        broken.append(entry)
    return broken


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        default="docs/book/src",
        type=Path,
        help="authored Markdown source root (default: docs/book/src)",
    )
    args = parser.parse_args()
    root = args.root
    if not root.is_dir():
        print(f"error: docs source root does not exist: {root}", file=sys.stderr)
        return 2

    broken = find_broken_links(root)
    if broken:
        print("Broken internal Markdown link target(s):")
        for source, line_number, target in broken:
            print(f"  {source}:{line_number} -> {target}")
        print(f"Found {len(broken)} broken internal Markdown link(s).")
        return 1

    files = sum(1 for suffix in ("*.md", "*.mdx") for _ in root.rglob(suffix))
    print(f"Checked authored Markdown links in {files} file(s); no broken local targets.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
