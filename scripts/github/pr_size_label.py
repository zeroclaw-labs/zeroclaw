#!/usr/bin/env python3
"""Apply ZeroClaw PR size labels from GitHub pull-request metadata."""

from __future__ import annotations

import argparse
import http.client
import json
import os
import re
import sys
import urllib.parse
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import Any


SIZE_THRESHOLDS: tuple[tuple[str, int | None], ...] = (
    ("size:XS", 80),
    ("size:S", 250),
    ("size:M", 500),
    ("size:L", 1000),
    ("size:XL", None),
)
CANONICAL_SIZE_LABELS = {label for label, _ in SIZE_THRESHOLDS}
DOCS_LIKE_EXACT_PATHS = {
    "LICENSE",
    ".markdownlint-cli2.yaml",
    ".github/pull_request_template.md",
}
DOCS_LIKE_CONTRACT_SENTENCE = (
    "Docs-like files are paths under `docs/`, Markdown or MDX files, "
    "`.github/ISSUE_TEMPLATE/**`, `.github/pull_request_template.md`, "
    "`.markdownlint-cli2.yaml`, and `LICENSE`."
)
DEFAULT_DOCS_PATH = Path(__file__).resolve().parents[2] / "docs/book/src/maintainers/labels.md"
USER_AGENT = "zeroclaw-pr-size-labeler/1.0"


@dataclass(frozen=True)
class FileChange:
    filename: str
    additions: int
    deletions: int


@dataclass(frozen=True)
class SizePlan:
    effective_changed_lines: int
    selected_label: str
    labels_to_add: tuple[str, ...]
    labels_to_remove: tuple[str, ...]

    @property
    def changed(self) -> bool:
        return bool(self.labels_to_add or self.labels_to_remove)


class GitHubClient:
    def __init__(self, token: str, api_url: str = "https://api.github.com") -> None:
        if not token:
            raise ValueError("missing GitHub token")
        self.token = token
        self.api_url = api_url.rstrip("/")
        self.api_origin = urllib.parse.urlparse(self.api_url)
        if self.api_origin.scheme not in {"http", "https"} or not self.api_origin.netloc:
            raise ValueError(f"invalid GitHub API URL: {api_url!r}")

    def _parse_url(self, path_or_url: str) -> urllib.parse.ParseResult:
        if path_or_url.startswith("/"):
            return urllib.parse.urlparse(f"{self.api_url}{path_or_url}")
        parsed = urllib.parse.urlparse(path_or_url)
        if (
            parsed.scheme != self.api_origin.scheme
            or parsed.netloc != self.api_origin.netloc
            or not parsed.path.startswith("/")
        ):
            raise ValueError(f"refusing non-GitHub-API URL: {path_or_url!r}")
        return parsed

    def _send(
        self,
        method: str,
        path_or_url: str,
        payload: dict[str, Any] | None = None,
    ) -> tuple[int, dict[str, str], bytes]:
        parsed = self._parse_url(path_or_url)
        request_path = parsed.path
        if parsed.query:
            request_path = f"{request_path}?{parsed.query}"
        body = None if payload is None else json.dumps(payload).encode("utf-8")
        headers = {
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {self.token}",
            "User-Agent": USER_AGENT,
            "X-GitHub-Api-Version": "2022-11-28",
        }
        if body is not None:
            headers["Content-Type"] = "application/json"
        connection_class = (
            http.client.HTTPSConnection if parsed.scheme == "https" else http.client.HTTPConnection
        )
        connection = connection_class(parsed.netloc, timeout=30)
        try:
            connection.request(method, request_path, body=body, headers=headers)
            response = connection.getresponse()
            data = response.read()
            response_headers = {key: value for key, value in response.getheaders()}
            return response.status, response_headers, data
        finally:
            connection.close()

    def request(self, method: str, path_or_url: str, payload: dict[str, Any] | None = None) -> Any:
        status, _headers, data = self._send(method, path_or_url, payload)
        if status >= 400:
            raise GitHubHTTPError(status, data.decode("utf-8", errors="replace"))
        if not data:
            return None
        return json.loads(data.decode("utf-8"))

    def paginate(self, path: str) -> Iterator[dict[str, Any]]:
        url: str | None = path
        while url:
            status, headers, data = self._send("GET", url)
            if status >= 400:
                raise GitHubHTTPError(status, data.decode("utf-8", errors="replace"))
            page = json.loads(data.decode("utf-8"))
            if not isinstance(page, list):
                raise ValueError(f"expected list response from {url}")
            yield from page
            url = next_link(headers.get("Link"))


