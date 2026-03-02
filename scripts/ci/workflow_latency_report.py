#!/usr/bin/env python3
"""Generate workflow latency summaries from GitHub Actions run/job payloads."""

from __future__ import annotations

import argparse
import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


def parse_timestamp(raw: str | None) -> datetime | None:
    if not raw:
        return None
    try:
        return datetime.fromisoformat(str(raw).replace("Z", "+00:00"))
    except ValueError:
        return None


def duration_seconds(start: datetime | None, end: datetime | None) -> int | None:
    if start is None or end is None:
        return None
    seconds = int((end - start).total_seconds())
    if seconds < 0:
        return None
    return seconds


def read_json(path: str) -> Any:
    return json.loads(Path(path).read_text(encoding="utf-8"))


def first_present(mapping: dict[str, Any], *keys: str) -> Any:
    for key in keys:
        if key in mapping and mapping[key] is not None:
            return mapping[key]
    return None


def normalize_history_runs(payload: Any) -> list[dict[str, Any]]:
    if isinstance(payload, list):
        return [item for item in payload if isinstance(item, dict)]
    if isinstance(payload, dict):
        if isinstance(payload.get("workflow_runs"), list):
            return [item for item in payload["workflow_runs"] if isinstance(item, dict)]
        if isinstance(payload.get("runs"), list):
            return [item for item in payload["runs"] if isinstance(item, dict)]
    return []


def percentile_seconds(values: list[int], percentile: int) -> int | None:
    if not values:
        return None
    sorted_values = sorted(values)
    index = int((percentile / 100.0) * (len(sorted_values) - 1))
    return sorted_values[index]


def format_seconds(seconds: int | None) -> str:
    if seconds is None:
        return "n/a"
    return f"{seconds}s"


def build_markdown(report: dict[str, Any]) -> str:
    lines = [
        "# Workflow Latency Summary",
        "",
        f"- Workflow: `{report['workflow_name']}`",
        f"- Run ID: `{report['current']['run_id']}`",
        f"- Queue delay: `{format_seconds(report['current']['queue_delay_seconds'])}`",
        f"- Execution: `{format_seconds(report['current']['execution_seconds'])}`",
        f"- Total wall: `{format_seconds(report['current']['run_wall_seconds'])}`",
        "",
        "## Key Steps",
    ]
    steps: dict[str, int | None] = report["current"]["steps"]
    if not steps:
        lines.append("- No step metrics configured.")
    else:
        for name, seconds in steps.items():
            lines.append(f"- `{name}`: `{format_seconds(seconds)}`")

    baseline = report["baseline"]
    lines.extend(
        [
            "",
            "## Recent Baseline",
            f"- Completed samples: `{baseline['count']}`",
            f"- p50: `{format_seconds(baseline['p50_seconds'])}`",
            f"- p90: `{format_seconds(baseline['p90_seconds'])}`",
            f"- max: `{format_seconds(baseline['max_seconds'])}`",
        ]
    )
    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Generate workflow latency summary artifacts.")
    parser.add_argument("--workflow-name", required=True)
    parser.add_argument("--current-run-json", required=True)
    parser.add_argument("--current-jobs-json", required=True)
    parser.add_argument("--history-runs-json", required=True)
    parser.add_argument("--step-name", action="append", default=[])
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    now = datetime.now(timezone.utc)

    run_payload = read_json(args.current_run_json)
    jobs_payload = read_json(args.current_jobs_json)
    history_payload = read_json(args.history_runs_json)

    run_created = parse_timestamp(first_present(run_payload, "created_at", "createdAt"))
    run_updated = parse_timestamp(first_present(run_payload, "updated_at", "updatedAt"))
    run_wall = duration_seconds(run_created, run_updated)

    jobs = jobs_payload.get("jobs", []) if isinstance(jobs_payload, dict) else []
    first_job = jobs[0] if jobs else {}
    job_created = parse_timestamp(first_present(first_job, "created_at", "createdAt"))
    job_started = parse_timestamp(first_present(first_job, "started_at", "startedAt"))
    job_completed = parse_timestamp(first_present(first_job, "completed_at", "completedAt"))
    queue_delay = duration_seconds(job_created, job_started)
    execution = duration_seconds(job_started, job_completed or now)

    steps_payload = first_job.get("steps", []) if isinstance(first_job, dict) else []
    step_duration_map: dict[str, int | None] = {}
    for step_name in args.step_name:
        duration: int | None = None
        for step in steps_payload:
            if step.get("name") != step_name:
                continue
            started = parse_timestamp(first_present(step, "started_at", "startedAt"))
            completed = parse_timestamp(first_present(step, "completed_at", "completedAt"))
            duration = duration_seconds(started, completed or now)
            break
        step_duration_map[step_name] = duration

    history_runs = normalize_history_runs(history_payload)
    completed_durations: list[int] = []
    for run in history_runs:
        status = str(first_present(run, "status", "Status") or "")
        conclusion = first_present(run, "conclusion", "Conclusion")
        if status != "completed":
            continue
        if conclusion not in ("success", "neutral", "skipped", None):
            continue
        created = parse_timestamp(first_present(run, "created_at", "createdAt"))
        updated = parse_timestamp(first_present(run, "updated_at", "updatedAt"))
        duration = duration_seconds(created, updated)
        if duration is not None:
            completed_durations.append(duration)

    report = {
        "schema_version": "zeroclaw.workflow-latency.v1",
        "workflow_name": args.workflow_name,
        "generated_at": now.isoformat(),
        "current": {
            "run_id": first_present(run_payload, "id", "databaseId"),
            "status": first_present(run_payload, "status"),
            "conclusion": first_present(run_payload, "conclusion"),
            "queue_delay_seconds": queue_delay,
            "execution_seconds": execution,
            "run_wall_seconds": run_wall,
            "steps": step_duration_map,
        },
        "baseline": {
            "count": len(completed_durations),
            "p50_seconds": percentile_seconds(completed_durations, 50),
            "p90_seconds": percentile_seconds(completed_durations, 90),
            "max_seconds": max(completed_durations) if completed_durations else None,
        },
    }

    output_json = Path(args.output_json)
    output_md = Path(args.output_md)
    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_md.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    output_md.write_text(build_markdown(report), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
