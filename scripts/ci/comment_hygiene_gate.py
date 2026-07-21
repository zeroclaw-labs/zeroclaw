#!/usr/bin/env python3

"""Extract source comments and enforce the repository comment-hygiene policy."""

from __future__ import annotations

import io
import re
import sys
import tokenize
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional, Pattern


FINDINGS_EXIT = 1
FATAL_EXIT = 2


@dataclass
class Comment:
    path: str
    line: int
    source: str
    text: str
    family: str
    block_id: Optional[int] = None
    continues: bool = False


def add(
    out: list[Comment],
    path: str,
    lines: list[str],
    line: int,
    text: str,
    family: str,
    block_id: Optional[int] = None,
) -> None:
    out.append(Comment(path, line, lines[line - 1].rstrip("\n"), text, family, block_id))


def rust_comments(path: str, lines: list[str]) -> list[Comment]:
    out: list[Comment] = []
    block_depth = 0
    block_id = 0
    active_block: Optional[int] = None
    raw_end: Optional[str] = None
    in_string = False

    for lineno, line in enumerate(lines, 1):
        i = 0
        while i < len(line):
            if block_depth:
                start = i
                while i < len(line):
                    if line.startswith("/*", i):
                        block_depth += 1
                        i += 2
                    elif line.startswith("*/", i):
                        block_depth -= 1
                        i += 2
                        if block_depth == 0:
                            break
                    else:
                        i += 1
                add(out, path, lines, lineno, line[start:i], "rust_block", active_block)
                if block_depth == 0:
                    active_block = None
                continue

            if raw_end is not None:
                end = line.find(raw_end, i)
                if end < 0:
                    break
                i = end + len(raw_end)
                raw_end = None
                continue

            if in_string:
                while i < len(line):
                    if line[i] == "\\":
                        i += 2
                    elif line[i] == '"':
                        i += 1
                        in_string = False
                        break
                    else:
                        i += 1
                continue

            raw = re.match(r'(?:br|rb|r)(?P<hashes>#{0,255})"', line[i:])
            if raw:
                raw_end = '"' + raw.group("hashes")
                i += raw.end()
                continue

            if line.startswith('b"', i):
                in_string = True
                i += 2
                continue
            if line[i] == '"':
                in_string = True
                i += 1
                continue
            if line[i] == "'":
                end = i + 1
                escaped = False
                while end < len(line) and end - i <= 12:
                    if line[end] == "'" and not escaped:
                        i = end + 1
                        break
                    escaped = line[end] == "\\" and not escaped
                    if line[end] != "\\":
                        escaped = False
                    end += 1
                else:
                    i += 1
                continue
            if line.startswith("//", i):
                add(out, path, lines, lineno, line[i:].rstrip("\n"), "rust_line")
                break
            if line.startswith("/*", i):
                block_id += 1
                active_block = block_id
                block_depth = 1
                start = i
                i += 2
                while i < len(line):
                    if line.startswith("/*", i):
                        block_depth += 1
                        i += 2
                    elif line.startswith("*/", i):
                        block_depth -= 1
                        i += 2
                        if block_depth == 0:
                            break
                    else:
                        i += 1
                add(out, path, lines, lineno, line[start:i], "rust_block", active_block)
                if block_depth == 0:
                    active_block = None
                continue
            i += 1
    return out


def python_comments(path: str, raw: str, lines: list[str]) -> list[Comment]:
    out: list[Comment] = []
    for token in tokenize.generate_tokens(io.StringIO(raw).readline):
        if token.type == tokenize.COMMENT:
            add(out, path, lines, token.start[0], token.string, "hash_line")
    return out


def consume_toml_multiline(line: str, start: int, literal: bool) -> tuple[int, bool]:
    i = start
    while i < len(line):
        if not literal and line[i] == "\\":
            i += 2
            continue
        if line.startswith("'''" if literal else '\"\"\"', i):
            quote = "'" if literal else '"'
            run = 0
            while i + run < len(line) and line[i + run] == quote:
                run += 1
            if run >= 3:
                return i + run, True
        i += 1
    return i, False


