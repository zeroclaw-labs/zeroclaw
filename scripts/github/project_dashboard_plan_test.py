#!/usr/bin/env python3
"""Tests for the report-only Project dashboard planner."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

import project_dashboard_plan as planner


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


if __name__ == "__main__":
    unittest.main()
