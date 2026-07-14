#!/usr/bin/env python3
"""Report-only validation for Project Initiative, hierarchy, and milestone state.

The input is an on-demand snapshot of canonical GitHub state. Its top-level
keys are ``tracker`` (``number``, ``state``, ``listed_item_numbers``),
``initiative``, optional ``retired_milestone``, and ``items``. Each item records
``number``, ``type``, ``state``, Initiative and milestone values, optional parent
and relationship number lists, and optional ``release_evidence``. Relationship
lists use ``related_item_numbers``, ``linked_issue_numbers``, and
``closing_issue_numbers``.

This command does not call GitHub, write Project fields, edit issues, or change
milestones.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any


NUMBERED_RELEASE = re.compile(r"^v\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$")


@dataclass(frozen=True)
class Finding:
    code: str
    number: int | None
    message: str


def integer(value: Any, field_name: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError(f"{field_name} must be an integer")
    return value


def integer_list(value: Any, field_name: str) -> list[int]:
    if value is None:
        return []
    if not isinstance(value, list):
        raise ValueError(f"{field_name} must be a list")
    return [integer(item, field_name) for item in value]


def text(value: Any, field_name: str, *, allow_empty: bool = False) -> str:
    if not isinstance(value, str) or (not allow_empty and not value):
        raise ValueError(f"{field_name} must be a string")
    return value


def optional_text(value: Any, field_name: str) -> str | None:
    if value is None or value == "":
        return None
    return text(value, field_name)


def item_number(item: dict[str, Any]) -> int:
    return integer(item.get("number"), "items[].number")


def item_relations(item: dict[str, Any]) -> set[int]:
    relations: set[int] = set()
    for field_name in (
        "related_item_numbers",
        "linked_issue_numbers",
        "closing_issue_numbers",
    ):
        relations.update(integer_list(item.get(field_name), f"items[].{field_name}"))
    return relations


def is_numbered_release(milestone: str | None) -> bool:
    return bool(milestone and NUMBERED_RELEASE.fullmatch(milestone))


def load_snapshot(path: Path) -> dict[str, Any]:
    snapshot = json.loads(path.read_text())
    if not isinstance(snapshot, dict):
        raise ValueError("snapshot must be a JSON object")
    validate_snapshot_shape(snapshot)
    return snapshot


def validate_snapshot_shape(snapshot: dict[str, Any]) -> None:
    tracker = snapshot.get("tracker")
    if not isinstance(tracker, dict):
        raise ValueError("tracker must be an object")
    integer(tracker.get("number"), "tracker.number")
    text(tracker.get("state"), "tracker.state")
    integer_list(tracker.get("listed_item_numbers"), "tracker.listed_item_numbers")
    text(snapshot.get("initiative"), "initiative")
    optional_text(snapshot.get("retired_milestone"), "retired_milestone")

    items = snapshot.get("items")
    if not isinstance(items, list):
        raise ValueError("items must be a list")
    for item in items:
        if not isinstance(item, dict):
            raise ValueError("items entries must be objects")
        item_number(item)
        item_type = text(item.get("type"), "items[].type")
        if item_type not in {"Issue", "PullRequest"}:
            raise ValueError(f"items[].type has unsupported value {item_type!r}")
        text(item.get("state"), "items[].state")
        optional_text(item.get("initiative"), "items[].initiative")
        optional_text(item.get("milestone"), "items[].milestone")
        optional_text(item.get("release_evidence"), "items[].release_evidence")
        if item.get("parent_issue_number") is not None:
            integer(item.get("parent_issue_number"), "items[].parent_issue_number")
        item_relations(item)


def supported_numbers(snapshot: dict[str, Any]) -> set[int]:
    tracker = snapshot["tracker"]
    tracker_number = tracker["number"]
    supported = {tracker_number}
    supported.update(tracker.get("listed_item_numbers", []))

    items = snapshot["items"]
    supported.update(
        item_number(item)
        for item in items
        if item.get("parent_issue_number") == tracker_number
    )

    changed = True
    while changed:
        changed = False
        for item in items:
            number = item_number(item)
            if number in supported or not item_relations(item) & supported:
                continue
            supported.add(number)
            changed = True
    return supported


def validate_initiative_snapshot(snapshot: dict[str, Any]) -> list[Finding]:
    validate_snapshot_shape(snapshot)
    tracker = snapshot["tracker"]
    tracker_number = tracker["number"]
    expected_initiative = snapshot["initiative"]
    retired_milestone = snapshot.get("retired_milestone")
    items = snapshot["items"]
    findings: list[Finding] = []

    by_number: dict[int, list[dict[str, Any]]] = {}
    for item in items:
        by_number.setdefault(item_number(item), []).append(item)

    for number, duplicates in sorted(by_number.items()):
        if len(duplicates) > 1:
            findings.append(
                Finding(
                    "duplicate-project-item",
                    number,
                    f"#{number} appears {len(duplicates)} times in the Project snapshot.",
                )
            )

    for number in sorted(set(tracker.get("listed_item_numbers", [])) - set(by_number)):
        findings.append(
            Finding(
                "tracker-item-missing-from-project",
                number,
                f"Tracker #{tracker_number} lists #{number}, but it is absent "
                "from the Project snapshot.",
            )
        )

    supported = supported_numbers(snapshot)
    for item in items:
        number = item_number(item)
        item_type = item["type"]
        initiative = item.get("initiative") or None
        milestone = item.get("milestone") or None
        parent = item.get("parent_issue_number")
        direct_tracker_link = tracker_number in item_relations(item)

        if number in supported and initiative != expected_initiative:
            if number == tracker_number:
                code = "tracker-missing-initiative"
                reason = "initiative tracker"
            elif item_type == "Issue" and parent == tracker_number:
                code = "sub-issue-missing-initiative"
                reason = f"sub-issue of #{tracker_number}"
            elif item_type == "PullRequest" and direct_tracker_link:
                code = "direct-pr-missing-initiative"
                reason = f"pull request directly linked to #{tracker_number}"
            else:
                code = "supported-item-missing-initiative"
                reason = f"tracker-supported {item_type.lower()}"
            findings.append(
                Finding(
                    code,
                    number,
                    f"#{number} is a {reason} but does not have Initiative "
                    f"{expected_initiative!r}.",
                )
            )

        if initiative == expected_initiative and number not in supported:
            findings.append(
                Finding(
                    "unsupported-initiative-membership",
                    number,
                    f"#{number} has Initiative {expected_initiative!r} without "
                    f"a supported relationship to #{tracker_number}.",
                )
            )

        if (
            initiative == expected_initiative
            and retired_milestone
            and milestone == retired_milestone
        ):
            findings.append(
                Finding(
                    "retired-milestone-still-assigned",
                    number,
                    f"#{number} still uses retired milestone {retired_milestone!r}.",
                )
            )

        if initiative == expected_initiative and milestone and not is_numbered_release(milestone):
            findings.append(
                Finding(
                    "initiative-with-capability-milestone",
                    number,
                    f"#{number} combines an Initiative with non-release milestone {milestone!r}.",
                )
            )

        if (
            initiative == expected_initiative
            and is_numbered_release(milestone)
            and not item.get("release_evidence")
        ):
            findings.append(
                Finding(
                    "release-milestone-without-evidence",
                    number,
                    f"#{number} is assigned to {milestone!r} without recorded release evidence.",
                )
            )

    if str(tracker["state"]).lower() == "closed":
        for item in items:
            if (
                item.get("type") == "Issue"
                and item.get("parent_issue_number") == tracker_number
                and str(item.get("state", "")).lower() == "open"
            ):
                number = item_number(item)
                findings.append(
                    Finding(
                        "closed-tracker-with-open-sub-issue",
                        number,
                        f"Tracker #{tracker_number} is closed while sub-issue "
                        f"#{number} remains open.",
                    )
                )

    return findings


def as_json(snapshot: dict[str, Any], findings: list[Finding]) -> str:
    payload = {
        "report_only": True,
        "tracker": snapshot["tracker"]["number"],
        "initiative": snapshot["initiative"],
        "item_count": len(snapshot["items"]),
        "finding_count": len(findings),
        "findings": [
            {"code": finding.code, "number": finding.number, "message": finding.message}
            for finding in findings
        ],
    }
    return json.dumps(payload, indent=2, sort_keys=True) + "\n"


def as_markdown(snapshot: dict[str, Any], findings: list[Finding]) -> str:
    lines = [
        "## Project Initiative Validation",
        "",
        f"- Tracker: #{snapshot['tracker']['number']}",
        f"- Initiative: `{snapshot['initiative']}`",
        f"- Project items inspected: {len(snapshot['items'])}",
        f"- Findings: {len(findings)}",
        "- Action: report-only; no Project fields, issues, pull requests, or "
        "milestones were changed.",
    ]
    if findings:
        lines.extend(["", "### Findings", ""])
        lines.extend(
            f"- `{finding.code}`: {finding.message}" for finding in findings
        )
    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--snapshot", required=True, type=Path)
    parser.add_argument("--format", choices=("json", "markdown"), default="json")
    parser.add_argument("--output", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        snapshot = load_snapshot(args.snapshot)
        findings = validate_initiative_snapshot(snapshot)
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"Failed to validate Project Initiative snapshot: {exc}", file=sys.stderr)
        return 2

    output = (
        as_json(snapshot, findings)
        if args.format == "json"
        else as_markdown(snapshot, findings)
    )
    if args.output:
        args.output.write_text(output)
        print("Wrote Project Initiative validation report.")
    else:
        print(output, end="")
    return 1 if findings else 0


if __name__ == "__main__":
    raise SystemExit(main())
