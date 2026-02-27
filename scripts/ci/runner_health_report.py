#!/usr/bin/env python3
"""Self-hosted runner pool health report for a GitHub repository.

This script queries GitHub Actions runner and workflow-run state, then prints a
human-readable summary and optional JSON artifact.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Report self-hosted runner pool health and queue pressure.",
    )
    parser.add_argument(
        "--repo",
        default=os.getenv("GITHUB_REPOSITORY", "zeroclaw-labs/zeroclaw"),
        help="GitHub repository in owner/repo form (default: env GITHUB_REPOSITORY or zeroclaw-labs/zeroclaw).",
    )
    parser.add_argument(
        "--api-url",
        default=os.getenv("GITHUB_API_URL", "https://api.github.com"),
        help="GitHub API base URL.",
    )
    parser.add_argument(
        "--token",
        default="",
        help="GitHub token (default: GH_TOKEN/GITHUB_TOKEN, then `gh auth token` fallback).",
    )
    parser.add_argument(
        "--require-label",
        action="append",
        default=["self-hosted", "aws-india"],
        help="Required runner label; repeatable.",
    )
    parser.add_argument(
        "--min-online",
        type=int,
        default=3,
        help="Minimum required online runners matching labels.",
    )
    parser.add_argument(
        "--min-available",
        type=int,
        default=1,
        help="Minimum required online and idle runners matching labels.",
    )
    parser.add_argument(
        "--max-queued-runs",
        type=int,
        default=20,
        help="Maximum acceptable queued workflow runs.",
    )
    parser.add_argument(
        "--max-busy-ratio",
        type=float,
        default=0.90,
        help="Maximum acceptable busy ratio among online runners.",
    )
    parser.add_argument(
        "--output-json",
        default="",
        help="Optional path to write structured JSON report.",
    )
    parser.add_argument(
        "--fail-on-threshold",
        action="store_true",
        help="Exit non-zero if any threshold is violated.",
    )
    return parser.parse_args()


class GitHubApi:
    def __init__(self, api_url: str, token: str | None) -> None:
        self.api_url = api_url.rstrip("/")
        self.token = token

    def get(self, path: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        query = urllib.parse.urlencode(params or {}, doseq=True)
        url = f"{self.api_url}{path}"
        if query:
            url = f"{url}?{query}"
        req = urllib.request.Request(url)
        req.add_header("Accept", "application/vnd.github+json")
        req.add_header("X-GitHub-Api-Version", "2022-11-28")
        if self.token:
            req.add_header("Authorization", f"Bearer {self.token}")
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read().decode("utf-8"))

    def paginate(self, path: str, key: str, params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
        page = 1
        results: list[dict[str, Any]] = []
        while True:
            query = {"per_page": 100, "page": page}
            if params:
                query.update(params)
            payload = self.get(path, query)
            items = payload.get(key, [])
            if not items:
                break
            results.extend(items)
            if len(items) < 100:
                break
            page += 1
        return results


def split_repo(repo: str) -> tuple[str, str]:
    parts = repo.split("/", 1)
    if len(parts) != 2 or not parts[0] or not parts[1]:
        raise ValueError(f"Invalid --repo value: {repo!r}. Expected owner/repo.")
    return parts[0], parts[1]


def normalize_labels(labels: list[str]) -> list[str]:
    out: list[str] = []
    seen: set[str] = set()
    for value in labels:
        item = value.strip()
        if not item:
            continue
        if item in seen:
            continue
        out.append(item)
        seen.add(item)
    return out


def collect_report(args: argparse.Namespace) -> dict[str, Any]:
    owner, repo = split_repo(args.repo)
    required_labels = normalize_labels(args.require_label)
    token = args.token or os.getenv("GH_TOKEN") or os.getenv("GITHUB_TOKEN")
    if not token:
        try:
            token = subprocess.check_output(
                ["gh", "auth", "token"],
                text=True,
                stderr=subprocess.DEVNULL,
            ).strip()
        except Exception:
            token = ""

    api = GitHubApi(args.api_url, token)

    runners = api.paginate(
        f"/repos/{owner}/{repo}/actions/runners",
        key="runners",
    )

    matching_runners: list[dict[str, Any]] = []
    for runner in runners:
        names = {entry.get("name", "") for entry in runner.get("labels", [])}
        if all(label in names for label in required_labels):
            matching_runners.append(runner)

    queued_runs = api.paginate(
        f"/repos/{owner}/{repo}/actions/runs",
        key="workflow_runs",
        params={"status": "queued"},
    )
    in_progress_runs = api.paginate(
        f"/repos/{owner}/{repo}/actions/runs",
        key="workflow_runs",
        params={"status": "in_progress"},
    )

    total = len(matching_runners)
    online = sum(1 for runner in matching_runners if runner.get("status") == "online")
    offline = total - online
    online_busy = sum(
        1
        for runner in matching_runners
        if runner.get("status") == "online" and bool(runner.get("busy"))
    )
    available = online - online_busy
    busy_ratio = (online_busy / online) if online else 1.0

    alerts: list[dict[str, Any]] = []
    if online < args.min_online:
        alerts.append(
            {
                "id": "low-online-runners",
                "severity": "critical",
                "message": f"Online runners below threshold: {online} < {args.min_online}",
            }
        )
    if available < args.min_available:
        alerts.append(
            {
                "id": "low-available-runners",
                "severity": "critical",
                "message": f"Available runners below threshold: {available} < {args.min_available}",
            }
        )
    if len(queued_runs) > args.max_queued_runs:
        alerts.append(
            {
                "id": "queue-pressure",
                "severity": "critical",
                "message": f"Queued runs above threshold: {len(queued_runs)} > {args.max_queued_runs}",
            }
        )
    if busy_ratio > args.max_busy_ratio:
        alerts.append(
            {
                "id": "high-busy-ratio",
                "severity": "warning",
                "message": f"Busy ratio above threshold: {busy_ratio:.2%} > {args.max_busy_ratio:.2%}",
            }
        )
    if offline > 0:
        alerts.append(
            {
                "id": "offline-runners",
                "severity": "warning",
                "message": f"{offline} runners are offline in the target label pool.",
            }
        )

    queued_examples = [
        {
            "id": item.get("id"),
            "name": item.get("name"),
            "head_branch": item.get("head_branch"),
            "event": item.get("event"),
            "created_at": item.get("created_at"),
            "html_url": item.get("html_url"),
        }
        for item in queued_runs[:10]
    ]

    return {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "repository": f"{owner}/{repo}",
        "required_labels": required_labels,
        "runner_counts": {
            "total_matching": total,
            "online": online,
            "offline": offline,
            "online_busy": online_busy,
            "online_available": available,
            "online_busy_ratio": round(busy_ratio, 4),
        },
        "workflow_run_counts": {
            "queued": len(queued_runs),
            "in_progress": len(in_progress_runs),
        },
        "thresholds": {
            "min_online": args.min_online,
            "min_available": args.min_available,
            "max_queued_runs": args.max_queued_runs,
            "max_busy_ratio": args.max_busy_ratio,
        },
        "queued_run_examples": queued_examples,
        "alerts": alerts,
    }


def print_summary(report: dict[str, Any]) -> None:
    counts = report["runner_counts"]
    queue = report["workflow_run_counts"]
    print("Runner Pool Health Report")
    print(f"repo: {report['repository']}")
    print(f"labels: {', '.join(report['required_labels'])}")
    print(
        "runners:"
        f" total={counts['total_matching']} online={counts['online']} "
        f"available={counts['online_available']} busy={counts['online_busy']} offline={counts['offline']}"
    )
    print(
        "workflows:"
        f" queued={queue['queued']} in_progress={queue['in_progress']}"
    )
    print(f"generated_at: {report['generated_at']}")
    if report["alerts"]:
        print("alerts:")
        for alert in report["alerts"]:
            print(f"  - [{alert['severity']}] {alert['id']}: {alert['message']}")
    else:
        print("alerts: none")


def main() -> int:
    args = parse_args()
    try:
        report = collect_report(args)
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", errors="replace")
        print(
            f"error: GitHub API request failed ({exc.code} {exc.reason}): {body}",
            file=sys.stderr,
        )
        return 2
    except Exception as exc:  # pragma: no cover - defensive surface
        print(f"error: unexpected failure: {exc}", file=sys.stderr)
        return 2

    print_summary(report)

    if args.output_json:
        output_dir = os.path.dirname(args.output_json)
        if output_dir:
            os.makedirs(output_dir, exist_ok=True)
        with open(args.output_json, "w", encoding="utf-8") as handle:
            json.dump(report, handle, ensure_ascii=False, indent=2)
            handle.write("\n")

    if args.fail_on_threshold and report["alerts"]:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
