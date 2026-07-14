#!/usr/bin/env python3
"""Tests for the report-only Project dashboard planner."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

import project_dashboard_plan as planner
import project_initiative_validate as initiative_validator


def issue(state: str = "open", labels: list[str] | None = None, **extra: object) -> dict[str, object]:
    payload: dict[str, object] = {
        "number": 6808,
        "title": "Governance rollout",
        "state": state,
        "labels": [{"name": label} for label in labels or []],
    }
    payload.update(extra)
    return payload


class ProjectDashboardPlanTest(unittest.TestCase):
    def setUp(self) -> None:
        self.contract = planner.load_contract()

    def classify(self, payload: dict[str, object]) -> planner.Plan:
        return planner.classify_issue(payload, self.contract)

    def test_contract_rules_emit_only_fnd_project_status_values(self) -> None:
        statuses = planner.project_status_values()
        self.assertEqual(statuses, {"Idea", "Backlog", "Defined", "In Progress", "In Review", "Done", "Won't Do"})
        for rule in self.contract["dry_run_issue_status_rules"]:
            self.assertIn(rule["status"], statuses)

    def test_load_issue_from_event_accepts_event_and_issue_json(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            event_path = Path(tmpdir) / "event.json"
            issue_path = Path(tmpdir) / "issue.json"
            event_path.write_text(json.dumps({"issue": issue(labels=["status:accepted"])}))
            issue_path.write_text(json.dumps(issue(labels=["status:accepted"])))

            self.assertEqual(planner.load_issue_from_event(event_path)["number"], 6808)
            self.assertEqual(planner.load_issue_from_event(issue_path)["number"], 6808)

    def test_label_names_accepts_string_and_object_labels(self) -> None:
        labels = planner.label_names({"labels": ["status:accepted", {"name": "type:rfc"}]})
        self.assertEqual(labels, {"status:accepted", "type:rfc"})

    def test_not_planned_closed_issue_maps_to_wont_do_before_open_conflicts(self) -> None:
        plan = self.classify(
            issue(
                state="closed",
                state_reason="not_planned",
                labels=["status:blocked", "status:in-progress"],
            )
        )
        self.assertEqual(plan.status, "Won't Do")
        self.assertEqual(plan.warnings, [])

    def test_closed_issue_without_terminal_signal_maps_to_done(self) -> None:
        plan = self.classify(issue(state="closed"))
        self.assertEqual(plan.status, "Done")

    def test_in_progress_label_maps_to_in_review(self) -> None:
        plan = self.classify(issue(labels=["status:in-progress"]))
        self.assertEqual(plan.status, "In Review")

    def test_blocked_no_stale_issue_is_a_conflict(self) -> None:
        plan = self.classify(issue(labels=["status:blocked", "status:no-stale"]))
        self.assertEqual(plan.status, "Backlog")
        self.assertEqual(plan.confidence, "low")
        self.assertEqual(len(plan.warnings), 1)

    def test_open_conflict_uses_backlog_with_low_confidence(self) -> None:
        plan = self.classify(issue(labels=["status:blocked", "status:in-progress"]))
        self.assertEqual(plan.status, "Backlog")
        self.assertEqual(plan.confidence, "low")
        self.assertEqual(len(plan.warnings), 1)

    def test_no_stale_with_in_progress_is_a_conflict(self) -> None:
        plan = self.classify(issue(labels=["status:no-stale", "status:in-progress"]))
        self.assertEqual(plan.status, "Backlog")
        self.assertEqual(plan.confidence, "low")
        self.assertEqual(len(plan.warnings), 1)

    def test_blocked_pickup_labels_are_a_conflict(self) -> None:
        plan = self.classify(issue(labels=["status:blocked", "help wanted"]))
        self.assertEqual(plan.status, "Backlog")
        self.assertEqual(plan.confidence, "low")
        self.assertEqual(len(plan.warnings), 1)

    def test_contract_rejects_unknown_status(self) -> None:
        bad_contract = json.loads(json.dumps(self.contract))
        bad_contract["dry_run_issue_status_rules"][0]["status"] = "Almost Done"
        with self.assertRaisesRegex(ValueError, "unknown Project Status"):
            planner.validate_contract(bad_contract)

    def test_contract_requires_open_and_closed_catchalls(self) -> None:
        bad_contract = json.loads(json.dumps(self.contract))
        bad_contract["dry_run_issue_status_rules"] = [
            rule for rule in bad_contract["dry_run_issue_status_rules"] if rule["name"] != "open_default_intake"
        ]
        with self.assertRaisesRegex(ValueError, "open catchall"):
            planner.validate_contract(bad_contract)


def initiative_snapshot() -> dict[str, object]:
    initiative = "Runtime Safety (#101)"
    return {
        "tracker": {
            "number": 101,
            "state": "open",
            "listed_item_numbers": [102, 103],
        },
        "initiative": initiative,
        "retired_milestone": "Runtime Safety",
        "items": [
            {
                "number": 101,
                "type": "Issue",
                "state": "open",
                "initiative": initiative,
                "milestone": None,
            },
            {
                "number": 102,
                "type": "Issue",
                "state": "open",
                "initiative": initiative,
                "milestone": None,
                "parent_issue_number": 101,
            },
            {
                "number": 103,
                "type": "PullRequest",
                "state": "open",
                "initiative": initiative,
                "milestone": None,
                "linked_issue_numbers": [101],
            },
            {
                "number": 104,
                "type": "PullRequest",
                "state": "closed",
                "initiative": initiative,
                "milestone": "v0.9.0",
                "related_item_numbers": [102],
                "release_evidence": "Release tracker explicitly schedules this pull request.",
            },
        ],
    }


class ProjectInitiativeValidateTest(unittest.TestCase):
    def codes(self, snapshot: dict[str, object]) -> list[str]:
        return [
            finding.code
            for finding in initiative_validator.validate_initiative_snapshot(snapshot)
        ]

    def test_valid_initiative_snapshot_has_no_findings(self) -> None:
        self.assertEqual(self.codes(initiative_snapshot()), [])

    def test_missing_initiative_variants_are_detected(self) -> None:
        snapshot = initiative_snapshot()
        for item in snapshot["items"]:
            if item["number"] in {101, 102, 103}:
                item["initiative"] = None
        self.assertEqual(
            self.codes(snapshot),
            [
                "tracker-missing-initiative",
                "sub-issue-missing-initiative",
                "direct-pr-missing-initiative",
            ],
        )

    def test_unsupported_initiative_membership_is_detected(self) -> None:
        snapshot = initiative_snapshot()
        snapshot["items"].append(
            {
                "number": 110,
                "type": "Issue",
                "state": "open",
                "initiative": snapshot["initiative"],
                "milestone": None,
            }
        )
        self.assertEqual(self.codes(snapshot), ["unsupported-initiative-membership"])

    def test_retired_capability_milestone_is_detected(self) -> None:
        snapshot = initiative_snapshot()
        snapshot["items"][1]["milestone"] = snapshot["retired_milestone"]
        self.assertEqual(
            self.codes(snapshot),
            [
                "retired-milestone-still-assigned",
                "initiative-with-capability-milestone",
            ],
        )

    def test_numbered_release_requires_evidence(self) -> None:
        snapshot = initiative_snapshot()
        snapshot["items"][3]["release_evidence"] = None
        self.assertEqual(self.codes(snapshot), ["release-milestone-without-evidence"])

    def test_closed_tracker_with_open_sub_issue_is_detected(self) -> None:
        snapshot = initiative_snapshot()
        snapshot["tracker"]["state"] = "closed"
        self.assertEqual(self.codes(snapshot), ["closed-tracker-with-open-sub-issue"])

    def test_missing_retired_milestone_name_does_not_flag_unscheduled_items(self) -> None:
        snapshot = initiative_snapshot()
        snapshot["retired_milestone"] = None
        self.assertEqual(self.codes(snapshot), [])

    def test_missing_and_duplicate_tracker_items_are_detected(self) -> None:
        snapshot = initiative_snapshot()
        snapshot["tracker"]["listed_item_numbers"].append(111)
        snapshot["items"].append(dict(snapshot["items"][2]))
        self.assertEqual(
            self.codes(snapshot),
            ["duplicate-project-item", "tracker-item-missing-from-project"],
        )


if __name__ == "__main__":
    unittest.main()
