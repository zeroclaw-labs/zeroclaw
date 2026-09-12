#!/usr/bin/env python3
"""Print report-only pull-request review queues from live GitHub state."""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
import json
import math
from pathlib import Path
import subprocess
import sys
import unicodedata
from typing import Any, Callable, Iterable
from urllib.parse import quote_plus

REPOSITORY = "zeroclaw-labs/zeroclaw"
CORE_ROSTER_PATH = Path(__file__).resolve().parents[2] / "docs/book/src/contributing/communication.md"
QUEUES = ("near-ready", "maintainer", "second-core", "author-action", "stacked", "mine", "all")
MAX_WORKERS = 8
GH_TIMEOUT_SECONDS = 30


class GitHubReadError(RuntimeError):
    """A read-only GitHub request failed or returned an unusable shape."""


def run_gh(*args: str) -> Any:
    try:
        result = subprocess.run(
            ["gh", *args],
            check=True,
            capture_output=True,
            text=True,
            timeout=GH_TIMEOUT_SECONDS,
        )
    except subprocess.CalledProcessError as exc:
        detail = (exc.stderr or exc.stdout or str(exc)).strip()
        raise GitHubReadError(f"gh command failed: {detail}") from exc
    except subprocess.TimeoutExpired as exc:
        raise GitHubReadError(f"gh command timed out after {GH_TIMEOUT_SECONDS}s") from exc
    try:
        return json.loads(result.stdout or "null")
    except json.JSONDecodeError as exc:
        raise GitHubReadError("gh returned invalid JSON") from exc


def flatten_pages(payload: Any, source: str) -> list[dict[str, Any]]:
    if not isinstance(payload, list):
        raise GitHubReadError(f"unexpected {source}: expected a list")
    values = [item for page in payload for item in page] if payload and all(isinstance(page, list) for page in payload) else payload
    if not all(isinstance(item, dict) for item in values):
        raise GitHubReadError(f"unexpected {source}: expected objects")
    return values


def sanitize(value: Any) -> str:
    text = "?" if value is None else str(value)
    escaped: list[str] = []
    bidi = {"LRE", "RLE", "LRO", "RLO", "PDF", "LRI", "RLI", "FSI", "PDI"}
    for character in text:
        if character == "\n":
            escaped.append("\\n")
        elif character == "\r":
            escaped.append("\\r")
        elif character == "\t":
            escaped.append("\\t")
        elif unicodedata.category(character).startswith("C") or unicodedata.bidirectional(character) in bidi:
            escaped.append(f"\\u{ord(character):04x}")
        else:
            escaped.append(character)
    return "".join(escaped)


def login(value: Any) -> str | None:
    if isinstance(value, str):
        return value
    if isinstance(value, dict) and isinstance(value.get("login"), str):
        return value["login"]
    return None


def labels(pr: dict[str, Any]) -> set[str]:
    values = pr.get("labels", [])
    if not isinstance(values, list):
        return set()
    names: set[str] = set()
    for item in values:
        name = item if isinstance(item, str) else item.get("name") if isinstance(item, dict) else None
        if isinstance(name, str) and name:
            names.add(name)
    return names


def timestamp(event: dict[str, Any]) -> datetime | None:
    for key in ("submitted_at", "created_at", "createdAt", "authored_at", "date"):
        value = event.get(key)
        if isinstance(value, str):
            try:
                return datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone(timezone.utc)
            except ValueError:
                continue
    return None


def search_query(queue: str, author: str | None = None) -> str:
    base = f"repo:{REPOSITORY} is:pr is:open draft:false"
    if queue in {"near-ready", "maintainer", "mine", "second-core"}:
        query = f'{base} label:"needs-maintainer-review" -label:"needs-author-action" -label:"status:blocked" -label:"do-not-merge" -label:stacked'
        if queue == "near-ready":
            query += " status:success"
        if queue == "mine":
            query += f" author:{author or '<author>'}"
        if queue == "second-core":
            query += ' label:"risk:high","domain:security" review:approved'
        return query
    if queue == "author-action":
        return f'{base} label:"needs-author-action" -label:"status:blocked" -label:"do-not-merge"'
    if queue == "stacked":
        return f"{base} label:stacked"
    raise ValueError(f"no search query for {queue}")


def discover(queue: str, author: str | None, gh: Callable[..., Any] = run_gh) -> list[dict[str, Any]]:
    payload = gh(
        "pr",
        "list",
        "--repo",
        REPOSITORY,
        "--state",
        "open",
        "--limit",
        "1000",
        "--search",
        search_query(queue, author),
        "--json",
        "number,title,author,labels,url,headRefOid",
    )
    rows = flatten_pages(payload, f"{queue} discovery")
    required = {"number", "title", "author", "url"}
    if any(not required.issubset(row) for row in rows):
        raise GitHubReadError(f"incomplete {queue} discovery row")
    return rows


