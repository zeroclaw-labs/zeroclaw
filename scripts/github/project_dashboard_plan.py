#!/usr/bin/env python3
"""Dry-run Project dashboard planner for ZeroClaw issues.

This script intentionally plans only issue-board Status values. It does not write to
GitHub Projects, edit issues, or mutate labels. Live ProjectV2 writes require a
separate credentialed workflow once maintainers approve the exact field mapping.
"""

from __future__ import annotations

import argparse
import json
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


DEFAULT_CONTRACT_PATH = (
    Path(__file__).resolve().parents[2]
    / "docs/book/src/maintainers/project-board-contract.json"
)
DEFAULT_STATUS_SOURCE_PATH = (
    Path(__file__).resolve().parents[2]
    / "docs/book/src/foundations/fnd-003-governance.md"
)


@dataclass(frozen=True)
class Plan:
    status: str
    reason: str
    confidence: str
    warnings: list[str] = field(default_factory=list)


def label_names(issue: dict[str, Any]) -> set[str]:
    labels = issue.get("labels", [])
    names: set[str] = set()
    for label in labels:
        if isinstance(label, str):
            names.add(label)
        elif isinstance(label, dict) and isinstance(label.get("name"), str):
            names.add(label["name"])
    return names


def issue_number(issue: dict[str, Any]) -> str:
    number = issue.get("number")
    return str(number) if number is not None else "unknown"


def issue_title(issue: dict[str, Any]) -> str:
    title = issue.get("title")
    return title if isinstance(title, str) else ""


def issue_state(issue: dict[str, Any]) -> str:
    state = str(issue.get("state", "")).lower()
    return "closed" if state == "closed" else "open"


def issue_state_reason(issue: dict[str, Any]) -> str:
    reason = issue.get("state_reason")
    return reason if isinstance(reason, str) else ""


def string_list(value: Any) -> list[str]:
    if not isinstance(value, list):
        return []
    return [item for item in value or [] if isinstance(item, str)]


def project_status_values(path: Path = DEFAULT_STATUS_SOURCE_PATH) -> set[str]:
    for line in path.read_text().splitlines():
        if not line.startswith("| **Status** |"):
            continue
        columns = [column.strip() for column in line.strip().strip("|").split("|")]
        if len(columns) < 3:
            break
        statuses = set()
        for value in columns[2].split("·"):
            parts = value.strip().split(maxsplit=1)
            if len(parts) == 2:
                statuses.add(parts[1])
        if statuses:
            return statuses
    raise ValueError(f"could not find Project Status values in {path}")


def labels_match(rule: dict[str, Any], labels: set[str]) -> bool:
    labels_all = set(string_list(rule.get("labels_all")))
    if labels_all and not labels_all <= labels:
        return False
    labels_any = set(string_list(rule.get("labels_any")))
    if labels_any and not labels_any & labels:
        return False
    return True


def detect_conflicts(contract: dict[str, Any], labels: set[str]) -> list[str]:
    conflicts: list[str] = []
    for rule in contract.get("dry_run_issue_conflicts", []):
        if not isinstance(rule, dict) or not labels_match(rule, labels):
            continue
        message = rule.get("message")
        if isinstance(message, str):
            conflicts.append(message)
    return conflicts


def label_warnings(contract: dict[str, Any], labels: set[str]) -> list[str]:
    warnings: list[str] = []
    for rule in contract.get("dry_run_issue_warning_rules", []):
        if not isinstance(rule, dict) or not labels_match(rule, labels):
            continue
        message = rule.get("message")
        if isinstance(message, str):
            warnings.append(message)
    return warnings


def rule_matches(
    rule: dict[str, Any],
    issue: dict[str, Any],
    labels: set[str],
    conflicts: list[str],
) -> bool:
    if rule.get("state") != issue_state(issue):
        return False
    state_reason = rule.get("state_reason")
    if isinstance(state_reason, str) and state_reason != issue_state_reason(issue):
        return False
    if bool(rule.get("has_conflicts")) and not conflicts:
        return False
    return labels_match(rule, labels)


def rule_plan(rule: dict[str, Any], warnings: list[str], conflicts: list[str]) -> Plan:
    status = rule["status"]
    reason = rule["reason"]
    confidence = rule["confidence"]
    plan_warnings: list[str] = []
    if rule.get("use_conflict_warnings"):
        plan_warnings.extend(conflicts)
    if rule.get("use_label_warnings"):
        plan_warnings.extend(warnings)
    return Plan(status=status, reason=reason, confidence=confidence, warnings=plan_warnings)


def validate_match_rule(rule: dict[str, Any], family: str) -> None:
    if not isinstance(rule, dict):
        raise ValueError(f"{family} entries must be objects")
    for field_name in ("labels_all", "labels_any"):
        if field_name in rule and not string_list(rule.get(field_name)):
            raise ValueError(f"{family} rule has invalid {field_name!r}")
    message = rule.get("message")
    if family != "status" and not isinstance(message, str):
        raise ValueError(f"{family} rule is missing string field 'message'")


