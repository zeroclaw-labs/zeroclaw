#!/usr/bin/env python3
"""Report auditable pull-request risk evidence without mutating GitHub state."""

from __future__ import annotations

import argparse
import base64
from functools import lru_cache
from html import escape as html_escape
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from typing import Any, Iterable, NamedTuple
from urllib.parse import parse_qs, quote, urlencode, urlparse


DEFAULT_REPOSITORY = "zeroclaw-labs/zeroclaw"
HIGH_LABEL = "risk:high"
MANUAL_LABEL = "risk:manual"
SECURITY_LABEL = "domain:security"
RISK_LABELS = ("risk:low", "risk:medium", HIGH_LABEL)
PAGE_SIZE = 100
MAX_PAGES = 1000
MAX_PR_FILES = 3000
MAX_TEST_ONLY_SOURCE_FILES = 25
RUST_SUFFIX = ".rs"
LOW_PATH_GLOBS = (
    "docs/**",
    "**/*.md",
    "**/*.mdx",
    "LICENSE",
    ".github/ISSUE_TEMPLATE/**",
    ".github/pull_request_template.md",
    ".markdownlint-cli2.yaml",
    "fixtures/**",
    "**/fixtures/**",
    "**/__fixtures__/**",
    ".editorconfig",
    ".gitattributes",
    ".gitignore",
)
CFG_TEST_RE = re.compile(r"^\s*#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*$")
CFG_BOUNDARY_RE = re.compile(r"(?:#\s*\[\s*cfg\b|\bcfg\s*(?:!|_attr\s*\())")
HUNK_RE = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")
RAW_STRING_START_RE = re.compile(r'(?:br|cr|r)(?P<hashes>#{0,255})"')
CHAR_LITERAL_RE = re.compile(
    r"'(?:\\(?:[nrt0\\'\"]|x[0-9A-Fa-f]{2}|u\{[0-9A-Fa-f_]{1,6}\})|[^\\'\n])'"
)
REPOSITORY_RE = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
WORKFLOW_YAML_RE = re.compile(r"^\.github/workflows/[^/]+\.ya?ml$")
TEST_FIXTURE_RE = re.compile(r"(?:^|/)(?:test_|.*_test\.py$|tests?/|fixtures?/|__fixtures__/)")
WORKFLOW_SEMANTIC_RULES = {
    "workflow permission expansion",
    "OIDC token access",
    "elevated pull_request_target",
}
WORKFLOW_PERMISSION_KEYS = (
    "actions",
    "attestations",
    "checks",
    "contents",
    "deployments",
    "discussions",
    "id-token",
    "issues",
    "models",
    "packages",
    "pages",
    "pull-requests",
    "security-events",
    "statuses",
)
WORKFLOW_PERMISSION_KEY_RE = "(?:" + "|".join(re.escape(key) for key in WORKFLOW_PERMISSION_KEYS) + ")"
WORKFLOW_METADATA_KEY_RE = re.compile(r"^\s*(?:-\s*)?(?:name|run-name)\s*:")
WORKFLOW_WRITE_SCALAR_RE = r"""["']?write(?:-all)?["']?"""
WORKFLOW_READ_RESTRICTED_SCALAR_RE = r"""["']?(?:read(?:-all)?|none|\{\})["']?"""


class ContentRule(NamedTuple):
    name: str
    paths: tuple[str, ...]
    pattern: re.Pattern[str]


class RiskPolicy(NamedTuple):
    high_globs: tuple[str, ...]
    content_rules: tuple[ContentRule, ...]


