#!/usr/bin/env python3
"""Orchestrate ZeroClaw factory roles with preview-first guardrails."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import subprocess
import sys
from pathlib import Path


DEFAULT_REPO = "zeroclaw-labs/zeroclaw"
ROOT = Path(__file__).resolve().parents[4]
DEFAULT_OUTPUT_DIR = Path("artifacts/factory-foreman")
TESTBENCH = ROOT / ".claude/skills/factory-testbench/scripts/factory_testbench.py"
CLERK = ROOT / ".claude/skills/factory-clerk/scripts/factory_clerk.py"
INSPECTOR = ROOT / ".claude/skills/factory-inspector/scripts/factory_inspector.py"


def utc_stamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def run_step(name: str, cmd: list[str]) -> dict[str, object]:
    result = subprocess.run(
        cmd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    return {
        "name": name,
        "cmd": cmd,
        "returncode": result.returncode,
        "stdout": result.stdout,
        "stderr": result.stderr,
    }


def write_outputs(summary: dict[str, object], output_dir: Path) -> Path:
    output_dir.mkdir(parents=True, exist_ok=True)
    path = output_dir / f"factory-foreman-{utc_stamp()}.json"
    path.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    latest = output_dir / "factory-foreman-latest.json"
    latest.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return path


def write_markdown_summary(summary: dict[str, object], path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = [
        "# Factory Foreman Summary",
        "",
        f"- Mode: `{summary['mode']}`",
        f"- Repo: `{summary['repo']}`",
        f"- Passed: `{summary['passed']}`",
        "",
        "## Steps",
        "",
    ]
    for step in summary["steps"]:  # type: ignore[index]
        lines.append(f"- `{step['name']}` exit `{step['returncode']}`")
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", DEFAULT_REPO))
    parser.add_argument("--mode", choices=("preview", "comment-only", "apply-safe"), default="preview")
    parser.add_argument("--allow-apply-safe", action="store_true")
    parser.add_argument("--max-mutations", type=int, default=25)
    parser.add_argument("--open-limit", type=int, default=1000)
    parser.add_argument("--merged-limit", type=int, default=250)
    parser.add_argument("--output-dir", default=str(DEFAULT_OUTPUT_DIR))
    parser.add_argument("--summary-file")
    parser.add_argument("--no-audit-file", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    if args.mode == "apply-safe" and not args.allow_apply_safe:
        print("apply-safe requires --allow-apply-safe", file=sys.stderr)
        return 2

    output_dir = Path(args.output_dir)
    steps: list[dict[str, object]] = []
    summary = {
        "schema": "factory-foreman-summary-v1",
        "repo": args.repo,
        "mode": args.mode,
        "generated_at": utc_stamp(),
        "passed": False,
        "steps": steps,
    }

    testbench_cmd = [
        "python3",
        str(TESTBENCH),
        "fixture-test",
        "--no-audit-file",
    ]
    steps.append(run_step("factory-testbench", testbench_cmd))
    if steps[-1]["returncode"] != 0:
        path = write_outputs(summary, output_dir)
        if args.summary_file:
            write_markdown_summary(summary, Path(args.summary_file))
        print(f"Factory Foreman failed in testbench. Summary file: {path}")
        return int(steps[-1]["returncode"])

    clerk_cmd = [
        "python3",
        str(CLERK),
        "--repo",
        args.repo,
        "--mode",
        args.mode,
        "--merged-limit",
        str(args.merged_limit),
        "--open-limit",
        str(args.open_limit),
        "--max-mutations",
        str(args.max_mutations),
        "--summary-file",
        str(output_dir / "factory-clerk-summary.md"),
        "--audit-dir",
        str(output_dir / "clerk"),
    ]
    if args.no_audit_file:
        clerk_cmd.append("--no-audit-file")
    steps.append(run_step("factory-clerk", clerk_cmd))
    if steps[-1]["returncode"] != 0:
        path = write_outputs(summary, output_dir)
        if args.summary_file:
            write_markdown_summary(summary, Path(args.summary_file))
        print(f"Factory Foreman failed in clerk. Summary file: {path}")
        return int(steps[-1]["returncode"])

    inspector_mode = "preview" if args.mode == "preview" else "comment-only"
    inspector_cmd = [
        "python3",
        str(INSPECTOR),
        "--repo",
        args.repo,
        "--mode",
        inspector_mode,
        "--limit",
        str(args.open_limit),
        "--max-mutations",
        str(args.max_mutations),
        "--summary-file",
        str(output_dir / "factory-inspector-summary.md"),
        "--audit-dir",
        str(output_dir / "inspector"),
    ]
    if args.no_audit_file:
        inspector_cmd.append("--no-audit-file")
    steps.append(run_step("factory-inspector", inspector_cmd))

    summary["passed"] = all(step["returncode"] == 0 for step in steps)
    path = write_outputs(summary, output_dir)
    if args.summary_file:
        write_markdown_summary(summary, Path(args.summary_file))

    print("Factory Foreman summary")
    print("=======================")
    for step in steps:
        print(f"- {step['name']}: exit {step['returncode']}")
    print(f"Summary file: {path}")
    return 0 if summary["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