def toml_comments(path: str, lines: list[str]) -> list[Comment]:
    out: list[Comment] = []
    multiline: Optional[str] = None
    for lineno, line in enumerate(lines, 1):
        i = 0
        while i < len(line):
            if multiline is not None:
                i, closed = consume_toml_multiline(line, i, multiline == "literal")
                if not closed:
                    break
                multiline = None
                continue
            if line.startswith('\"\"\"', i):
                i, closed = consume_toml_multiline(line, i + 3, False)
                if not closed:
                    multiline = "basic"
                    break
                continue
            if line.startswith("'''", i):
                i, closed = consume_toml_multiline(line, i + 3, True)
                if not closed:
                    multiline = "literal"
                    break
                continue
            if line[i] == '"':
                i += 1
                while i < len(line):
                    if line[i] == "\\":
                        i += 2
                    elif line[i] == '"':
                        i += 1
                        break
                    else:
                        i += 1
                continue
            if line[i] == "'":
                end = line.find("'", i + 1)
                i = len(line) if end < 0 else end + 1
                continue
            if line[i] == "#":
                add(out, path, lines, lineno, line[i:].rstrip("\n"), "hash_line")
                break
            i += 1
    return out


@dataclass
class Heredoc:
    delimiter: str
    strip_tabs: bool


def parse_shell_heredoc(line: str, start: int) -> Optional[tuple[Heredoc, int]]:
    if not line.startswith("<<", start) or line.startswith("<<<", start):
        return None
    if start > 0 and line[start - 1] == "<":
        return None

    i = start + 2
    strip_tabs = i < len(line) and line[i] == "-"
    if strip_tabs:
        i += 1
    while i < len(line) and line[i] in " \t":
        i += 1

    delimiter: list[str] = []
    quote: Optional[str] = None
    while i < len(line):
        ch = line[i]
        if quote == "single":
            if ch == "'":
                quote = None
            else:
                delimiter.append(ch)
            i += 1
            continue
        if quote == "double":
            if ch == '"':
                quote = None
                i += 1
            elif ch == "\\" and i + 1 < len(line):
                nxt = line[i + 1]
                if nxt in '$`"\\\n':
                    if nxt != "\n":
                        delimiter.append(nxt)
                else:
                    delimiter.extend(("\\", nxt))
                i += 2
            else:
                delimiter.append(ch)
                i += 1
            continue
        if ch in " \t\r\n;|&()<>#":
            break
        if ch == "'":
            quote = "single"
            i += 1
            continue
        if ch == '"':
            quote = "double"
            i += 1
            continue
        if ch == "\\" and i + 1 < len(line):
            delimiter.append(line[i + 1])
            i += 2
            continue
        delimiter.append(ch)
        i += 1

    if quote is not None or not delimiter:
        return None
    return Heredoc("".join(delimiter), strip_tabs), i


def shell_comments(path: str, lines: list[str]) -> list[Comment]:
    out: list[Comment] = []
    heredocs: list[Heredoc] = []
    quote: Optional[str] = None
    substitutions: list[tuple[Optional[str], int]] = []
    arithmetic_depth = 0

    for lineno, line in enumerate(lines, 1):
        if heredocs:
            current = heredocs[0]
            candidate = line.rstrip("\n")
            if current.strip_tabs:
                candidate = candidate.lstrip("\t")
            if candidate == current.delimiter:
                heredocs.pop(0)
            continue

        i = 0
        while i < len(line):
            ch = line[i]
            if arithmetic_depth:
                if ch == "(":
                    arithmetic_depth += 1
                elif ch == ")":
                    arithmetic_depth -= 1
                i += 1
                continue
            if quote == "single":
                if ch == "'":
                    quote = None
                i += 1
                continue
            if quote == "double":
                if ch == "\\":
                    i += 2
                    continue
                if line.startswith("$((", i):
                    arithmetic_depth = 2
                    i += 3
                    continue
                if line.startswith("$(", i):
                    substitutions.append((quote, 1))
                    quote = None
                    i += 2
                    continue
                if ch == '"':
                    quote = None
                i += 1
                continue

            if ch == "'":
                quote = "single"
                i += 1
                continue
            if ch == '"':
                quote = "double"
                i += 1
                continue
            if line.startswith("((", i):
                arithmetic_depth = 2
                i += 2
                continue
            if line.startswith("$((", i):
                arithmetic_depth = 2
                i += 3
                continue
            if line.startswith("$(", i):
                substitutions.append((None, 1))
                i += 2
                continue
            if substitutions and ch == "(":
                prior, depth = substitutions[-1]
                substitutions[-1] = (prior, depth + 1)
                i += 1
                continue
            if substitutions and ch == ")":
                prior, depth = substitutions[-1]
                depth -= 1
                if depth == 0:
                    substitutions.pop()
                    quote = prior
                else:
                    substitutions[-1] = (prior, depth)
                i += 1
                continue
            if line.startswith("<<", i):
                parsed = parse_shell_heredoc(line, i)
                if parsed:
                    heredoc, i = parsed
                    heredocs.append(heredoc)
                    continue
            if ch == "#" and (
                i == 0 or line[i - 1].isspace() or line[i - 1] in ";|&()"
            ):
                if not (lineno == 1 and i == 0 and line.startswith("#!")):
                    add(out, path, lines, lineno, line[i:].rstrip("\n"), "hash_line")
                break
            i += 1
    return out