class RiskReportError(RuntimeError):
    """A trusted policy or GitHub input cannot support a trustworthy report."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise RiskReportError(message)


def normalize_path(value: Any) -> str:
    require(isinstance(value, str), "file path is malformed")
    path = value.replace("\\", "/")
    require(
        path
        and not path.startswith("/")
        and "\x00" not in path
        and all(character >= " " and character != "\x7f" for character in path),
        "file path is malformed",
    )
    require(all(part not in {"", ".", ".."} for part in path.split("/")), "file path is malformed")
    return path


def parse_json_file(path: Path, description: str) -> Any:
    try:
        with path.open(encoding="utf-8") as stream:
            return json.load(stream)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise RiskReportError(f"{description} is invalid") from exc


def parse_content_rule(value: Any, seen_names: set[str]) -> ContentRule:
    require(isinstance(value, dict), "risk policy content rule is invalid")
    require(set(value) == {"name", "paths", "pattern"}, "risk policy content rule is invalid")
    name = value["name"]
    paths = value["paths"]
    pattern = value["pattern"]
    require(isinstance(name, str) and name and name not in seen_names, "risk policy content rule name is invalid")
    require(isinstance(paths, list) and paths, "risk policy content rule paths are invalid")
    normalized_paths: list[str] = []
    for rule_path in paths:
        require(
            isinstance(rule_path, str)
            and rule_path
            and "\x00" not in rule_path
            and not rule_path.startswith("/")
            and ".." not in rule_path.split("/"),
            "risk policy content rule path is invalid",
        )
        normalized_paths.append(rule_path)
    require(isinstance(pattern, str) and pattern, "risk policy content rule pattern is invalid")
    try:
        compiled = re.compile(pattern)
    except re.error as exc:
        raise RiskReportError("risk policy content rule pattern is invalid") from exc
    seen_names.add(name)
    return ContentRule(name, tuple(normalized_paths), compiled)


def load_policy(path: Path) -> RiskPolicy:
    payload = parse_json_file(path, "risk policy")
    require(isinstance(payload, dict) and set(payload) == {HIGH_LABEL}, "risk policy shape is invalid")
    rules = payload[HIGH_LABEL]
    require(isinstance(rules, list) and rules, "risk policy has no high-risk rules")

    globs: list[str] = []
    content_rules: list[ContentRule] = []
    seen_names: set[str] = set()
    for rule in rules:
        require(isinstance(rule, dict) and len(rule) == 1, "risk policy rule is invalid")
        if "changed-files" in rule:
            changed_files = rule["changed-files"]
            require(
                isinstance(changed_files, dict) and set(changed_files) == {"any-glob-to-any-file"},
                "risk policy changed-files rule is invalid",
            )
            values = changed_files["any-glob-to-any-file"]
            require(isinstance(values, list) and values, "risk policy glob list is empty")
            for value in values:
                require(
                    isinstance(value, str)
                    and value
                    and "\x00" not in value
                    and not value.startswith("/")
                    and ".." not in value.split("/"),
                    "risk policy glob is invalid",
                )
                globs.append(value)
        elif "changed-lines" in rule:
            changed_lines = rule["changed-lines"]
            require(
                isinstance(changed_lines, dict) and set(changed_lines) == {"any-rule-to-any-added-line"},
                "risk policy changed-lines rule is invalid",
            )
            values = changed_lines["any-rule-to-any-added-line"]
            require(isinstance(values, list) and values, "risk policy content rule list is empty")
            content_rules.extend(parse_content_rule(value, seen_names) for value in values)
        else:
            raise RiskReportError("risk policy rule is invalid")
    require(len(globs) == len(set(globs)), "risk policy contains duplicate globs")
    return RiskPolicy(tuple(globs), tuple(content_rules))


@lru_cache(maxsize=None)
def glob_regex(pattern: str) -> re.Pattern[str]:
    parts: list[str] = []
    index = 0
    while index < len(pattern):
        if pattern.startswith("**/", index):
            parts.append("(?:.*/)?")
            index += 3
        elif pattern.startswith("**", index):
            parts.append(".*")
            index += 2
        elif pattern[index] == "*":
            parts.append("[^/]*")
            index += 1
        elif pattern[index] == "?":
            parts.append("[^/]")
            index += 1
        elif pattern[index] == "[":
            end = pattern.find("]", index + 1)
            if end == -1:
                parts.append(r"\[")
                index += 1
            else:
                parts.append(pattern[index : end + 1])
                index = end + 1
        else:
            parts.append(re.escape(pattern[index]))
            index += 1
    return re.compile("^" + "".join(parts) + "$")


def glob_matches(path: str, pattern: str) -> bool:
    return glob_regex(pattern).match(path) is not None


def is_high_path(path: str, high_globs: Iterable[str]) -> list[str]:
    return [pattern for pattern in high_globs if glob_matches(path, pattern)]


def is_low_path(path: str) -> bool:
    return any(glob_matches(path, pattern) for pattern in LOW_PATH_GLOBS)


def matching_content_rules(path: str, content_rules: Iterable[ContentRule]) -> list[ContentRule]:
    return [rule for rule in content_rules if any(glob_matches(path, pattern) for pattern in rule.paths)]


def is_workflow_yaml(path: str) -> bool:
    return WORKFLOW_YAML_RE.fullmatch(path) is not None


def is_test_fixture_path(path: str) -> bool:
    return TEST_FIXTURE_RE.search(path) is not None


def active_content_rules(path: str, content_rules: Iterable[ContentRule]) -> list[ContentRule]:
    if is_test_fixture_path(path):
        return []
    rules = matching_content_rules(path, content_rules)
    if path.startswith(".github/workflows/") and not is_workflow_yaml(path):
        return [rule for rule in rules if rule.name not in WORKFLOW_SEMANTIC_RULES and rule.name != "release behavior"]
    return rules


def strip_yaml_comment(line: str) -> str:
    quote: str | None = None
    escaped = False
    for index, character in enumerate(line):
        if quote == '"':
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == quote:
                quote = None
            continue
        if quote == "'":
            if character == quote:
                quote = None
            continue
        if character in {"'", '"'}:
            quote = character
        elif character == "#":
            return line[:index].rstrip()
    return line.rstrip()


def workflow_semantic_line(line: str) -> str | None:
    stripped = strip_yaml_comment(line)
    if not stripped.strip() or WORKFLOW_METADATA_KEY_RE.match(stripped):
        return None
    return stripped


def workflow_permission_write(line: str) -> bool:
    key = rf"""["']?{WORKFLOW_PERMISSION_KEY_RE}["']?"""
    return any(
        re.search(pattern, line)
        for pattern in (
            rf"^\s*permissions\s*:\s*{WORKFLOW_WRITE_SCALAR_RE}\s*(?:#.*)?$",
            rf"^\s*{key}\s*:\s*{WORKFLOW_WRITE_SCALAR_RE}\s*(?:#.*)?$",
            rf"^\s*permissions\s*:\s*\{{[^}}]*{key}\s*:\s*{WORKFLOW_WRITE_SCALAR_RE}(?=\s*(?:[,}}#]|$))",
        )
    )


def workflow_permission_restriction(line: str) -> bool:
    key = rf"""["']?{WORKFLOW_PERMISSION_KEY_RE}["']?"""
    return any(
        re.search(pattern, line)
        for pattern in (
            rf"^\s*permissions\s*:\s*{WORKFLOW_READ_RESTRICTED_SCALAR_RE}\s*(?:#.*)?$",
            rf"^\s*{key}\s*:\s*{WORKFLOW_READ_RESTRICTED_SCALAR_RE}\s*(?:#.*)?$",
            rf"^\s*permissions\s*:\s*\{{[^}}]*{key}\s*:\s*{WORKFLOW_READ_RESTRICTED_SCALAR_RE}(?=\s*(?:[,}}#]|$))",
        )
    )


def workflow_oidc_write(line: str) -> bool:
    return any(
        re.search(pattern, line)
        for pattern in (
            r"""^\s*["']?id-token["']?\s*:\s*["']?write["']?\s*(?:#.*)?$""",
            r"""^\s*permissions\s*:\s*\{[^}]*["']?id-token["']?\s*:\s*["']?write["']?(?=\s*(?:[,}#]|$))""",
        )
    )


def workflow_pull_request_target(line: str) -> bool:
    event = r"""["']?pull_request_target["']?"""
    return any(
        re.search(pattern, line)
        for pattern in (
            r"^\s*pull_request_target\s*:\s*(?:#.*)?$",
            r"^\s*-\s*pull_request_target\s*(?:#.*)?$",
            r"^\s*on\s*:\s*\[[^\]]*\bpull_request_target\b[^\]]*\]\s*(?:#.*)?$",
            rf"^\s*on\s*:\s*{event}\s*(?:#.*)?$",
            rf"^\s*on\s*:\s*\{{[^}}]*{event}\s*:\s*",
        )
    )


def workflow_secret_access(line: str) -> bool:
    return (
        re.search(r"\$\{\{\s*secrets\.", line) is not None
        or re.search(r"""^\s*["']?secrets["']?\s*:""", line) is not None
    )


def workflow_release_behavior(line: str) -> bool:
    return any(
        re.search(pattern, line)
        for pattern in (
            r"^\s*(?:-\s*)?run\s*:\s*gh\s+release\s+(?:create|upload)\b",
            r"^\s*tags\s*:",
            r"^\s*-\s*[\"']?v[0-9]+\.[0-9]+\.[0-9]+",
        )
    )


def content_rule_matches(path: str, rule: ContentRule, line: str) -> bool:
    if is_workflow_yaml(path):
        semantic_line = workflow_semantic_line(line)
        if semantic_line is None:
            return False
        if rule.name == "workflow permission expansion":
            return workflow_permission_write(semantic_line)
        if rule.name == "OIDC token access":
            return workflow_oidc_write(semantic_line)
        if rule.name == "elevated pull_request_target":
            return workflow_pull_request_target(semantic_line)
        if rule.name == "secret access":
            return workflow_secret_access(semantic_line)
        if rule.name == "release behavior":
            return workflow_release_behavior(semantic_line)
        return rule.pattern.search(semantic_line) is not None
    return rule.pattern.search(line) is not None


def labels_from_pr(pr: dict[str, Any]) -> set[str]:
    values = pr.get("labels")
    require(isinstance(values, list), "PR labels are malformed")
    labels: set[str] = set()
    for value in values:
        require(
            isinstance(value, dict) and isinstance(value.get("name"), str) and bool(value["name"]),
            "PR label is malformed",
        )
        labels.add(value["name"])
    return labels


def parse_sha(value: Any, description: str) -> str:
    require(isinstance(value, str) and re.fullmatch(r"[0-9a-fA-F]{40}", value), f"{description} is missing")
    return value


def parse_repository(value: str) -> str:
    require(REPOSITORY_RE.fullmatch(value) is not None, "repository slug is malformed")
    return value


def parse_pr_metadata(pr: Any) -> tuple[str, str, set[str], int]:
    require(isinstance(pr, dict), "PR metadata is malformed")
    head = pr.get("head")
    base = pr.get("base")
    require(isinstance(head, dict) and isinstance(base, dict), "PR refs are malformed")
    changed_files = pr.get("changed_files")
    require(
        isinstance(changed_files, int)
        and not isinstance(changed_files, bool)
        and changed_files >= 0,
        "PR changed-files count is malformed",
    )
    return (
        parse_sha(head.get("sha"), "PR head SHA"),
        parse_sha(base.get("sha"), "PR base SHA"),
        labels_from_pr(pr),
        changed_files,
    )


def item_paths(item: dict[str, Any]) -> tuple[str, ...]:
    current = normalize_path(item.get("filename"))
    status = item.get("status")
    if status in {"renamed", "copied"}:
        return (current, normalize_path(item.get("previous_filename")))
    return (current,)


def file_bound_to_head(item: dict[str, Any], head_sha: str) -> bool:
    contents_url = item.get("contents_url")
    if isinstance(contents_url, str):
        refs = parse_qs(urlparse(contents_url).query).get("ref", [])
        return refs == [head_sha]

    for key, marker in (("raw_url", "raw"), ("blob_url", "blob")):
        value = item.get(key)
        if not isinstance(value, str):
            continue
        parts = urlparse(value).path.split("/")
        if len(parts) >= 5 and parts[3] == marker and parts[4] == head_sha:
            return True
    return False


def validate_files(files: Any, expected_count: int, head_sha: str | None = None) -> list[dict[str, Any]]:
    require(
        0 < expected_count <= MAX_PR_FILES,
        "PR file count is incomplete or exceeds the safe API boundary",
    )
    require(isinstance(files, list) and len(files) == expected_count, "PR file list is incomplete")
    result: list[dict[str, Any]] = []
    for item in files:
        require(isinstance(item, dict), "PR file record is malformed")
        status = item.get("status")
        require(
            isinstance(status, str)
            and status in {"added", "modified", "removed", "renamed", "copied"},
            "PR file status is malformed",
        )
        item_paths(item)
        for key in ("additions", "deletions", "changes"):
            value = item.get(key)
            require(
                isinstance(value, int) and not isinstance(value, bool) and value >= 0,
                "PR file counts are malformed",
            )
        if "patch" in item:
            require(item["patch"] is None or isinstance(item["patch"], str), "PR patch is malformed")
        if head_sha is not None:
            require(file_bound_to_head(item, head_sha), "PR file evidence is not bound to the captured head SHA")
        result.append(item)
    return result


def changed_line_content(patch: str) -> tuple[list[tuple[int, str]], list[tuple[int, str]], bool]:
    require(patch and not patch.endswith("..."), "complete patch is unavailable")
    old_line = new_line = 0
    old_remaining = new_remaining = 0
    old_changed: list[tuple[int, str]] = []
    new_changed: list[tuple[int, str]] = []
    saw_hunk = False

    for line in patch.splitlines():
        match = HUNK_RE.match(line)
        if match:
            require(old_remaining == 0 and new_remaining == 0, "patch hunk is incomplete")
            old_line = int(match.group(1))
            new_line = int(match.group(3))
            old_remaining = int(match.group(2) or "1")
            new_remaining = int(match.group(4) or "1")
            saw_hunk = True
            continue
        if not saw_hunk or line.startswith("\\"):
            continue
        if line.startswith("+++ ") or line.startswith("--- "):
            continue
        require(line.startswith((" ", "+", "-")), "patch line is malformed")
        if line.startswith("+"):
            require(new_remaining > 0, "patch adds beyond its hunk")
            new_changed.append((new_line, line[1:]))
            new_line += 1
            new_remaining -= 1
        elif line.startswith("-"):
            require(old_remaining > 0, "patch deletes beyond its hunk")
            old_changed.append((old_line, line[1:]))
            old_line += 1
            old_remaining -= 1
        else:
            require(old_remaining > 0 and new_remaining > 0, "patch context exceeds its hunk")
            old_line += 1
            new_line += 1
            old_remaining -= 1
            new_remaining -= 1

    require(saw_hunk and old_remaining == 0 and new_remaining == 0, "patch has incomplete hunks")
    return old_changed, new_changed, True


def parsed_patch_counts_match(
    item: dict[str, Any],
    old_changed: list[tuple[int, str]],
    new_changed: list[tuple[int, str]],
) -> bool:
    additions = len(new_changed)
    deletions = len(old_changed)
    return additions == item["additions"] and deletions == item["deletions"] and item["changes"] == additions + deletions


def source_text(payload: Any) -> str:
    require(isinstance(payload, dict), "source response is malformed")
    require(
        payload.get("encoding") == "base64" and isinstance(payload.get("content"), str),
        "source response is incomplete",
    )
    try:
        return base64.b64decode("".join(payload["content"].split()), validate=True).decode("utf-8")
    except (ValueError, UnicodeDecodeError) as exc:
        raise RiskReportError("source response is not valid UTF-8") from exc


def rust_structural_text(source: str) -> str:
    """Blank Rust comments and literals while preserving line and brace positions."""

    output = list(source)

    def blank(start: int, end: int) -> None:
        for position in range(start, end):
            if output[position] not in {"\n", "\r"}:
                output[position] = " "

    index = 0
    while index < len(source):
        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            end = len(source) if end == -1 else end
            blank(index, end)
            index = end
            continue

        if source.startswith("/*", index):
            depth = 1
            end = index + 2
            while end < len(source) and depth:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            require(depth == 0, "Rust source has an unterminated block comment")
            blank(index, end)
            index = end
            continue

        raw = RAW_STRING_START_RE.match(source, index)
        if raw and (index == 0 or not (source[index - 1].isalnum() or source[index - 1] == "_")):
            terminator = '"' + raw.group("hashes")
            end = source.find(terminator, raw.end())
            require(end != -1, "Rust source has an unterminated raw string")
            end += len(terminator)
            blank(index, end)
            index = end
            continue

        if source[index] == '"':
            end = index + 1
            escaped = False
            while end < len(source):
                character = source[end]
                if escaped:
                    escaped = False
                elif character == "\\":
                    escaped = True
                elif character == '"':
                    end += 1
                    break
                end += 1
            require(end <= len(source) and source[end - 1] == '"', "Rust source has an unterminated string")
            blank(index, end)
            index = end
            continue

        if source[index] == "'":
            character = CHAR_LITERAL_RE.match(source, index)
            if character:
                blank(index, character.end())
                index = character.end()
                continue

        index += 1
    return "".join(output)


def cfg_test_ranges(source: str) -> list[tuple[int, int]]:
    lines = source.splitlines()
    structural_lines = rust_structural_text(source).splitlines()
    ranges: list[tuple[int, int]] = []
    for index, line in enumerate(structural_lines):
        if not CFG_TEST_RE.match(line):
            continue
        next_index = index + 1
        while next_index < len(lines) and not lines[next_index].strip():
            next_index += 1
        require(next_index < len(lines), "cfg(test) item is incomplete")
        require(
            not structural_lines[next_index].lstrip().startswith("#"),
            "cfg(test) item has an ambiguous attribute boundary",
        )
        if "{" not in structural_lines[next_index]:
            end = next_index
            while end < len(lines) and ";" not in structural_lines[end]:
                end += 1
            require(end < len(lines), "cfg(test) item has no end")
            ranges.append((index + 1, end + 1))
            continue

        depth = 0
        opened = False
        end = next_index
        for line_index in range(next_index, len(lines)):
            for character in structural_lines[line_index]:
                if character == "{":
                    depth += 1
                    opened = True
                elif character == "}" and opened:
                    depth -= 1
            if opened and depth == 0:
                end = line_index
                break
        require(opened and depth == 0, "cfg(test) item has unbalanced braces")
        ranges.append((index + 1, end + 1))
    return ranges


def line_in_ranges(line_number: int, ranges: Iterable[tuple[int, int]]) -> bool:
    return any(start <= line_number <= end for start, end in ranges)


def structural_brace_changed(
    old_changed: Iterable[tuple[int, str]],
    new_changed: Iterable[tuple[int, str]],
    base_structural_lines: list[str],
    head_structural_lines: list[str],
) -> bool:
    for line_number, _ in old_changed:
        if 1 <= line_number <= len(base_structural_lines):
            line = base_structural_lines[line_number - 1]
            if "{" in line or "}" in line:
                return True
    for line_number, _ in new_changed:
        if 1 <= line_number <= len(head_structural_lines):
            line = head_structural_lines[line_number - 1]
            if "{" in line or "}" in line:
                return True
    return False


def strict_test_only_proof(
    files: list[dict[str, Any]],
    high_globs: tuple[str, ...],
    api: Any,
    base_sha: str,
    head_sha: str,
) -> tuple[bool, str]:
    high_rust_files = [
        item
        for item in files
        if normalize_path(item.get("filename")).endswith(RUST_SUFFIX)
        and is_high_path(normalize_path(item.get("filename")), high_globs)
    ]
    if not high_rust_files:
        return False, "no canonical high-risk Rust file"
    if len(high_rust_files) > MAX_TEST_ONLY_SOURCE_FILES:
        return False, "too many high-risk Rust files for bounded test-only proof"
    if any(item["status"] != "modified" for item in high_rust_files):
        return False, "test-only proof requires an unchanged Rust file path"
    high_rust_paths = {normalize_path(item.get("filename")) for item in high_rust_files}
    for item in files:
        paths = item_paths(item)
        if paths[0] in high_rust_paths:
            continue
        if not all(is_low_path(path) for path in paths):
            return False, "non-low or mixed production file is present"

    for item in high_rust_files:
        patch = item.get("patch")
        if not isinstance(patch, str):
            return False, "complete Rust patch is unavailable"
        try:
            old_changed, new_changed, _ = changed_line_content(patch)
        except RiskReportError as exc:
            return False, str(exc)
        additions = sum(1 for line in patch.splitlines() if line.startswith("+") and not line.startswith("+++"))
        deletions = sum(1 for line in patch.splitlines() if line.startswith("-") and not line.startswith("---"))
        if not old_changed and not new_changed:
            return False, "Rust patch has no changed lines"
        if additions != item["additions"] or deletions != item["deletions"] or item["changes"] != additions + deletions:
            return False, "Rust patch is truncated"
        changed_text = [
            line[1:]
            for line in patch.splitlines()
            if (line.startswith("+") and not line.startswith("+++"))
            or (line.startswith("-") and not line.startswith("---"))
        ]
        if any(CFG_BOUNDARY_RE.search(line) for line in changed_text):
            return False, "conditional-compilation boundary changed"

        path = normalize_path(item.get("filename"))
        try:
            base_source = source_text(api.get_source(path, base_sha))
            head_source = source_text(api.get_source(path, head_sha))
            base_lines = base_source.splitlines()
            head_lines = head_source.splitlines()
            base_structural_lines = rust_structural_text(base_source).splitlines()
            head_structural_lines = rust_structural_text(head_source).splitlines()
            for line_number, content in old_changed:
                require(
                    1 <= line_number <= len(base_lines) and base_lines[line_number - 1] == content,
                    "Rust patch does not match base source",
                )
            for line_number, content in new_changed:
                require(
                    1 <= line_number <= len(head_lines) and head_lines[line_number - 1] == content,
                    "Rust patch does not match head source",
                )
            if structural_brace_changed(
                old_changed,
                new_changed,
                base_structural_lines,
                head_structural_lines,
            ):
                return False, "Rust structural brace changed"
            base_ranges = cfg_test_ranges(base_source)
            head_ranges = cfg_test_ranges(head_source)
        except Exception as exc:
            return False, f"source evidence is unavailable: {exc}"
        if not all(line_in_ranges(line, base_ranges) for line, _ in old_changed):
            return False, "deleted Rust line is outside an existing cfg(test) item"
        if not all(line_in_ranges(line, head_ranges) for line, _ in new_changed):
            return False, "added Rust line is outside an existing cfg(test) item"
    return True, "complete diff is confined to existing cfg(test) items"


def content_evidence_for_file(item: dict[str, Any], content_rules: tuple[ContentRule, ...]) -> dict[str, Any] | None:
    path = normalize_path(item.get("filename"))
    rules = active_content_rules(path, content_rules)
    if not rules:
        return None

    patch = item.get("patch")
    if not isinstance(patch, str):
        return {
            "path": path,
            "content_rules": ["content-sensitive diff unavailable"],
        }
    try:
        old_changed, new_changed, _ = changed_line_content(patch)
    except RiskReportError:
        return {
            "path": path,
            "content_rules": ["content-sensitive diff unavailable"],
        }
    if not parsed_patch_counts_match(item, old_changed, new_changed):
        return {
            "path": path,
            "content_rules": ["content-sensitive diff unavailable"],
        }

    matches: list[str] = []
    line_numbers: list[int] = []
    for line_number, line in new_changed:
        for rule in rules:
            if content_rule_matches(path, rule, line):
                matches.append(rule.name)
                line_numbers.append(line_number)
    if is_workflow_yaml(path) and any(rule.name == "workflow permission expansion" for rule in rules):
        added_permission_lines = {
            line_number
            for line_number, line in new_changed
            if (semantic_line := workflow_semantic_line(line)) is not None
            and (workflow_permission_write(semantic_line) or workflow_permission_restriction(semantic_line))
        }
        for line_number, line in old_changed:
            semantic_line = workflow_semantic_line(line)
            if (
                line_number not in added_permission_lines
                and semantic_line is not None
                and workflow_permission_restriction(semantic_line)
            ):
                matches.append("workflow permission expansion")
                line_numbers.append(line_number)
    if not matches:
        return None
    return {
        "path": path,
        "content_rules": sorted(set(matches)),
        "line_numbers": sorted(set(line_numbers)),
    }


def classify(
    files: list[dict[str, Any]],
    policy: RiskPolicy,
    api: Any,
    base_sha: str,
    head_sha: str,
) -> dict[str, Any]:
    evidence: list[dict[str, Any]] = []
    high_match = False
    for item in files:
        for path in item_paths(item):
            patterns = is_high_path(path, policy.high_globs)
            if patterns:
                high_match = True
                evidence.append({"path": path, "high_globs": patterns})
        content_evidence = content_evidence_for_file(item, policy.content_rules)
        if content_evidence is not None:
            high_match = True
            evidence.append(content_evidence)

    exception = False
    detail = "not applicable"
    if high_match:
        exception, detail = strict_test_only_proof(files, policy.high_globs, api, base_sha, head_sha)
        proposed = "risk:medium" if exception else HIGH_LABEL
    elif all(all(is_low_path(path) for path in item_paths(item)) for item in files):
        proposed = "risk:low"
    else:
        proposed = "risk:medium"
    return {
        "proposed_risk": proposed,
        "matching_evidence": evidence,
        "exception_9530": {"applied": exception, "detail": detail},
    }


def build_report(pr: dict[str, Any], classification: dict[str, Any]) -> dict[str, Any]:
    head_sha, base_sha, live_labels, changed_files = parse_pr_metadata(pr)
    current_risk = [label for label in RISK_LABELS if label in live_labels]
    manual = MANUAL_LABEL in live_labels
    security = SECURITY_LABEL in live_labels
    mismatches: list[str] = []
    if len(current_risk) > 1:
        mismatches.append("multiple current risk labels")
    if current_risk and classification["proposed_risk"] not in current_risk:
        mismatches.append("proposed risk differs from current risk label")
    if manual:
        mismatches.append("risk:manual freezes future automatic risk replacement")
    return {
        "report_only": True,
        "head_sha": head_sha,
        "base_sha": base_sha,
        "changed_files": changed_files,
        "proposed_risk": classification["proposed_risk"],
        "matching_evidence": classification["matching_evidence"],
        "current_risk": current_risk,
        "risk_manual": manual,
        "mutation_freeze": manual,
        "domain_security": security,
        "exception_9530": classification["exception_9530"],
        "mismatches": mismatches,
        "mutations_attempted": False,
    }


def repository_path(repository: str, path: str) -> str:
    encoded_repository = quote(repository, safe="/")
    encoded_path = "/".join(quote(part, safe="") for part in path.split("/"))
    return f"/repos/{encoded_repository}/{encoded_path}"


class GitHubAPI:
    """Read-only GitHub REST client for the trusted workflow."""

    def __init__(self, repository: str, token: str, api_url: str | None = None) -> None:
        require(repository and token, "GitHub API credentials are missing")
        self.repository = parse_repository(repository)
        self.base_url = (api_url or os.environ.get("GITHUB_API_URL", "https://api.github.com")).rstrip("/")
        self.api_origin = urlparse(self.base_url)
        require(
            self.api_origin.scheme == "https"
            and bool(self.api_origin.netloc)
            and self.api_origin.username is None
            and self.api_origin.password is None
            and not self.api_origin.params
            and not self.api_origin.query
            and not self.api_origin.fragment,
            "GitHub API URL must be an HTTPS origin",
        )
        self.base_path = self.api_origin.path.rstrip("/")
        self.token = token

    def request_target(self, path: str) -> str:
        require(isinstance(path, str) and path.startswith("/"), "GitHub API path is malformed")
        parsed = urlparse(path)
        require(
            not parsed.scheme
            and not parsed.netloc
            and parsed.path.startswith("/")
            and not parsed.params
            and not parsed.fragment,
            "GitHub API path must stay on the configured API origin",
        )
        require(
            all(character >= " " and character != "\x7f" for character in path),
            "GitHub API path is malformed",
        )
        target = f"{self.base_path}{parsed.path}" if self.base_path else parsed.path
        if parsed.query:
            target = f"{target}?{parsed.query}"
        return target

    def request(self, method: str, path: str) -> Any:
        require(method == "GET", "risk report API is read-only")
        target = self.request_target(path)
        hostname = "github.com" if self.api_origin.netloc == "api.github.com" else self.api_origin.netloc
        environment = {
            **os.environ,
            "GH_TOKEN": self.token,
        }
        if self.api_origin.netloc != "api.github.com":
            environment["GH_ENTERPRISE_TOKEN"] = self.token
        try:
            result = subprocess.run(
                [
                    "gh",
                    "api",
                    "--method",
                    "GET",
                    "--hostname",
                    hostname,
                    "--header",
                    "Accept: application/vnd.github+json",
                    "--header",
                    "X-GitHub-Api-Version: 2022-11-28",
                    target,
                ],
                check=False,
                capture_output=True,
                env=environment,
                timeout=30,
            )
        except (OSError, TimeoutError, subprocess.SubprocessError) as exc:
            raise RiskReportError("GitHub API request failed") from exc
        if result.returncode != 0:
            raise RiskReportError("GitHub API request failed")
        try:
            return json.loads(result.stdout.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise RiskReportError("GitHub API returned invalid JSON") from exc

    def get_pull(self, number: int) -> Any:
        return self.request("GET", repository_path(self.repository, f"pulls/{number}"))

    def paginate(self, path: str, expected_count: int | None = None) -> list[Any]:
        if expected_count is not None:
            require(0 <= expected_count <= MAX_PR_FILES, "pagination file count is outside the safe API boundary")
        values: list[Any] = []
        for page in range(1, MAX_PAGES + 1):
            separator = "&" if "?" in path else "?"
            payload = self.request(
                "GET",
                f"{path}{separator}{urlencode({'per_page': PAGE_SIZE, 'page': page})}",
            )
            require(isinstance(payload, list), "GitHub paginated response is malformed")
            values.extend(payload)
            if expected_count is not None and len(values) > expected_count:
                raise RiskReportError("GitHub paginated response exceeds expected PR file count")
            if expected_count is not None and len(values) == expected_count:
                return values
            if len(payload) < PAGE_SIZE:
                return values
        raise RiskReportError("GitHub pagination did not terminate")

    def get_source(self, path: str, revision: str) -> Any:
        return self.request(
            "GET",
            f"{repository_path(self.repository, f'contents/{path}')}?{urlencode({'ref': revision})}",
        )


def evaluate(api: Any, pr_number: int, policy_path: Path) -> dict[str, Any]:
    policy = load_policy(policy_path)
    pr = api.get_pull(pr_number)
    head_sha, base_sha, live_labels, changed_file_count = parse_pr_metadata(pr)
    require(0 < changed_file_count <= MAX_PR_FILES, "PR file count is incomplete or exceeds the safe API boundary")
    files = validate_files(
        api.paginate(repository_path(api.repository, f"pulls/{pr_number}/files"), changed_file_count),
        changed_file_count,
        head_sha,
    )
    classification = classify(files, policy, api, base_sha, head_sha)

    latest_pr = api.get_pull(pr_number)
    latest_head, latest_base, latest_labels, latest_file_count = parse_pr_metadata(latest_pr)
    require(
        (latest_head, latest_base, latest_labels, latest_file_count)
        == (head_sha, base_sha, live_labels, changed_file_count),
        "PR metadata changed during evaluation",
    )
    report = build_report(latest_pr, classification)
    require(report["head_sha"] == head_sha, "PR head changed during evaluation")
    return report


def summary_text(value: Any) -> str:
    text = str(value).replace("\r", r"\r").replace("\n", r"\n")
    text = html_escape(text, quote=False)
    return re.sub(r"([\\`*_{}\[\]()#+\-.!|>~])", r"\\\1", text)


def evidence_summary(item: dict[str, Any]) -> str:
    parts: list[str] = []
    if item.get("high_globs"):
        parts.append("path: " + ", ".join(summary_text(pattern) for pattern in item["high_globs"]))
    if item.get("content_rules"):
        parts.append("content: " + ", ".join(summary_text(rule) for rule in item["content_rules"]))
    if item.get("line_numbers"):
        parts.append("lines: " + ", ".join(str(line) for line in item["line_numbers"]))
    return f"{summary_text(item['path'])} ({'; '.join(parts)})"


def human_summary(report: dict[str, Any]) -> str:
    evidence = report["matching_evidence"]
    evidence_text = (
        ", ".join(
            evidence_summary(item)
            for item in evidence
        )
        if evidence
        else "none"
    )
    current = ", ".join(summary_text(label) for label in report["current_risk"]) or "none"
    manual = "present; future automatic mutation is frozen" if report["risk_manual"] else "absent"
    security = "present; separate security-shaped review signal" if report["domain_security"] else "absent"
    exception = report["exception_9530"]
    return "\n".join(
        [
            "PR risk report (report-only)",
            f"Proposed risk: {report['proposed_risk']}",
            f"High-risk evidence: {evidence_text}",
            f"Current risk labels: {current}",
            f"risk:manual: {manual}",
            f"domain:security: {security}",
            f"#9530 test-only exception: {'applied' if exception['applied'] else 'not applied'} ({summary_text(exception['detail'])})",
            "GitHub label mutation: none",
            "Commit status mutation: none",
            "Approval-count enforcement: none",
        ]
    )


def write_summary(path: Path, report: dict[str, Any]) -> None:
    json_lines = json.dumps(report, indent=2, sort_keys=True, ensure_ascii=True).splitlines()
    path.write_text(
        "## PR risk report\n\n"
        + human_summary(report)
        + "\n\nJSON report:\n\n"
        + "\n".join(f"    {line}" for line in json_lines)
        + "\n",
        encoding="utf-8",
    )


def main(argv: list[str] | None = None, api: Any | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pr-number", type=int, required=True)
    parser.add_argument("--policy", type=Path, required=True)
    parser.add_argument("--repository", default=DEFAULT_REPOSITORY)
    parser.add_argument("--summary", type=Path)
    args = parser.parse_args(argv)

    try:
        require(args.pr_number > 0, "PR number is invalid")
        client = api or GitHubAPI(args.repository, os.environ.get("GH_TOKEN", ""))
        report = evaluate(client, args.pr_number, args.policy)
        summary = human_summary(report)
        if args.summary:
            write_summary(args.summary, report)
        print(summary)
        print(json.dumps(report, sort_keys=True, ensure_ascii=True))
        return 0
    except Exception as exc:
        error = {
            "report_only": True,
            "mutations_attempted": False,
            "error": str(exc),
        }
        if args.summary:
            try:
                json_lines = json.dumps(error, indent=2, sort_keys=True).splitlines()
                args.summary.write_text(
                    "## PR risk report\n\n"
                    + f"Risk report failed closed: {summary_text(exc)}\n\n"
                    + "JSON report:\n\n"
                    + "\n".join(f"    {line}" for line in json_lines)
                    + "\n",
                    encoding="utf-8",
                )
            except OSError:
                pass
        print(f"PR risk report failed closed: {exc}", file=sys.stderr)
        print(json.dumps(error, sort_keys=True))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