def validate_contract(
    contract: dict[str, Any],
    status_source: Path = DEFAULT_STATUS_SOURCE_PATH,
) -> None:
    statuses = project_status_values(status_source)

    rules = contract.get("dry_run_issue_status_rules")
    if not isinstance(rules, list) or not rules:
        raise ValueError("dry_run_issue_status_rules must list at least one rule")

    for family in ("dry_run_issue_conflicts", "dry_run_issue_warning_rules"):
        family_rules = contract.get(family, [])
        if not isinstance(family_rules, list):
            raise ValueError(f"{family} must be a list")
        for rule in family_rules:
            validate_match_rule(rule, family)

    seen_open_default = False
    seen_closed_default = False
    for rule in rules:
        validate_match_rule(rule, "status")
        name = rule.get("name", "<unnamed>")
        if rule.get("state") not in {"open", "closed"}:
            raise ValueError(f"rule {name!r} has invalid state {rule.get('state')!r}")
        status = rule.get("status")
        if status not in statuses:
            raise ValueError(f"rule {name!r} uses unknown Project Status {status!r}")
        for field_name in ("reason", "confidence"):
            if not isinstance(rule.get(field_name), str):
                raise ValueError(f"rule {name!r} is missing string field {field_name!r}")
        if rule.get("confidence") not in {"low", "medium", "high"}:
            raise ValueError(f"rule {name!r} has invalid confidence {rule.get('confidence')!r}")
        for flag_name in ("has_conflicts", "use_conflict_warnings", "use_label_warnings"):
            if flag_name in rule and not isinstance(rule.get(flag_name), bool):
                raise ValueError(f"rule {name!r} has non-boolean {flag_name!r}")
        if rule.get("state") == "open" and not any(
            key in rule for key in ("state_reason", "labels_all", "labels_any", "has_conflicts")
        ):
            seen_open_default = True
        if rule.get("state") == "closed" and not any(
            key in rule for key in ("state_reason", "labels_all", "labels_any", "has_conflicts")
        ):
            seen_closed_default = True
    if not seen_open_default:
        raise ValueError("dry_run_issue_status_rules is missing an open catchall rule")
    if not seen_closed_default:
        raise ValueError("dry_run_issue_status_rules is missing a closed catchall rule")


def load_contract(path: Path = DEFAULT_CONTRACT_PATH) -> dict[str, Any]:
    contract = json.loads(path.read_text())
    if not isinstance(contract, dict):
        raise ValueError(f"{path} does not contain a JSON object")
    validate_contract(contract)
    return contract


def classify_issue(issue: dict[str, Any], contract: dict[str, Any]) -> Plan:
    labels = label_names(issue)
    conflicts = detect_conflicts(contract, labels) if issue_state(issue) == "open" else []
    warnings = label_warnings(contract, labels)
    for rule in contract["dry_run_issue_status_rules"]:
        if rule_matches(rule, issue, labels, conflicts):
            return rule_plan(rule, warnings, conflicts)
    raise ValueError(f"no dashboard planner rule matched issue #{issue_number(issue)}")


def load_issue_from_event(path: Path) -> dict[str, Any]:
    data = json.loads(path.read_text())
    if isinstance(data, dict) and isinstance(data.get("issue"), dict):
        return data["issue"]
    if isinstance(data, dict) and "number" in data and "labels" in data:
        return data
    raise ValueError(f"{path} does not look like a GitHub issue event or issue JSON")


def as_json(issue: dict[str, Any], plan: Plan) -> str:
    payload = {
        "issue": {
            "number": issue.get("number"),
            "title": issue_title(issue),
            "state": issue.get("state"),
            "labels": sorted(label_names(issue)),
        },
        "plan": {
            "status": plan.status,
            "reason": plan.reason,
            "confidence": plan.confidence,
            "warnings": plan.warnings,
        },
    }
    return json.dumps(payload, indent=2, sort_keys=True)


def as_markdown(issue: dict[str, Any], plan: Plan) -> str:
    labels = ", ".join(f"`{label}`" for label in sorted(label_names(issue))) or "_none_"
    lines = [
        "## Project Dashboard Plan",
        "",
        f"- Issue: #{issue_number(issue)} {issue_title(issue)}",
        f"- Proposed Status: `{plan.status}`",
        f"- Confidence: `{plan.confidence}`",
        f"- Reason: {plan.reason}",
        f"- Labels: {labels}",
        "- Action: report-only; no Project fields, labels, or issues were changed.",
    ]
    if plan.warnings:
        lines.append("")
        lines.append("### Warnings")
        lines.extend(f"- {warning}" for warning in plan.warnings)
    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event-path", type=Path, help="Path to GitHub event JSON.")
    parser.add_argument("--issue-json", type=Path, help="Path to a GitHub issue JSON object.")
    parser.add_argument(
        "--contract",
        type=Path,
        default=DEFAULT_CONTRACT_PATH,
        help="Path to the project board planning contract JSON.",
    )
    parser.add_argument(
        "--format",
        choices=("json", "markdown"),
        default="json",
        help="Output format.",
    )
    parser.add_argument("--output", type=Path, help="Optional path to write output.")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if bool(args.event_path) == bool(args.issue_json):
        print("Pass exactly one of --event-path or --issue-json.", file=sys.stderr)
        return 2

    source = args.event_path or args.issue_json
    try:
        issue = load_issue_from_event(source)
        contract = load_contract(args.contract)
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"Failed to load planner input: {exc}", file=sys.stderr)
        return 1

    plan = classify_issue(issue, contract)
    output = as_json(issue, plan) if args.format == "json" else as_markdown(issue, plan)
    if args.output:
        args.output.write_text(output)
        print("Wrote Project Dashboard Plan.")
    else:
        print(output, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