def nix_comments(path: str, lines: list[str]) -> list[Comment]:
    out: list[Comment] = []
    mode = "normal"
    block_id = 0
    active_block: Optional[int] = None
    interpolation: list[tuple[str, int]] = []

    for lineno, line in enumerate(lines, 1):
        i = 0
        while i < len(line):
            if mode == "block":
                end = line.find("*/", i)
                if end < 0:
                    add(out, path, lines, lineno, line[i:].rstrip("\n"), "nix_block", active_block)
                    break
                add(out, path, lines, lineno, line[i : end + 2], "nix_block", active_block)
                i = end + 2
                mode = "normal"
                active_block = None
                continue

            if mode == "multi":
                if line.startswith("''${", i):
                    i += 4
                    continue
                if line.startswith("'''", i) or line.startswith("''\\", i):
                    i += 3
                    continue
                if line.startswith("${", i):
                    interpolation.append((mode, 1))
                    mode = "normal"
                    i += 2
                    continue
                if line.startswith("''", i):
                    mode = "normal"
                    i += 2
                    continue
                i += 1
                continue

            if mode == "double":
                if line[i] == "\\":
                    i += 2
                    continue
                if line.startswith("${", i):
                    interpolation.append((mode, 1))
                    mode = "normal"
                    i += 2
                    continue
                if line[i] == '"':
                    mode = "normal"
                i += 1
                continue

            if interpolation and line[i] == "{":
                return_mode, depth = interpolation[-1]
                interpolation[-1] = (return_mode, depth + 1)
                i += 1
                continue
            if interpolation and line[i] == "}":
                return_mode, depth = interpolation[-1]
                depth -= 1
                if depth == 0:
                    interpolation.pop()
                    mode = return_mode
                else:
                    interpolation[-1] = (return_mode, depth)
                i += 1
                continue
            if line.startswith("''", i):
                mode = "multi"
                i += 2
                continue
            if line[i] == '"':
                mode = "double"
                i += 1
                continue
            if line.startswith("/*", i):
                block_id += 1
                active_block = block_id
                mode = "block"
                start = i
                end = line.find("*/", i + 2)
                if end < 0:
                    add(out, path, lines, lineno, line[start:].rstrip("\n"), "nix_block", active_block)
                    break
                add(out, path, lines, lineno, line[start : end + 2], "nix_block", active_block)
                i = end + 2
                mode = "normal"
                active_block = None
                continue
            if line[i] == "#":
                add(out, path, lines, lineno, line[i:].rstrip("\n"), "hash_line")
                break
            i += 1
    return out


def extract(path: str) -> list[Comment]:
    with open(path, encoding="utf-8", errors="replace") as source:
        raw = source.read()
    lines = raw.splitlines(keepends=True)
    if not lines:
        return []
    ext = Path(path).suffix
    if ext == ".rs":
        return rust_comments(path, lines)
    if ext == ".py":
        return python_comments(path, raw, lines)
    if ext == ".toml":
        return toml_comments(path, lines)
    if ext == ".sh":
        return shell_comments(path, lines)
    if ext == ".nix":
        return nix_comments(path, lines)
    return []


def mark_continuations(comments: list[Comment]) -> None:
    by_path_line: dict[tuple[str, int], list[Comment]] = {}
    for comment in comments:
        by_path_line.setdefault((comment.path, comment.line), []).append(comment)
    for comment in comments:
        for nxt in by_path_line.get((comment.path, comment.line + 1), []):
            same_block = comment.block_id is not None and (
                comment.family == nxt.family and comment.block_id == nxt.block_id
            )
            same_line_family = comment.block_id is None and comment.family == nxt.family
            if same_block or same_line_family:
                comment.continues = True
                break


