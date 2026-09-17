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
FENCE_RE = re.compile(r"^[ \t]*(`{3,}|~{3,})(.*)$")
QUOTE_PREFIX_RE = re.compile(r"^[ \t]{0,3}>[ \t]?(?:>[ \t]?)*")
ATX_HEADING_RE = re.compile(r"^[ \t]{0,3}#{1,6}(?:[ \t]+|$)")
THEMATIC_BREAK_RE = re.compile(r"^[ \t]{0,3}(?:={3,}|-{3,}|(?:\*[ \t]*){3,}|(?:_[ \t]*){3,})[ \t]*$")
BLOCK_START_RE = re.compile(
    r"^[ \t]{0,3}(?:"
    r"#{1,6}(?:[ \t]+|$)|"
    r">[ \t]?|"
    r"(?:[-+*]|\d{1,9}[.)])[ \t]+|"
    r"(?:={3,}|-{3,})[ \t]*$|"
    r"(?:\*[ \t]*){3,}$|"
    r"(?:_[ \t]*){3,}$"
    r")"
)
INCLUDE_RE = re.compile(r"\{\{#include\s+([^}\s]+)")
ANCHOR_START_RE = re.compile(r"^[ \t]*<!--\s*ANCHOR:\s*([^\s]+)\s*-->[ \t]*$")
ANCHOR_END_RE = re.compile(r"^[ \t]*<!--\s*ANCHOR_END:\s*([^\s]+)\s*-->[ \t]*$")


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


def _backtick_run_end(line: str, start: int) -> int:
    end = start
    while end < len(line) and line[end] == "`":
        end += 1
    return end


def _find_backtick_run(line: str, start: int, length: int) -> tuple[int, int] | None:
    probe = start
    while probe < len(line):
        delimiter_start = line.find("`", probe)
        if delimiter_start == -1:
            return None
        delimiter_end = _backtick_run_end(line, delimiter_start)
        if delimiter_end - delimiter_start == length:
            return delimiter_start, delimiter_end
        probe = delimiter_end
    return None


def _is_escaped_backtick(line: str, start: int) -> bool:
    backslashes = 0
    probe = start - 1
    while probe >= 0 and line[probe] == "\\":
        backslashes += 1
        probe -= 1
    return backslashes % 2 == 1


def _blockquote_content(line: str) -> tuple[int, str]:
    match = QUOTE_PREFIX_RE.match(line)
    if match is None:
        return 0, line
    prefix = match.group()
    return prefix.count(">"), line[match.end() :]


def _has_matching_backtick(
    lines: list[tuple[int, str]], line_index: int, start: int, length: int
) -> bool:
    start_quote_depth, start_content = _blockquote_content(lines[line_index][1])
    if _find_backtick_run(lines[line_index][1], start, length) is not None:
        return True
    if ATX_HEADING_RE.match(start_content) or THEMATIC_BREAK_RE.match(start_content):
        return False

    for candidate_index in range(line_index, len(lines)):
        candidate = lines[candidate_index][1]
        if candidate_index == line_index:
            continue
        if not candidate.strip() or FENCE_RE.match(candidate):
            return False
        quote_depth, content = _blockquote_content(candidate)
        if quote_depth and quote_depth != start_quote_depth:
            return False
        if not content.strip() or BLOCK_START_RE.match(content) or FENCE_RE.match(content):
            return False
        search_start = 0
        if _find_backtick_run(candidate, search_start, length) is not None:
            return True
    return False


def _strip_inline_code(
    line: str,
    inline_code_length: int | None,
    lines: list[tuple[int, str]],
    line_index: int,
) -> tuple[str, int | None]:
    visible: list[str] = []
    index = 0
    active_length = inline_code_length
    while index < len(line):
        if active_length is not None:
            closing = _find_backtick_run(line, index, active_length)
            if closing is None:
                return "".join(visible), active_length
            _, index = closing
            active_length = None
            continue

        start = line.find("`", index)
        if start == -1:
            visible.append(line[index:])
            return "".join(visible), None
        visible.append(line[index:start])
        delimiter_end = _backtick_run_end(line, start)
        delimiter_length = delimiter_end - start
        if _is_escaped_backtick(line, start):
            visible.append(line[start : start + 1])
            index = start + 1
            continue
        if not _has_matching_backtick(lines, line_index, delimiter_end, delimiter_length):
            visible.append(line[start:delimiter_end])
            index = delimiter_end
            continue
        closing = _find_backtick_run(line, delimiter_end, delimiter_length)
        if closing is None:
            return "".join(visible), delimiter_length
        _, index = closing
    return "".join(visible), active_length


