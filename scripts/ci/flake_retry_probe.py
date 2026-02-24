#!/usr/bin/env python3
"""Run a single retry probe for failed test jobs and emit flake telemetry artifacts."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
import time
from pathlib import Path


def parse_bool(value: str) -> bool:
    return value.strip().lower() in {"1", "true", "yes", "y", "on"}


def run_retry(command: str) -> tuple[int, int]:
    started = time.perf_counter()
    proc = subprocess.run(command, shell=True, check=False)
    elapsed_ms = int((time.perf_counter() - started) * 1000)
    return (proc.returncode, elapsed_ms)


def build_markdown(report: dict) -> str:
    lines: list[str] = []
    lines.append("# Test Flake Retry Probe")
    lines.append("")
    lines.append(f"- Generated at: `{report['generated_at']}`")
    lines.append(f"- Initial test result: `{report['initial_test_result']}`")
    lines.append(f"- Retry attempted: `{report['retry_attempted']}`")
    lines.append(f"- Classification: `{report['classification']}`")
    lines.append(f"- Block on flake: `{report['block_on_flake']}`")
    if report["retry_attempted"]:
        lines.append(f"- Retry exit code: `{report['retry_exit_code']}`")
        lines.append(f"- Retry duration (ms): `{report['retry_duration_ms']}`")
    lines.append("")
    if report["classification"] == "flake_suspected":
        lines.append("Detected flaky pattern: first run failed, retry run passed.")
    elif report["classification"] == "persistent_failure":
        lines.append("Detected persistent failure: first run failed and retry run failed.")
    else:
        lines.append("No retry probe needed because initial test run did not fail.")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description="Emit flaky-test retry probe artifacts.")
    parser.add_argument("--initial-result", required=True, help="needs.test.result from workflow context")
    parser.add_argument("--retry-command", required=True, help="Command to rerun failed tests once")
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--block-on-flake", default="false", help="Whether suspected flakes should fail the job")
    args = parser.parse_args()

    initial = args.initial_result.strip().lower()
    block_on_flake = parse_bool(args.block_on_flake)

    retry_attempted = False
    retry_exit_code: int | None = None
    retry_duration_ms = 0
    classification = "not_applicable"

    if initial == "failure":
        retry_attempted = True
        retry_exit_code, retry_duration_ms = run_retry(args.retry_command)
        if retry_exit_code == 0:
            classification = "flake_suspected"
        else:
            classification = "persistent_failure"

    report = {
        "schema_version": "zeroclaw.audit.v1",
        "event_type": "test_flake_retry_probe",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "initial_test_result": initial,
        "retry_attempted": retry_attempted,
        "retry_exit_code": retry_exit_code,
        "retry_duration_ms": retry_duration_ms,
        "classification": classification,
        "block_on_flake": block_on_flake,
    }

    json_path = Path(args.output_json)
    md_path = Path(args.output_md)
    json_path.parent.mkdir(parents=True, exist_ok=True)
    md_path.parent.mkdir(parents=True, exist_ok=True)
    json_path.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    md_path.write_text(build_markdown(report), encoding="utf-8")

    if classification == "flake_suspected" and block_on_flake:
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