def load_core_roster(path: Path = CORE_ROSTER_PATH) -> set[str]:
    roster: set[str] = set()
    for line in path.read_text().splitlines():
        if not line.startswith("|") or "|---" in line:
            continue
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if len(cells) < 2 or not cells[1].startswith("Core Team"):
            continue
        first_cell = cells[0]
        for token in first_cell.split("@")[1:]:
            handle = token.split("]", 1)[0].strip()
            if handle:
                roster.add(handle.casefold())
    if not roster:
        raise GitHubReadError("published Core roster is empty")
    return roster


def fetch_reviews(pr: dict[str, Any], gh: Callable[..., Any]) -> list[dict[str, Any]]:
    payload = gh("api", "--paginate", "--slurp", f"repos/{REPOSITORY}/pulls/{pr['number']}/reviews?per_page=100")
    return flatten_pages(payload, f"PR #{pr['number']} reviews")


def latest_review_by_author(reviews: Iterable[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    latest: dict[str, tuple[tuple[datetime, int], dict[str, Any]]] = {}
    minimum = datetime.min.replace(tzinfo=timezone.utc)
    for review in reviews:
        if str(review.get("state", "")).upper() not in {"APPROVED", "CHANGES_REQUESTED", "DISMISSED"}:
            continue
        reviewer = login(review.get("user") or review.get("author"))
        if not reviewer:
            continue
        key = (timestamp(review) or minimum, int(review.get("id") or 0))
        normalized = reviewer.casefold()
        if normalized not in latest or key >= latest[normalized][0]:
            latest[normalized] = (key, review)
    return {reviewer: review for reviewer, (_, review) in latest.items()}


def second_core_row(pr: dict[str, Any], reviews: list[dict[str, Any]], core: set[str]) -> dict[str, Any] | None:
    head = pr.get("headRefOid")
    if not isinstance(head, str) or not head:
        return base_row(pr, "second-core", "unknown", "current head SHA unavailable")
    current: list[str] = []
    ambiguous: list[str] = []
    for reviewer, review in latest_review_by_author(reviews).items():
        if reviewer not in core or str(review.get("state", "")).upper() != "APPROVED":
            continue
        commit = review.get("commit_id") or review.get("commitId")
        if not isinstance(commit, str) or not commit:
            ambiguous.append(reviewer)
        elif commit == head:
            current.append(reviewer)
    if ambiguous:
        names = ", ".join("@" + name for name in sorted(ambiguous))
        return base_row(pr, "second-core", "unknown", f"Core approval missing commit SHA: {names}")
    if len(current) == 1:
        return base_row(pr, "second-core", "candidate", f"one current-head Core approval: @{current[0]}")
    return None


def fetch_timeline(pr: dict[str, Any], gh: Callable[..., Any]) -> list[dict[str, Any]]:
    payload = gh(
        "api",
        "--paginate",
        "--slurp",
        "-H",
        "Accept: application/vnd.github+json",
        f"repos/{REPOSITORY}/issues/{pr['number']}/timeline?per_page=100",
    )
    return flatten_pages(payload, f"PR #{pr['number']} timeline")


def event_label(event: dict[str, Any]) -> str | None:
    value = event.get("label")
    return value if isinstance(value, str) else value.get("name") if isinstance(value, dict) else None


def author_action_row(pr: dict[str, Any], timeline: list[dict[str, Any]], now: datetime, threshold: float) -> dict[str, Any] | None:
    active_start: datetime | None = None
    active_label_index: int | None = None
    for index, event in enumerate(timeline):
        kind = str(event.get("event") or event.get("type") or "").lower()
        if event_label(event) != "needs-author-action":
            continue
        if kind == "labeled":
            active_start = timestamp(event) or active_start
            active_label_index = index
        elif kind == "unlabeled":
            active_start = None
            active_label_index = None
    if active_start is None or active_label_index is None:
        return base_row(pr, "author-action", "unknown", "label start is missing from timeline")
    pr_author = (login(pr.get("author")) or "").casefold()
    for index, event in enumerate(timeline):
        if index <= active_label_index:
            continue
        kind = str(event.get("event") or event.get("type") or "").lower()
        if kind == "committed":
            return base_row(pr, "author-action", "unknown", "commit activity followed the request; unresolved age is uncertain")
        actor = (login(event.get("actor") or event.get("user") or event.get("author")) or "").casefold()
        if actor == pr_author and kind in {"commented", "reviewed"}:
            return base_row(pr, "author-action", "unknown", "author activity followed the request; unresolved age is uncertain")
    days = round(max(0.0, (now - active_start).total_seconds() / 86400), 1)
    if days < threshold:
        return None
    row = base_row(pr, "author-action", "candidate", f"unanswered label age: {days:g} days")
    row["wait_days"] = days
    return row


def base_row(pr: dict[str, Any], queue: str, status: str = "candidate", detail: str = "search match") -> dict[str, Any]:
    return {
        "number": pr["number"],
        "queue": queue,
        "author": sanitize(login(pr.get("author"))),
        "title": sanitize(pr.get("title")),
        "status": status,
        "detail": sanitize(detail),
        "wait_days": None,
        "labels": sorted(labels(pr)),
        "url": pr.get("url") or f"https://github.com/{REPOSITORY}/pull/{pr['number']}",
    }


def detail_rows(
    queue: str,
    prs: list[dict[str, Any]],
    older_than_days: float,
    now: datetime,
    gh: Callable[..., Any],
    core: set[str],
) -> list[dict[str, Any]]:
    if queue == "second-core":
        worker = lambda pr: second_core_row(pr, fetch_reviews(pr, gh), core)
    elif queue == "author-action":
        worker = lambda pr: author_action_row(pr, fetch_timeline(pr, gh), now, older_than_days)
    else:
        return [base_row(pr, queue) for pr in prs]
    with ThreadPoolExecutor(max_workers=min(MAX_WORKERS, len(prs) or 1)) as executor:
        return [row for row in executor.map(worker, prs) if row is not None]


def collect(
    queue: str,
    author: str | None,
    older_than_days: float,
    gh: Callable[..., Any] = run_gh,
    now: datetime | None = None,
    core: set[str] | None = None,
) -> list[dict[str, Any]]:
    lanes = ("maintainer", "second-core", "author-action", "stacked") if queue == "all" else (queue,)
    if queue == "all" and author:
        lanes += ("mine",)
    now = now or datetime.now(timezone.utc)
    core_error: str | None = None
    if core is None:
        try:
            core = load_core_roster() if "second-core" in lanes else set()
        except (GitHubReadError, OSError) as exc:
            core = set()
            core_error = str(exc)
    rows: list[dict[str, Any]] = []
    for lane in lanes:
        discovered = discover(lane, author, gh)
        if lane == "second-core" and core_error:
            rows.extend(base_row(pr, lane, "unknown", f"Core roster unavailable: {core_error}") for pr in discovered)
        else:
            rows.extend(detail_rows(lane, discovered, older_than_days, now, gh, core))
    return sorted(rows, key=lambda row: (row["queue"], row["number"]))


def render_table(rows: list[dict[str, Any]]) -> str:
    headers = ("PR", "QUEUE", "AUTHOR", "AGE", "STATUS", "TITLE", "DETAIL", "URL")
    values = [
        (
            f"#{row['number']}",
            row["queue"],
            row["author"],
            f"{row['wait_days']:g}d" if row["wait_days"] is not None else "?",
            row["status"],
            row["title"],
            row["detail"],
            row["url"],
        )
        for row in rows
    ]
    widths = [max([len(headers[index]), *(len(row[index]) for row in values)]) for index in range(len(headers))]
    lines = ["  ".join(value.ljust(widths[index]) for index, value in enumerate(headers))]
    lines.append("  ".join("-" * width for width in widths))
    lines.extend("  ".join(value.ljust(widths[index]) for index, value in enumerate(row)) for row in values)
    return "\n".join(lines) + "\n"


def render_links(queue: str, author: str | None) -> str:
    lanes = ("maintainer", "second-core", "author-action", "stacked") if queue == "all" else (queue,)
    if queue == "all" and author:
        lanes += ("mine",)
    lines = [f"{lane}: https://github.com/{REPOSITORY}/pulls?q={quote_plus(search_query(lane, author))}" for lane in lanes]
    if queue == "all" and not author:
        lines.append("mine: omitted; pass --author LOGIN to include it")
    return "\n".join(lines) + "\n"


def finite_nonnegative(value: str) -> float:
    try:
        parsed = float(value)
    except ValueError as exc:
        raise argparse.ArgumentTypeError("must be a finite non-negative number") from exc
    if not math.isfinite(parsed) or parsed < 0:
        raise argparse.ArgumentTypeError("must be a finite non-negative number")
    return parsed


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--queue", choices=QUEUES, required=True)
    parser.add_argument("--older-than-days", type=finite_nonnegative, default=7)
    parser.add_argument("--author", help="GitHub login for the mine queue")
    parser.add_argument("--format", choices=("table", "json", "links"), default="table")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None, gh: Callable[..., Any] = run_gh) -> int:
    args = parse_args(argv)
    if args.queue == "mine" and not args.author:
        print("--author is required for --queue mine", file=sys.stderr)
        return 2
    try:
        if args.format == "links":
            print(render_links(args.queue, args.author), end="")
            return 0
        rows = collect(args.queue, args.author, args.older_than_days, gh)
    except (GitHubReadError, OSError, ValueError) as exc:
        print(f"Failed to read GitHub state: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(rows, indent=2, sort_keys=True) if args.format == "json" else render_table(rows))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
