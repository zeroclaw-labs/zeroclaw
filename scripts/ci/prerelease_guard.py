#!/usr/bin/env python3
"""Validate pre-release stage transitions and tag integrity."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
import sys
from pathlib import Path

STABLE_TAG_RE = re.compile(r"^v(?P<version>\d+\.\d+\.\d+)$")
PRERELEASE_TAG_RE = re.compile(
    r"^v(?P<version>\d+\.\d+\.\d+)-(?P<stage>alpha|beta|rc)\.(?P<number>\d+)$"
)
STAGE_SEQUENCE = ["alpha", "beta", "rc", "stable"]
STAGE_RANK = {stage: index + 1 for index, stage in enumerate(STAGE_SEQUENCE)}


def run_git(args: list[str], *, cwd: Path) -> str:
    proc = subprocess.run(
        ["git", *args],
        cwd=str(cwd),
        text=True,
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed ({proc.returncode}): {proc.stderr.strip()}")
    return proc.stdout.strip()


def parse_tag(tag: str) -> tuple[str, str, int | None]:
    stable_match = STABLE_TAG_RE.fullmatch(tag)
    if stable_match:
        return (stable_match.group("version"), "stable", None)

    pre_match = PRERELEASE_TAG_RE.fullmatch(tag)
    if pre_match:
        return (
            pre_match.group("version"),
            pre_match.group("stage"),
            int(pre_match.group("number")),
        )

    raise ValueError(
        f"Tag `{tag}` must be `vX.Y.Z` or `vX.Y.Z-(alpha|beta|rc).N` (for example `v0.2.0-rc.1`)."
    )


def parse_stage_policy(policy: dict) -> tuple[list[str], dict[str, str], dict[str, list[str]], list[str]]:
    violations: list[str] = []

    stage_order = STAGE_SEQUENCE.copy()
    stage_order_raw = policy.get("stage_order")
    if not isinstance(stage_order_raw, list):
        violations.append("Policy field `stage_order` must be an array of stage names.")
    else:
        normalized_order: list[str] = []
        for item in stage_order_raw:
            if not isinstance(item, str) or not item.strip():
                violations.append("Policy field `stage_order` contains an invalid stage entry.")
                continue
            normalized_order.append(item.strip())
        if normalized_order != STAGE_SEQUENCE:
            violations.append(
                f"Policy field `stage_order` must be exactly {STAGE_SEQUENCE!r}."
            )
        else:
            stage_order = normalized_order

    expected_previous = {stage_order[idx]: stage_order[idx - 1] for idx in range(1, len(stage_order))}
    required_previous: dict[str, str] = dict(expected_previous)
    required_previous_raw = policy.get("required_previous_stage")
    if not isinstance(required_previous_raw, dict):
        violations.append("Policy field `required_previous_stage` must be an object.")
        required_previous_raw = {}

    for stage, expected in expected_previous.items():
        configured = required_previous_raw.get(stage)
        if configured != expected:
            violations.append(
                f"Policy requires `required_previous_stage.{stage} = {expected}`, got `{configured}`."
            )

    extra_previous = sorted(set(required_previous_raw) - set(expected_previous))
    if extra_previous:
        violations.append(
            f"Policy field `required_previous_stage` contains unknown stage keys: {', '.join(extra_previous)}."
        )

    required_checks = {stage: [] for stage in stage_order}
    required_checks_raw = policy.get("required_checks")
    if not isinstance(required_checks_raw, dict):
        violations.append("Policy field `required_checks` must be an object.")
        required_checks_raw = {}

    for stage in stage_order:
        checks_raw = required_checks_raw.get(stage)
        if not isinstance(checks_raw, list) or not checks_raw:
            violations.append(
                f"Policy requires non-empty `required_checks.{stage}` stage check list."
            )
            continue
        checks: list[str] = []
        seen_checks: set[str] = set()
        for item in checks_raw:
            if not isinstance(item, str) or not item.strip():
                violations.append(
                    f"Policy field `required_checks.{stage}` contains an invalid check name."
                )
                continue
            check_name = item.strip()
            if check_name in seen_checks:
                violations.append(
                    f"Policy field `required_checks.{stage}` contains duplicate check `{check_name}`."
                )
                continue
            checks.append(check_name)
            seen_checks.add(check_name)
        required_checks[stage] = checks

    extra_check_stages = sorted(set(required_checks_raw) - set(stage_order))
    if extra_check_stages:
        violations.append(
            f"Policy field `required_checks` contains unknown stage keys: {', '.join(extra_check_stages)}."
        )

    return stage_order, required_previous, required_checks, violations


def stage_sort_key(stage: str, stage_number: int | None) -> tuple[int, int]:
    return (STAGE_RANK.get(stage, 0), stage_number or 0)


def highest_stage_entry(entries: list[dict[str, object]]) -> dict[str, object] | None:
    if not entries:
        return None
    return max(
        entries,
        key=lambda item: stage_sort_key(
            str(item["stage"]),
            int(item["stage_number"]) if item["stage_number"] is not None else None,
        ),
    )


def parse_stage_entries(tags: list[str]) -> list[dict[str, object]]:
    entries: list[dict[str, object]] = []
    for candidate in tags:
        try:
            _, sibling_stage, sibling_stage_number = parse_tag(candidate)
        except ValueError:
            continue
        entries.append(
            {
                "tag": candidate,
                "stage": sibling_stage,
                "stage_number": sibling_stage_number,
                "rank": STAGE_RANK.get(sibling_stage, 0),
            }
        )
    entries.sort(
        key=lambda item: stage_sort_key(
            str(item["stage"]),
            int(item["stage_number"]) if item["stage_number"] is not None else None,
        )
        + (str(item["tag"]),)
    )
    return entries


def build_markdown(report: dict) -> str:
    lines: list[str] = []
    lines.append("# Pre-release Guard Report")
    lines.append("")
    lines.append(f"- Generated at: `{report['generated_at']}`")
    lines.append(f"- Tag: `{report['tag']}`")
    lines.append(f"- Stage: `{report['stage']}`")
    lines.append(f"- Mode: `{report['mode']}`")
    lines.append(f"- Ready to publish: `{report['ready_to_publish']}`")
    lines.append("")

    lines.append("## Current Stage Required Checks")
    required_checks = report.get("required_checks", [])
    if required_checks:
        for check_name in required_checks:
            lines.append(f"- `{check_name}`")
    else:
        lines.append("- none configured")
    lines.append("")

    stage_gate_matrix = report.get("stage_gate_matrix", [])
    if stage_gate_matrix:
        lines.append("## Stage Gate Matrix")
        lines.append("| Stage | Required Previous Stage | Required Checks |")
        lines.append("| --- | --- | --- |")
        for row in stage_gate_matrix:
            stage_name = row.get("stage", "unknown")
            previous_stage = row.get("required_previous_stage")
            previous_text = f"`{previous_stage}`" if previous_stage else "-"
            checks = row.get("required_checks", [])
            checks_text = ", ".join(f"`{item}`" for item in checks) if checks else "none configured"
            lines.append(f"| `{stage_name}` | {previous_text} | {checks_text} |")
        lines.append("")

    transition = report.get("transition", {})
    if transition:
        lines.append("## Transition Audit")
        lines.append(f"- Type: `{transition.get('type', 'unknown')}`")
        lines.append(f"- Outcome: `{transition.get('outcome', 'unknown')}`")
        lines.append(
            f"- Previous highest tag: `{transition.get('previous_highest_tag') or 'none'}` "
            f"(stage: `{transition.get('previous_highest_stage') or 'none'}`)"
        )
        lines.append(
            f"- Required previous stage: `{transition.get('required_previous_stage') or 'none'}`"
        )
        lines.append(
            f"- Required previous tag: `{transition.get('required_previous_tag') or 'none'}`"
        )
        if transition.get("same_stage_latest_tag"):
            lines.append(
                f"- Same-stage latest tag before current: "
                f"`{transition.get('same_stage_latest_tag')}`"
            )
        lines.append("")

    stage_history = report.get("stage_history", {})
    per_stage = stage_history.get("per_stage", {})
    if per_stage:
        lines.append("## Release Stage History")
        for stage_name in report.get("stage_order", []):
            tags = per_stage.get(stage_name, [])
            if tags:
                lines.append(f"- `{stage_name}`: {', '.join(f'`{tag}`' for tag in tags)}")
            else:
                lines.append(f"- `{stage_name}`: none")
        lines.append(
            f"- Latest known stage/tag: "
            f"`{stage_history.get('latest_stage') or 'none'}` / "
            f"`{stage_history.get('latest_tag') or 'none'}`"
        )
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
    parser = argparse.ArgumentParser(description="Validate release tag stage gating.")
    parser.add_argument("--repo-root", default=".")
    parser.add_argument("--tag", required=True)
    parser.add_argument("--stage-config-file", required=True)
    parser.add_argument("--mode", choices=("dry-run", "publish"), default="dry-run")
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--fail-on-violation", action="store_true")
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    out_json = Path(args.output_json)
    out_md = Path(args.output_md)

    policy = json.loads(Path(args.stage_config_file).read_text(encoding="utf-8"))
    stage_order, required_prev, required_checks, policy_violations = parse_stage_policy(policy)

    violations: list[str] = []
    warnings: list[str] = []
    violations.extend(policy_violations)

    try:
        version, stage, stage_number = parse_tag(args.tag)
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 2

    try:
        run_git(["fetch", "--quiet", "origin", "main", "--tags"], cwd=repo_root)
    except RuntimeError as exc:
        warnings.append(f"Failed to refresh origin refs/tags before validation: {exc}")

    try:
        tag_sha = run_git(["rev-parse", f"{args.tag}^{{commit}}"], cwd=repo_root)
    except RuntimeError as exc:
        violations.append(f"Unable to resolve tag `{args.tag}`: {exc}")
        tag_sha = ""

    if tag_sha:
        proc = subprocess.run(
            ["git", "merge-base", "--is-ancestor", tag_sha, "origin/main"],
            cwd=str(repo_root),
            text=True,
            capture_output=True,
            check=False,
        )
        if proc.returncode != 0:
            violations.append(
                f"Tag `{args.tag}` ({tag_sha}) is not reachable from `origin/main`; prerelease tags must originate from main."
            )

    tags_text = run_git(["tag", "--list", f"v{version}*"], cwd=repo_root)
    all_version_tags = [line.strip() for line in tags_text.splitlines() if line.strip()]
    parsed_entries = parse_stage_entries(all_version_tags)
    sibling_entries = [entry for entry in parsed_entries if entry["tag"] != args.tag]
    sibling_tags = [str(entry["tag"]) for entry in sibling_entries]

    sibling_stages = [str(entry["stage"]) for entry in sibling_entries]
    same_stage_entries = [entry for entry in sibling_entries if entry["stage"] == stage]
    same_stage_numbers = [
        int(entry["stage_number"]) for entry in same_stage_entries if entry["stage_number"] is not None
    ]
    same_stage_numbers.sort()
    same_stage_latest_tag = None
    same_stage_latest_number = None
    if same_stage_entries:
        same_stage_latest_entry = highest_stage_entry(same_stage_entries)
        if same_stage_latest_entry:
            same_stage_latest_tag = str(same_stage_latest_entry["tag"])
            if same_stage_latest_entry["stage_number"] is not None:
                same_stage_latest_number = int(same_stage_latest_entry["stage_number"])

    if stage_number is not None and same_stage_numbers and stage_number <= same_stage_numbers[-1]:
        violations.append(
            f"Stage `{stage}` tag number must increase monotonically for version `{version}`; "
            f"got `{args.tag}` but latest existing `{stage}` number is `{same_stage_numbers[-1]}`."
        )

    prerequisite_stage = required_prev.get(stage)
    prerequisite_tag = None
    if prerequisite_stage:
        prerequisite_entries = [entry for entry in sibling_entries if entry["stage"] == prerequisite_stage]
        prerequisite_entry = highest_stage_entry(prerequisite_entries)
        prerequisite_tag = str(prerequisite_entry["tag"]) if prerequisite_entry else None
        if not prerequisite_tag:
            violations.append(
                f"Stage `{stage}` requires at least one `{prerequisite_stage}` tag for version `{version}` before publishing `{args.tag}`."
            )

    highest_sibling_entry = highest_stage_entry(sibling_entries)
    highest_sibling_rank = 0
    highest_sibling_stage = None
    highest_sibling_tag = None
    if highest_sibling_entry:
        highest_sibling_rank = int(highest_sibling_entry["rank"])
        highest_sibling_stage = str(highest_sibling_entry["stage"])
        highest_sibling_tag = str(highest_sibling_entry["tag"])

    current_rank = STAGE_RANK.get(stage, 0)
    transition_type = "initial_stage"
    if highest_sibling_entry:
        if highest_sibling_rank > current_rank:
            transition_type = "demotion_blocked"
        elif highest_sibling_rank < current_rank:
            transition_type = "promotion"
        else:
            transition_type = "stage_iteration"

    if sibling_stages:
        if highest_sibling_rank > current_rank:
            violations.append(
                f"Higher stage tags already exist for `{version}`. Refusing stage regression to `{stage}`."
            )

    cargo_version = ""
    if tag_sha:
        try:
            cargo_toml = run_git(["show", f"{args.tag}:Cargo.toml"], cwd=repo_root)
            for line in cargo_toml.splitlines():
                line = line.strip()
                if line.startswith("version = "):
                    cargo_version = line.split('"', 2)[1]
                    break
        except RuntimeError as exc:
            violations.append(f"Failed to inspect Cargo.toml at `{args.tag}`: {exc}")

    if cargo_version and cargo_version != version:
        violations.append(
            f"Tag `{args.tag}` version `{version}` does not match Cargo.toml version `{cargo_version}` at the same ref."
        )

    transition_outcome = transition_type
    if violations:
        if transition_type == "promotion":
            transition_outcome = "promotion_blocked"
        elif transition_type == "stage_iteration":
            transition_outcome = "stage_iteration_blocked"
        elif transition_type == "initial_stage":
            transition_outcome = "initial_stage_blocked"

    stage_gate_matrix = [
        {
            "stage": stage_name,
            "required_previous_stage": required_prev.get(stage_name),
            "required_checks": required_checks.get(stage_name, []),
        }
        for stage_name in stage_order
    ]

    latest_entry = highest_stage_entry(parsed_entries)
    stage_history = {
        "version": version,
        "known_tags": [str(entry["tag"]) for entry in parsed_entries],
        "per_stage": {
            stage_name: [
                str(entry["tag"]) for entry in parsed_entries if entry["stage"] == stage_name
            ]
            for stage_name in stage_order
        },
        "timeline": parsed_entries,
        "latest_stage": str(latest_entry["stage"]) if latest_entry else None,
        "latest_tag": str(latest_entry["tag"]) if latest_entry else None,
    }

    ready_to_publish = args.mode == "publish" and not violations

    report = {
        "schema_version": "zeroclaw.prerelease-guard.v2",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "policy_schema_version": policy.get("schema_version"),
        "stage_order": stage_order,
        "tag": args.tag,
        "tag_sha": tag_sha or None,
        "version": version,
        "stage": stage,
        "stage_number": stage_number,
        "mode": args.mode,
        "ready_to_publish": ready_to_publish,
        "required_checks": required_checks.get(stage, []),
        "stage_gate_matrix": stage_gate_matrix,
        "sibling_tags": sibling_tags,
        "transition": {
            "type": transition_type,
            "outcome": transition_outcome,
            "required_previous_stage": prerequisite_stage,
            "required_previous_tag": prerequisite_tag,
            "previous_highest_stage": highest_sibling_stage,
            "previous_highest_tag": highest_sibling_tag,
            "same_stage_latest_tag": same_stage_latest_tag,
            "same_stage_latest_number": same_stage_latest_number,
        },
        "stage_history": stage_history,
        "warnings": warnings,
        "violations": violations,
    }

    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    out_md.write_text(build_markdown(report), encoding="utf-8")

    if args.fail_on_violation and violations:
        print("prerelease guard violations found:", file=sys.stderr)
        for item in violations:
            print(f"- {item}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
