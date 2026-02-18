#!/usr/bin/env python3
"""Fetch GitHub Actions workflow runs for a given date and summarize costs."""

import json
import subprocess
import sys
from datetime import datetime, timezone


def fetch_runs(repo, date_str, page=1, per_page=100):
    """Fetch completed workflow runs for a given date."""
    url = (
        f"https://api.github.com/repos/{repo}/actions/runs"
        f"?created={date_str}&per_page={per_page}&page={page}"
    )
    result = subprocess.run(
        ["curl", "-sS", "-H", "Accept: application/vnd.github+json", url],
        capture_output=True, text=True
    )
    return json.loads(result.stdout)


def fetch_jobs(repo, run_id):
    """Fetch jobs for a specific run."""
    url = f"https://api.github.com/repos/{repo}/actions/runs/{run_id}/jobs?per_page=100"
    result = subprocess.run(
        ["curl", "-sS", "-H", "Accept: application/vnd.github+json", url],
        capture_output=True, text=True
    )
    return json.loads(result.stdout)


def parse_duration(started, completed):
    """Return duration in seconds between two ISO timestamps."""
    if not started or not completed:
        return 0
    try:
        s = datetime.fromisoformat(started.replace("Z", "+00:00"))
        c = datetime.fromisoformat(completed.replace("Z", "+00:00"))
        return max(0, (c - s).total_seconds())
    except Exception:
        return 0


def main():
    repo = "zeroclaw-labs/zeroclaw"
    date_str = "2026-02-17"

    print(f"Fetching workflow runs for {repo} on {date_str}...")
    print("=" * 100)

    all_runs = []
    for page in range(1, 5):  # up to 400 runs
        data = fetch_runs(repo, date_str, page=page)
        runs = data.get("workflow_runs", [])
        if not runs:
            break
        all_runs.extend(runs)
        if len(runs) < 100:
            break

    print(f"Total workflow runs found: {len(all_runs)}")
    print()

    # Group by workflow name
    workflow_stats = {}
    for run in all_runs:
        name = run.get("name", "Unknown")
        event = run.get("event", "unknown")
        conclusion = run.get("conclusion", "unknown")
        run_id = run.get("id")

        if name not in workflow_stats:
            workflow_stats[name] = {
                "count": 0,
                "events": {},
                "conclusions": {},
                "total_job_seconds": 0,
                "total_jobs": 0,
                "run_ids": [],
            }

        workflow_stats[name]["count"] += 1
        workflow_stats[name]["events"][event] = workflow_stats[name]["events"].get(event, 0) + 1
        workflow_stats[name]["conclusions"][conclusion] = workflow_stats[name]["conclusions"].get(conclusion, 0) + 1
        workflow_stats[name]["run_ids"].append(run_id)

    # For each workflow, sample up to 3 runs to get job-level timing
    print("Sampling job-level timing (up to 3 runs per workflow)...")
    print()

    for name, stats in workflow_stats.items():
        sample_ids = stats["run_ids"][:3]
        for run_id in sample_ids:
            jobs_data = fetch_jobs(repo, run_id)
            jobs = jobs_data.get("jobs", [])
            for job in jobs:
                started = job.get("started_at")
                completed = job.get("completed_at")
                duration = parse_duration(started, completed)
                stats["total_job_seconds"] += duration
                stats["total_jobs"] += 1

        # Extrapolate: if we sampled N runs but there are M total, scale up
        sampled = len(sample_ids)
        total = stats["count"]
        if sampled > 0 and sampled < total:
            scale = total / sampled
            stats["estimated_total_seconds"] = stats["total_job_seconds"] * scale
        else:
            stats["estimated_total_seconds"] = stats["total_job_seconds"]

    # Print summary sorted by estimated cost (descending)
    sorted_workflows = sorted(
        workflow_stats.items(),
        key=lambda x: x[1]["estimated_total_seconds"],
        reverse=True
    )

    print("=" * 100)
    print(f"{'Workflow':<40} {'Runs':>5} {'SampledJobs':>12} {'SampledMins':>12} {'Est.TotalMins':>14} {'Events'}")
    print("-" * 100)

    grand_total_minutes = 0
    for name, stats in sorted_workflows:
        sampled_mins = stats["total_job_seconds"] / 60
        est_total_mins = stats["estimated_total_seconds"] / 60
        grand_total_minutes += est_total_mins
        events_str = ", ".join(f"{k}={v}" for k, v in stats["events"].items())
        conclusions_str = ", ".join(f"{k}={v}" for k, v in stats["conclusions"].items())
        print(
            f"{name:<40} {stats['count']:>5} {stats['total_jobs']:>12} "
            f"{sampled_mins:>12.1f} {est_total_mins:>14.1f}   {events_str}"
        )
        print(f"{'':>40} {'':>5} {'':>12} {'':>12} {'':>14}   outcomes: {conclusions_str}")

    print("-" * 100)
    print(f"{'GRAND TOTAL':>40} {len(all_runs):>5} {'':>12} {'':>12} {grand_total_minutes:>14.1f}")
    print(f"\nEstimated total billable minutes on {date_str}: {grand_total_minutes:.0f} min ({grand_total_minutes/60:.1f} hours)")
    print()

    # Also show raw run list
    print("\n" + "=" * 100)
    print("DETAILED RUN LIST")
    print("=" * 100)
    for run in all_runs:
        name = run.get("name", "Unknown")
        event = run.get("event", "unknown")
        conclusion = run.get("conclusion", "unknown")
        run_id = run.get("id")
        started = run.get("run_started_at", "?")
        print(f"  [{run_id}] {name:<40} conclusion={conclusion:<12} event={event:<20} started={started}")


if __name__ == "__main__":
    main()