class GitHubHTTPError(RuntimeError):
    def __init__(self, status: int, message: str) -> None:
        super().__init__(f"GitHub API request failed with HTTP {status}: {message}")
        self.status = status


def next_link(link_header: str | None) -> str | None:
    if not link_header:
        return None
    for part in link_header.split(","):
        match = re.match(r'\s*<([^>]+)>;\s*rel="([^"]+)"', part)
        if match and match.group(2) == "next":
            return match.group(1)
    return None


def is_docs_like(path: str) -> bool:
    return (
        path.startswith("docs/")
        or path.startswith(".github/ISSUE_TEMPLATE/")
        or path.endswith(".md")
        or path.endswith(".mdx")
        or path in DOCS_LIKE_EXACT_PATHS
    )


def effective_changed_lines(files: list[FileChange]) -> int:
    total = 0
    for file in files:
        if is_docs_like(file.filename) or file.filename == "Cargo.lock":
            continue
        total += file.additions + file.deletions
    return total


def select_size_label(changed_lines: int) -> str:
    if changed_lines < 0:
        raise ValueError("changed_lines must not be negative")
    for label, limit in SIZE_THRESHOLDS:
        if limit is None or changed_lines <= limit:
            return label
    raise AssertionError("unreachable size threshold state")


def plan_size_label(files: list[FileChange], current_labels: set[str]) -> SizePlan:
    changed_lines = effective_changed_lines(files)
    selected = select_size_label(changed_lines)
    current_size_labels = current_labels & CANONICAL_SIZE_LABELS
    labels_to_add = () if selected in current_size_labels else (selected,)
    labels_to_remove = tuple(sorted(label for label in current_size_labels if label != selected))
    return SizePlan(
        effective_changed_lines=changed_lines,
        selected_label=selected,
        labels_to_add=labels_to_add,
        labels_to_remove=labels_to_remove,
    )


def file_change_from_api(payload: dict[str, Any]) -> FileChange:
    if not isinstance(payload, dict):
        raise ValueError(f"file entry must be an object: {payload!r}")
    filename = payload.get("filename")
    additions = payload.get("additions")
    deletions = payload.get("deletions")
    if not isinstance(filename, str) or not filename:
        raise ValueError(f"file entry missing filename: {payload!r}")
    if type(additions) is not int or additions < 0:
        raise ValueError(f"file entry has invalid additions for {filename!r}")
    if type(deletions) is not int or deletions < 0:
        raise ValueError(f"file entry has invalid deletions for {filename!r}")
    return FileChange(filename=filename, additions=additions, deletions=deletions)


def label_name_from_api(payload: Any) -> str:
    if isinstance(payload, str):
        return payload
    if isinstance(payload, dict) and isinstance(payload.get("name"), str):
        return payload["name"]
    raise ValueError(f"label entry missing name: {payload!r}")


def parse_repo(value: str) -> str:
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", value):
        raise ValueError(f"invalid repository slug: {value!r}")
    return value


def load_pr_changed_file_count(client: GitHubClient, repo: str, pr_number: int) -> int:
    encoded_repo = urllib.parse.quote(repo, safe="/")
    payload = client.request("GET", f"/repos/{encoded_repo}/pulls/{pr_number}")
    if not isinstance(payload, dict):
        raise ValueError(f"pull request metadata must be an object: {payload!r}")
    changed_files = payload.get("changed_files")
    if type(changed_files) is not int or changed_files < 0:
        raise ValueError(f"pull request metadata has invalid changed_files: {changed_files!r}")
    return changed_files


