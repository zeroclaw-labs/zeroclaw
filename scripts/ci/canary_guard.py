#!/usr/bin/env python3
"""Evaluate canary health metrics against policy thresholds."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import sys
from pathlib import Path

SEMVER_TAG_RE = re.compile(r"^v\d+\.\d+\.\d+([.-][0-9A-Za-z.-]+)?$")


def parse_string_list(raw: object, *, field: str, violations: list[str]) -> list[str]:
    if raw is None:
        return []
    if not isinstance(raw, list):
        violations.append(f"Policy field `{field}` must be a list of strings.")
        return []
    out: list[str] = []
    for idx, item in enumerate(raw):
        if not isinstance(item, str) or not item.strip():
            violations.append(f"Policy field `{field}` has invalid entry at index {idx}.")
            continue
        out.append(item.strip())
    return out


def parse_cohorts(raw: object, violations: list[str]) -> list[dict[str, int | str]]:
    if raw is None:
        return []
    if not isinstance(raw, list):
        violations.append("Policy field `cohorts` must be a list.")
        return []

    cohorts: list[dict[str, int | str]] = []
    for idx, item in enumerate(raw):
        if not isinstance(item, dict):
            violations.append(f"Policy field `cohorts` entry {idx} must be an object.")
            continue
        name = item.get("name")
        traffic = item.get("traffic_percent")
        duration = item.get("duration_minutes")
        if not isinstance(name, str) or not name.strip():
            violations.append(f"Policy field `cohorts` entry {idx} missing non-empty `name`.")
            continue
        if not isinstance(traffic, int) or traffic <= 0 or traffic > 100:
            violations.append(f"Policy field `cohorts` entry {idx} has invalid `traffic_percent`.")
            continue
        if not isinstance(duration, int) or duration <= 0:
            violations.append(f"Policy field `cohorts` entry {idx} has invalid `duration_minutes`.")
            continue
        cohorts.append(
            {
                "name": name.strip(),
                "traffic_percent": traffic,
                "duration_minutes": duration,
            }
        )

    names = [str(item["name"]) for item in cohorts]
    if len(set(names)) != len(names):
        violations.append("Policy field `cohorts` must use unique cohort names.")

    traffic_steps = [int(item["traffic_percent"]) for item in cohorts]
    if traffic_steps != sorted(traffic_steps):
        violations.append("Policy field `cohorts` must be ordered by ascending `traffic_percent`.")

    return cohorts


def build_markdown(report: dict) -> str:
    lines: list[str] = []
    lines.append("# Canary Guard Report")
    lines.append("")
    lines.append(f"- Generated at: `{report['generated_at']}`")
    lines.append(f"- Candidate tag: `{report['candidate_tag']}`")
    lines.append(f"- Mode: `{report['mode']}`")
    lines.append(f"- Decision: `{report['decision']}`")
    lines.append(f"- Ready to execute: `{report['ready_to_execute']}`")
    lines.append("")

    lines.append("## Metrics")
    lines.append(f"- Error rate: `{report['metrics']['error_rate']}` (max `{report['thresholds']['max_error_rate']}`)")
    lines.append(f"- Crash rate: `{report['metrics']['crash_rate']}` (max `{report['thresholds']['max_crash_rate']}`)")
    lines.append(f"- P95 latency ms: `{report['metrics']['p95_latency_ms']}` (max `{report['thresholds']['max_p95_latency_ms']}`)")
    lines.append(f"- Sample size: `{report['metrics']['sample_size']}` (min `{report['minimum_sample_size']}`)")
    lines.append("")

    if report["cohorts"]:
        lines.append("## Cohorts")
        for item in report["cohorts"]:
            lines.append(
                f"- `{item['name']}`: {item['traffic_percent']}% traffic for {item['duration_minutes']} minutes"
            )
        lines.append("")

    if report["observability_signals"]:
        lines.append("## Observability Signals")
        for signal in report["observability_signals"]:
            lines.append(f"- `{signal}`")
        lines.append("")

    if report["violations"]:
        lines.append("## Violations")
        for item in report["violations"]:
            lines.append(f"- {item}")
        lines.append("")

    if report["warnings"]:
        lines.append("## Warnings")
        for item in report["warnings"]:
            lines.append(f"- {item}")
        lines.append("")

    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="Evaluate canary metrics and decide promote/hold/abort.")
    parser.add_argument("--policy-file", required=True)
    parser.add_argument("--candidate-tag", required=True)
    parser.add_argument("--candidate-sha", default="")
    parser.add_argument("--mode", choices=("dry-run", "execute"), default="dry-run")
    parser.add_argument("--error-rate", type=float, required=True)
    parser.add_argument("--crash-rate", type=float, required=True)
    parser.add_argument("--p95-latency-ms", type=float, required=True)
    parser.add_argument("--sample-size", type=int, required=True)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--fail-on-violation", action="store_true")
    args = parser.parse_args()

    policy = json.loads(Path(args.policy_file).read_text(encoding="utf-8"))
    violations: list[str] = []
    warnings: list[str] = []

    thresholds = policy.get("thresholds", {})
    cohorts = parse_cohorts(policy.get("cohorts"), violations)
    observability_signals = parse_string_list(
        policy.get("observability_signals"),
        field="observability_signals",
        violations=violations,
    )

    if not SEMVER_TAG_RE.fullmatch(args.candidate_tag):
        violations.append(
            f"Candidate tag `{args.candidate_tag}` does not match semver-like tag format (vX.Y.Z[-suffix])."
        )

    min_sample_size = int(policy.get("minimum_sample_size", 0))
    if args.sample_size < min_sample_size:
        violations.append(
            f"Insufficient sample size for canary decision: {args.sample_size} < required {min_sample_size}."
        )

    max_error_rate = float(thresholds.get("max_error_rate", 1.0))
    max_crash_rate = float(thresholds.get("max_crash_rate", 1.0))
    max_p95_latency_ms = float(thresholds.get("max_p95_latency_ms", 1e9))

    breach_ratio_error = args.error_rate / max_error_rate if max_error_rate > 0 else 999.0
    breach_ratio_crash = args.crash_rate / max_crash_rate if max_crash_rate > 0 else 999.0
    breach_ratio_latency = (
        args.p95_latency_ms / max_p95_latency_ms if max_p95_latency_ms > 0 else 999.0
    )
    max_ratio = max(breach_ratio_error, breach_ratio_crash, breach_ratio_latency)

    if max_ratio <= 1.0:
        decision = "promote"
    elif max_ratio <= 1.5:
        decision = "hold"
        warnings.append("One or more metrics exceeded threshold but stayed within soft breach margin (<=1.5x).")
    else:
        decision = "abort"
        warnings.append("One or more metrics exceeded hard breach margin (>1.5x).")

    if violations:
        decision = "hold"

    ready_to_execute = args.mode == "execute" and decision in {"promote", "abort"} and not violations

    report = {
        "schema_version": "zeroclaw.canary-guard.v1",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "policy_schema_version": policy.get("schema_version"),
        "candidate_tag": args.candidate_tag,
        "candidate_sha": args.candidate_sha or None,
        "mode": args.mode,
        "decision": decision,
        "ready_to_execute": ready_to_execute,
        "observation_window_minutes": int(policy.get("observation_window_minutes", 0)),
        "minimum_sample_size": min_sample_size,
        "cohorts": cohorts,
        "observability_signals": observability_signals,
        "thresholds": {
            "max_error_rate": max_error_rate,
            "max_crash_rate": max_crash_rate,
            "max_p95_latency_ms": max_p95_latency_ms,
        },
        "metrics": {
            "error_rate": args.error_rate,
            "crash_rate": args.crash_rate,
            "p95_latency_ms": args.p95_latency_ms,
            "sample_size": args.sample_size,
        },
        "breach_ratios": {
            "error_rate": round(breach_ratio_error, 4),
            "crash_rate": round(breach_ratio_crash, 4),
            "p95_latency_ms": round(breach_ratio_latency, 4),
        },
        "warnings": warnings,
        "violations": violations,
    }

    output_json = Path(args.output_json)
    output_md = Path(args.output_md)
    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_md.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    output_md.write_text(build_markdown(report), encoding="utf-8")

    if args.fail_on_violation and violations:
        print("canary guard violations found:", file=sys.stderr)
        for item in violations:
            print(f"- {item}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