URL = re.compile(r"https?://\S+")
COLOR_ASSIGNMENT = re.compile(
    r"\b(color|background|foreground|fill|stroke)\s*[:=]\s*$", re.I
)
ISSUE_REF = re.compile(r"#(?P<number>[0-9]{3,})(?![A-Za-z0-9])")


def contains_issue_ref(text: str) -> bool:
    clean = URL.sub("", text)
    for match in ISSUE_REF.finditer(clean):
        digits = match.group("number")
        context = clean[max(0, match.start() - 40) : match.start()]
        if len(digits) in {3, 4, 6, 8} and COLOR_ASSIGNMENT.search(context):
            continue
        return True
    return False


@dataclass
class Detector:
    label: str
    matcher: Callable[[str], bool]
    dangling: bool = False


def regex_match(pattern: Pattern[str]) -> Callable[[str], bool]:
    return lambda text: pattern.search(text) is not None


DETECTORS = [
    Detector("issue/PR refs (#NNNN) in comments", contains_issue_ref),
    Detector(
        "tracking/see-issue phrasing in comments",
        regex_match(re.compile(r"tracking #|see #|see issue|see PR |fixes #|closes #|resolves #", re.I)),
    ),
    Detector(
        "review-process leakage in comments",
        regex_match(
            re.compile(
                r"NEW in this PR|previous revision of this PR|that started this PR|audit blocker|review pass|Round [0-9]+:",
                re.I,
            )
        ),
    ),
    Detector(
        "dated notes in comments",
        regex_match(re.compile(r"as of 20[0-9]{2}-[0-9]{2}|last verified:", re.I)),
    ),
    Detector(
        "RFC/section refs stripped mid-token (RFC-glued artifacts)",
        regex_match(re.compile(r"RFC(§|\s*\)|#?\s*$)")),
    ),
    Detector(
        "glued-word artifacts (line-join residue from comment deletion)",
        regex_match(re.compile(r"[a-z](?<!over)(?<!under)(?<!re)(without|exposes|therefore|because)\b")),
    ),
    Detector(
        "dangling open-paren fragments in comments",
        regex_match(re.compile(r"\((see|issue|ref|tracking|regression)\s*$", re.I)),
        True,
    ),
    Detector(
        "dangling trailing-reference words in comments",
        regex_match(re.compile(r"(—|-|,)\s*(see|ref)\s*$", re.I)),
        True,
    ),
    Detector(
        "bare See/Ref/Tracking stub comments",
        regex_match(re.compile(r"^(//+!?|#+)\s*(See|Ref|Tracking)\s*[.,;:]?\s*$")),
    ),
    Detector(
        "double-space lowercase stub comments (likely mid-sentence truncation)",
        regex_match(re.compile(r"^//[/!]?  (?!(?:since|itself)\b)[a-z]")),
    ),
]


def read_paths(path: str) -> list[str]:
    raw = Path(path).read_bytes()
    return [entry.decode("utf-8", errors="surrogateescape") for entry in raw.split(b"\0") if entry]


def main() -> int:
    if len(sys.argv) != 2:
        print("FATAL: expected a NUL-delimited input file list", file=sys.stderr)
        return FATAL_EXIT

    comments: list[Comment] = []
    for path in read_paths(sys.argv[1]):
        comments.extend(extract(path))
    mark_continuations(comments)

    failed = False
    for detector in DETECTORS:
        hits: list[str] = []
        for comment in comments:
            if detector.matcher(comment.text) and not (
                detector.dangling and comment.continues
            ):
                if detector.dangling:
                    hits.append(
                        f"{comment.path}:{comment.line}: dangling fragment "
                        "(next line is not a comment continuation)"
                    )
                else:
                    hits.append(f"{comment.path}:{comment.line}:{comment.source}")
        if hits:
            failed = True
            print(f"FAIL: {detector.label}")
            print("\n".join(hits[:50]))
            print()

    if failed:
        print("Comment hygiene gate failed. Fix the comment or, if a fixture")
        print("legitimately needs the pattern, add the path to SKIP_PATHS in")
        print("scripts/ci/comment_hygiene_gate.sh (reviewed via this script's diff).")
        return FINDINGS_EXIT

    print("Comment hygiene gate passed.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except SystemExit:
        raise
    except Exception as exc:
        print(f"FATAL: comment parser failed: {exc}", file=sys.stderr)
        raise SystemExit(FATAL_EXIT)