def load_pr_files(
    client: GitHubClient,
    repo: str,
    pr_number: int,
    expected_file_count: int,
) -> list[FileChange]:
    encoded_repo = urllib.parse.quote(repo, safe="/")
    files: list[FileChange] = []
    for entry in client.paginate(f"/repos/{encoded_repo}/pulls/{pr_number}/files?per_page=100"):
        files.append(file_change_from_api(entry))
    if len(files) != expected_file_count:
        raise ValueError(
            "refusing to classify incomplete PR file list: "
            f"fetched {len(files)} files, GitHub reports {expected_file_count}"
        )
    return files


def load_issue_labels(client: GitHubClient, repo: str, issue_number: int) -> set[str]:
    encoded_repo = urllib.parse.quote(repo, safe="/")
    entries = client.paginate(f"/repos/{encoded_repo}/issues/{issue_number}/labels?per_page=100")
    return {label_name_from_api(entry) for entry in entries}


def apply_size_plan(client: GitHubClient, repo: str, issue_number: int, plan: SizePlan) -> None:
    encoded_repo = urllib.parse.quote(repo, safe="/")
    for label in plan.labels_to_add:
        client.request(
            "POST",
            f"/repos/{encoded_repo}/issues/{issue_number}/labels",
            {"labels": [label]},
        )
    for label in plan.labels_to_remove:
        encoded_label = urllib.parse.quote(label, safe="")
        try:
            client.request("DELETE", f"/repos/{encoded_repo}/issues/{issue_number}/labels/{encoded_label}")
        except GitHubHTTPError as error:
            if error.status != 404:
                raise


def docs_thresholds(path: Path = DEFAULT_DOCS_PATH) -> dict[str, int | None]:
    thresholds: dict[str, int | None] = {}
    pattern = re.compile(r"^\|\s*`(size:[A-Z]+)`\s*\|\s*([^|]+?)\s*\|$")
    for line in path.read_text(encoding="utf-8").splitlines():
        match = pattern.match(line)
        if not match:
            continue
        label, threshold = match.groups()
        threshold = threshold.strip()
        if threshold.startswith(">"):
            thresholds[label] = None
            continue
        limit_match = re.search(r"(\d+)", threshold)
        if not limit_match:
            continue
        thresholds[label] = int(limit_match.group(1))
    return thresholds


def validate_docs_thresholds(path: Path = DEFAULT_DOCS_PATH) -> None:
    expected = dict(SIZE_THRESHOLDS)
    observed = docs_thresholds(path)
    if observed != expected:
        raise ValueError(f"size thresholds in {path} do not match script constants: {observed!r}")


def validate_docs_contract(path: Path = DEFAULT_DOCS_PATH) -> None:
    validate_docs_thresholds(path)
    text = path.read_text(encoding="utf-8")
    if DOCS_LIKE_CONTRACT_SENTENCE not in text:
        raise ValueError(f"docs-like exclusion contract in {path} does not match script constants")


def plan_as_json(plan: SizePlan, dry_run: bool) -> str:
    return json.dumps(
        {
            "dry_run": dry_run,
            "effective_changed_lines": plan.effective_changed_lines,
            "selected_label": plan.selected_label,
            "labels_to_add": list(plan.labels_to_add),
            "labels_to_remove": list(plan.labels_to_remove),
            "changed": plan.changed,
        },
        indent=2,
        sort_keys=True,
    )


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", ""))
    parser.add_argument("--pr", type=int, default=int(os.environ.get("PR_NUMBER", "0") or "0"))
    parser.add_argument("--token", default=os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN", ""))
    parser.add_argument("--api-url", default=os.environ.get("GITHUB_API_URL", "https://api.github.com"))
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--validate-docs", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    if args.validate_docs:
        validate_docs_contract()
        print("size label docs contract matches script constants")
        return 0
    repo = parse_repo(args.repo)
    if args.pr <= 0:
        raise ValueError("--pr must be a positive pull request number")
    client = GitHubClient(args.token, args.api_url)
    expected_file_count = load_pr_changed_file_count(client, repo, args.pr)
    files = load_pr_files(client, repo, args.pr, expected_file_count)
    current_labels = load_issue_labels(client, repo, args.pr)
    plan = plan_size_label(files, current_labels)
    print(plan_as_json(plan, args.dry_run))
    if not args.dry_run and plan.changed:
        apply_size_plan(client, repo, args.pr, plan)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
