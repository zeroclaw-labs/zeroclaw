#!/usr/bin/env python3
"""Queue hygiene helper for GitHub Actions workflow runs.

Default behavior is non-destructive (`dry-run`). Use `--apply` to cancel runs.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
from collections import Counter, defaultdict
from datetime import datetime, timezone
from typing import Any


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Cancel obsolete or superseded queued workflow runs safely.",
    )
    parser.add_argument(
        "--repo",
        default=os.getenv("GITHUB_REPOSITORY", "zeroclaw-labs/zeroclaw"),
        help="GitHub repository in owner/repo form.",
    )
    parser.add_argument(
        "--api-url",
        default=os.getenv("GITHUB_API_URL", "https://api.github.com"),
        help="GitHub API base URL.",
    )
    parser.add_argument(
        "--token",
        default="",
        help="GitHub token (default: GH_TOKEN/GITHUB_TOKEN, then `gh auth token`).",
    )
    parser.add_argument(
        "--status",
        default="queued",
        choices=["queued", "in_progress", "requested", "waiting"],
        help="Workflow run status to inspect (default: queued).",
    )
    parser.add_argument(
        "--runs-json",
        default="",
        help="Optional local JSON fixture for offline dry-run/testing (list or {workflow_runs:[...]}).",
    )
    parser.add_argument(
        "--obsolete-workflow",
        action="append",
        default=[],
        help="Workflow name to cancel unconditionally (repeatable).",
    )
    parser.add_argument(
        "--dedupe-workflow",
        action="append",
        default=[],
        help="Workflow name to dedupe by event+branch+PR-key, keeping newest run only (repeatable).",
    )
    parser.add_argument(
        "--dedupe-include-non-pr",
        action="store_true",
        help="Also dedupe non-PR runs (push/manual). Default dedupe scope is PR-originated runs only.",
    )
    parser.add_argument(
        "--non-pr-key",
        default="sha",
        choices=["sha", "branch"],
        help=(
            "Identity key mode for non-PR dedupe when --dedupe-include-non-pr is enabled: "
            "'sha' keeps one run per commit (default), 'branch' keeps one run per branch."
        ),
    )
    parser.add_argument(
        "--max-cancel",
        type=int,
        default=200,
        help="Maximum number of runs to cancel/apply in one execution.",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="Apply cancel operations. Default is dry-run.",
    )
    parser.add_argument(
        "--output-json",
        default="",
        help="Optional path to write structured report JSON.",
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="Print selected run details.",
    )
    return parser.parse_args()


class GitHubApi:
    def __init__(self, api_url: str, token: str | None) -> None:
        self.api_url = api_url.rstrip("/")
        self.token = token

    def _request(
        self,
        method: str,
        path: str,
        params: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        query = urllib.parse.urlencode(params or {}, doseq=True)
        url = f"{self.api_url}{path}"
        if query:
            url = f"{url}?{query}"
        req = urllib.request.Request(url, method=method)
        req.add_header("Accept", "application/vnd.github+json")
        req.add_header("X-GitHub-Api-Version", "2022-11-28")
        if self.token:
            req.add_header("Authorization", f"Bearer {self.token}")
        with urllib.request.urlopen(req, timeout=30) as resp:
            body = resp.read().decode("utf-8")
        if not body:
            return {}
        return json.loads(body)

    def get(self, path: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        return self._request("GET", path, params=params)

    def post(self, path: str) -> dict[str, Any]:
        return self._request("POST", path)

    def paginate(self, path: str, key: str, params: dict[str, Any] | None = None) -> list[dict[str, Any]]:
        results: list[dict[str, Any]] = []
        page = 1
        while True:
            query = {"per_page": 100, "page": page}
            if params:
                query.update(params)
            payload = self.get(path, params=query)
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


def normalize_values(values: list[str]) -> set[str]:
    out: set[str] = set()
    for value in values:
        item = value.strip()
        if item:
            out.add(item)
    return out


def parse_timestamp(value: str | None) -> datetime:
    if not value:
        return datetime.fromtimestamp(0, tz=timezone.utc)
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return datetime.fromtimestamp(0, tz=timezone.utc)


def run_identity_key(run: dict[str, Any], *, non_pr_key: str) -> tuple[str, str, str, str]:
    name = str(run.get("name", ""))
    event = str(run.get("event", ""))
    head_branch = str(run.get("head_branch", ""))
    head_sha = str(run.get("head_sha", ""))
    pr_number = ""
    pull_requests = run.get("pull_requests")
    if isinstance(pull_requests, list) and pull_requests:
        first = pull_requests[0]
        if isinstance(first, dict) and first.get("number") is not None:
            pr_number = str(first.get("number"))
    if pr_number:
        # For PR traffic, cancel stale runs across synchronize updates for the same PR.
        return (name, event, f"pr:{pr_number}", "")
    if non_pr_key == "branch":
        # Branch-level supersedence for push/manual lanes.
        return (name, event, head_branch, "")
    # SHA-level supersedence for push/manual lanes.
    return (name, event, head_branch, head_sha)


def collect_candidates(
    runs: list[dict[str, Any]],
    obsolete_workflows: set[str],
    dedupe_workflows: set[str],
    *,
    include_non_pr: bool,
    non_pr_key: str,
) -> tuple[list[dict[str, Any]], Counter[str]]:
    reasons_by_id: dict[int, set[str]] = defaultdict(set)
    runs_by_id: dict[int, dict[str, Any]] = {}

    for run in runs:
        run_id_raw = run.get("id")
        if run_id_raw is None:
            continue
        try:
            run_id = int(run_id_raw)
        except (TypeError, ValueError):
            continue
        runs_by_id[run_id] = run
        if str(run.get("name", "")) in obsolete_workflows:
            reasons_by_id[run_id].add("obsolete-workflow")

    by_workflow: dict[str, dict[tuple[str, str, str, str], list[dict[str, Any]]]] = defaultdict(
        lambda: defaultdict(list)
    )
    for run in runs:
        name = str(run.get("name", ""))
        if name not in dedupe_workflows:
            continue
        event = str(run.get("event", ""))
        is_pr_event = event in {"pull_request", "pull_request_target"}
        if not is_pr_event and not include_non_pr:
            continue
        pull_requests = run.get("pull_requests")
        has_pr_context = isinstance(pull_requests, list) and len(pull_requests) > 0
        if is_pr_event and not has_pr_context and not include_non_pr:
            continue
        key = run_identity_key(run, non_pr_key=non_pr_key)
        by_workflow[name][key].append(run)

    for groups in by_workflow.values():
        for group_runs in groups.values():
            if len(group_runs) <= 1:
                continue
            sorted_group = sorted(
                group_runs,
                key=lambda item: (
                    parse_timestamp(str(item.get("created_at", ""))),
                    int(item.get("id", 0)),
                ),
                reverse=True,
            )
            keep_id = int(sorted_group[0].get("id", 0))
            for stale in sorted_group[1:]:
                stale_id = int(stale.get("id", 0))
                reasons_by_id[stale_id].add(f"dedupe-superseded-by:{keep_id}")

    reason_counter: Counter[str] = Counter()
    selected: list[dict[str, Any]] = []
    for run_id, reasons in reasons_by_id.items():
        run = runs_by_id.get(run_id)
        if run is None:
            continue
        for reason in reasons:
            reason_counter[reason] += 1
        selected.append(
            {
                "id": run_id,
                "name": str(run.get("name", "")),
                "event": str(run.get("event", "")),
                "head_branch": str(run.get("head_branch", "")),
                "created_at": str(run.get("created_at", "")),
                "html_url": str(run.get("html_url", "")),
                "reasons": sorted(reasons),
            }
        )

    selected.sort(
        key=lambda item: (
            parse_timestamp(item.get("created_at", "")),
            int(item.get("id", 0)),
        )
    )
    return selected, reason_counter


def resolve_token(explicit_token: str) -> str:
    token = explicit_token or os.getenv("GH_TOKEN") or os.getenv("GITHUB_TOKEN") or ""
    if token:
        return token
    try:
        return subprocess.check_output(
            ["gh", "auth", "token"],
            text=True,
            stderr=subprocess.DEVNULL,
        ).strip()
    except Exception:
        return ""


def load_runs_from_json(path: str) -> list[dict[str, Any]]:
    payload = json.loads(open(path, "r", encoding="utf-8").read())
    if isinstance(payload, list):
        return [item for item in payload if isinstance(item, dict)]
    if isinstance(payload, dict):
        items = payload.get("workflow_runs", [])
        if isinstance(items, list):
            return [item for item in items if isinstance(item, dict)]
    raise ValueError("--runs-json must contain a list or an object with `workflow_runs` list.")


def main() -> int:
    args = parse_args()

    obsolete_workflows = normalize_values(args.obsolete_workflow)
    dedupe_workflows = normalize_values(args.dedupe_workflow)
    if not obsolete_workflows and not dedupe_workflows:
        print(
            "queue_hygiene: no policy configured. Provide --obsolete-workflow and/or --dedupe-workflow.",
            file=sys.stderr,
        )
        return 2

    owner, repo = split_repo(args.repo)
    token = resolve_token(args.token)
    if args.apply and not token:
        print(
            "queue_hygiene: apply mode requires authentication token "
            "(set GH_TOKEN/GITHUB_TOKEN, pass --token, or configure gh auth).",
            file=sys.stderr,
        )
        return 2
    api = GitHubApi(args.api_url, token)

    if args.runs_json:
        runs = load_runs_from_json(args.runs_json)
    else:
        runs = api.paginate(
            f"/repos/{owner}/{repo}/actions/runs",
            key="workflow_runs",
            params={"status": args.status},
        )

    selected, reason_counter = collect_candidates(
        runs,
        obsolete_workflows,
        dedupe_workflows,
        include_non_pr=args.dedupe_include_non_pr,
        non_pr_key=args.non_pr_key,
    )

    capped = selected[: max(0, args.max_cancel)]
    skipped_by_cap = max(0, len(selected) - len(capped))

    report: dict[str, Any] = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "repository": f"{owner}/{repo}",
        "status_scope": args.status,
        "mode": "apply" if args.apply else "dry-run",
        "policies": {
            "obsolete_workflows": sorted(obsolete_workflows),
            "dedupe_workflows": sorted(dedupe_workflows),
            "dedupe_include_non_pr": args.dedupe_include_non_pr,
            "non_pr_key": args.non_pr_key,
            "max_cancel": args.max_cancel,
        },
        "counts": {
            "runs_in_scope": len(runs),
            "candidate_runs_before_cap": len(selected),
            "candidate_runs_after_cap": len(capped),
            "skipped_by_cap": skipped_by_cap,
        },
        "reason_counts": dict(sorted(reason_counter.items())),
        "planned_actions": capped,
        "results": {
            "canceled": 0,
            "skipped": 0,
            "failed": 0,
            "failures": [],
        },
    }

    print("Queue Hygiene Report")
    print(f"repo: {report['repository']}")
    print(f"status_scope: {args.status}")
    print(
        "runs: in_scope={runs_in_scope} candidate_before_cap={before} candidate_after_cap={after} skipped_by_cap={skipped}".format(
            runs_in_scope=report["counts"]["runs_in_scope"],
            before=report["counts"]["candidate_runs_before_cap"],
            after=report["counts"]["candidate_runs_after_cap"],
            skipped=report["counts"]["skipped_by_cap"],
        )
    )
    if reason_counter:
        print("reason_counts:")
        for reason, count in sorted(reason_counter.items()):
            print(f"  - {reason}: {count}")

    if args.verbose:
        for item in capped:
            reasons = ",".join(item.get("reasons", []))
            print(
                f"  run_id={item['id']} workflow={item['name']} branch={item['head_branch']} "
                f"created_at={item['created_at']} reasons={reasons}"
            )

    if args.apply and args.runs_json:
        print("queue_hygiene: --apply cannot be used with --runs-json offline fixture.", file=sys.stderr)
        return 2

    if args.apply:
        for item in capped:
            run_id = int(item["id"])
            try:
                api.post(f"/repos/{owner}/{repo}/actions/runs/{run_id}/cancel")
                report["results"]["canceled"] += 1
            except urllib.error.HTTPError as exc:
                body = exc.read().decode("utf-8", errors="replace")
                if exc.code in (404, 409, 422):
                    report["results"]["skipped"] += 1
                else:
                    report["results"]["failed"] += 1
                    report["results"]["failures"].append(
                        {
                            "run_id": run_id,
                            "status_code": exc.code,
                            "body": body[:500],
                        }
                    )

        print(
            "apply_results: canceled={canceled} skipped={skipped} failed={failed}".format(
                canceled=report["results"]["canceled"],
                skipped=report["results"]["skipped"],
                failed=report["results"]["failed"],
            )
        )

    if args.output_json:
        with open(args.output_json, "w", encoding="utf-8") as handle:
            json.dump(report, handle, indent=2, sort_keys=True)
            handle.write("\n")

    if args.apply and report["results"]["failed"] > 0:
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