def _is_fence_close(line: str, fence: tuple[str, int]) -> bool:
    match = FENCE_RE.match(line)
    if match is None:
        return False
    marker = match.group(1)
    return marker[0] == fence[0] and len(marker) >= fence[1] and not match.group(2).strip()


def _read_lines(path: Path) -> list[tuple[int, str]]:
    try:
        return list(enumerate(path.read_text(encoding="utf-8").splitlines(), 1))
    except (OSError, UnicodeDecodeError):
        return []


def _include_spec(raw_include: str) -> tuple[str, str | None]:
    include_path, separator, selector = raw_include.partition(":")
    return include_path, selector if separator else None


def _include_path(candidate: Path, raw_include: str) -> Path:
    include_path, _ = _include_spec(raw_include)
    return (candidate.parent / include_path).resolve()


def _selected_lines(path: Path, selector: str | None) -> list[tuple[int, str]]:
    lines = _read_lines(path)
    if selector is None:
        return lines

    range_parts = selector.split(":")
    if len(range_parts) == 2 and all(part == "" or part.isdigit() for part in range_parts):
        start = max(1, int(range_parts[0])) if range_parts[0] else 1
        end = int(range_parts[1]) if range_parts[1] else len(lines)
        return lines[start - 1 : end] if start <= end else []
    if selector.isdigit():
        line_number = int(selector)
        return [lines[line_number - 1]] if 1 <= line_number <= len(lines) else []

    start_index: int | None = None
    depth = 0
    for index, (_, line) in enumerate(lines):
        start_match = ANCHOR_START_RE.match(line)
        if start_match is not None and start_match.group(1) == selector:
            if start_index is None:
                start_index = index
            depth += 1
            continue
        end_match = ANCHOR_END_RE.match(line)
        if end_match is None or end_match.group(1) != selector or start_index is None:
            continue
        depth -= 1
        if depth == 0:
            return lines[start_index : index + 1]
    return []


def _is_within(root: Path, path: Path) -> bool:
    return path == root or root in path.parents


ScanLink = tuple[Path, int, str, Path]


def _scan_lines(
    lines: list[tuple[int, str]],
    source: Path,
    root: Path,
    context: Path,
    stack: tuple[Path, ...],
    state: tuple[tuple[str, int] | None, int | None] = (None, None),
) -> tuple[list[ScanLink], tuple[tuple[str, int] | None, int | None]]:
    links: list[ScanLink] = []
    fence, inline_code_length = state
    for line_index, (line_number, line) in enumerate(lines):
        match = FENCE_RE.match(line)
        if fence is None and match:
            marker = match.group(1)
            fence = (marker[0], len(marker))
            inline_code_length = None
            continue
        if fence is not None:
            if _is_fence_close(line, fence):
                fence = None
                inline_code_length = None
            continue
        quote_depth, content = _blockquote_content(line)
        if not line.strip() or (quote_depth > 0 and not content.strip()):
            inline_code_length = None
            continue
        if ATX_HEADING_RE.match(content) or THEMATIC_BREAK_RE.match(content):
            inline_code_length = None

        visible_line, inline_code_length = _strip_inline_code(
            line, inline_code_length, lines, line_index
        )
        for match in INLINE_LINK_RE.finditer(visible_line):
            links.append((source, line_number, match.group(1), context))
        reference = REFERENCE_LINK_RE.match(visible_line)
        if reference:
            links.append((source, line_number, reference.group(1), context))
        for include_match in INCLUDE_RE.finditer(visible_line):
            raw_include = include_match.group(1)
            included = _include_path(source, raw_include)
            if not included.is_file() or not _is_within(root, included) or included in stack:
                continue
            _, selector = _include_spec(raw_include)
            child_links, (fence, inline_code_length) = _scan_lines(
                _selected_lines(included, selector),
                included,
                root,
                context,
                stack + (included,),
                (fence, inline_code_length),
            )
            links.extend(child_links)
    return links, (fence, inline_code_length)


def find_broken_links(root: Path) -> list[tuple[str, int, str]]:
    root = root.resolve()
    broken: list[tuple[str, int, str]] = []
    seen: set[tuple[str, int, str]] = set()
    generated_targets = {
        (root / "reference/cli.md").as_posix(),
        (root / "reference/config.md").as_posix(),
    }
    for path in sorted(root.rglob("*.md")) + sorted(root.rglob("*.mdx")):
        if not path.is_file() or "_snippets" in path.parts:
            continue
        links, _ = _scan_lines(_read_lines(path), path, root, path, (path,))
        for source_path, line_number, raw_target, context in links:
            resolved = _local_target(raw_target, context)
            if resolved is None or resolved in generated_targets:
                continue
            if not Path(resolved).exists():
                entry = (source_path.as_posix(), line_number, resolved)
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
