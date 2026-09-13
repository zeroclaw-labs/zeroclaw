#!/usr/bin/env python3
"""Focused tests for the report-only pull-request review queues."""

from __future__ import annotations

from datetime import datetime, timezone
import json
import subprocess
import unittest
from unittest.mock import patch

try:
    from scripts.github import pr_review_queue as queue
except ModuleNotFoundError:
    import pr_review_queue as queue


NOW = datetime(2026, 8, 29, tzinfo=timezone.utc)
CORE = {"core-one", "core-two"}


def pr(number: int = 1, **extra: object) -> dict[str, object]:
    value: dict[str, object] = {
        "number": number,
        "title": f"Change {number}",
        "author": {"login": "author"},
        "labels": [],
        "url": f"https://github.com/zeroclaw-labs/zeroclaw/pull/{number}",
        "headRefOid": "head",
    }
    value.update(extra)
    return value


def review(reviewer: str, state: str = "APPROVED", commit: str | None = "head", review_id: int = 1) -> dict[str, object]:
    return {
        "id": review_id,
        "user": {"login": reviewer},
        "state": state,
        "commit_id": commit,
        "submitted_at": f"2026-08-{20 + review_id:02d}T00:00:00Z",
    }


def event(kind: str, when: str, actor: str = "bot", label: str | None = None) -> dict[str, object]:
    value: dict[str, object] = {"event": kind, "created_at": when, "actor": {"login": actor}}
    if label:
        value["label"] = {"name": label}
    return value


class ReviewQueueTest(unittest.TestCase):
    def test_searches_are_lane_specific(self) -> None:
        near_ready = queue.search_query("near-ready")
        self.assertIn("status:success", near_ready)
        self.assertIn('label:"needs-maintainer-review"', near_ready)
        self.assertIn('-label:"needs-author-action"', near_ready)
        self.assertIn('label:"needs-maintainer-review"', queue.search_query("maintainer"))
        self.assertIn("author:maintainer-one", queue.search_query("mine", "maintainer-one"))
        self.assertIn("review:approved", queue.search_query("second-core"))
        self.assertIn('label:"needs-author-action"', queue.search_query("author-action"))
        self.assertIn("label:stacked", queue.search_query("stacked"))

    def test_discovery_uses_search_and_minimal_fields(self) -> None:
        calls: list[tuple[str, ...]] = []

        def fake(*args: str) -> object:
            calls.append(args)
            return [pr()]

        self.assertEqual(queue.discover("maintainer", None, fake)[0]["number"], 1)
        self.assertEqual(calls[0][0:2], ("pr", "list"))
        self.assertIn("--search", calls[0])

    def test_discovery_rejects_missing_fields(self) -> None:
        with self.assertRaises(queue.GitHubReadError):
            queue.discover("maintainer", None, lambda *args: [{"number": 1}])

    def test_latest_review_state_is_case_insensitive(self) -> None:
        reviews = [review("Core-One", review_id=1), review("core-one", state="CHANGES_REQUESTED", review_id=2)]
        self.assertEqual(queue.latest_review_by_author(reviews)["core-one"]["state"], "CHANGES_REQUESTED")

    def test_comment_review_does_not_discard_prior_approval(self) -> None:
        reviews = [review("core-one", review_id=1), review("core-one", state="COMMENTED", review_id=2)]
        row = queue.second_core_row(pr(), reviews, CORE)
        self.assertEqual(row["status"], "candidate")
        self.assertIn("@core-one", row["detail"])

    def test_second_core_requires_one_current_head_core_approval(self) -> None:
        payload = pr()
        row = queue.second_core_row(payload, [review("core-one")], CORE)
        self.assertEqual(row["status"], "candidate")
        self.assertIn("@core-one", row["detail"])
        self.assertIsNone(queue.second_core_row(payload, [], CORE))
        self.assertIsNone(queue.second_core_row(payload, [review("core-one"), review("core-two", review_id=2)], CORE))

    def test_second_core_ignores_old_or_non_core_approvals(self) -> None:
        self.assertIsNone(queue.second_core_row(pr(), [review("core-one", commit="old")], CORE))
        self.assertIsNone(queue.second_core_row(pr(), [review("contributor")], CORE))

    def test_second_core_reports_missing_sha_as_unknown(self) -> None:
        self.assertEqual(queue.second_core_row(pr(headRefOid=None), [review("core-one")], CORE)["status"], "unknown")
        self.assertEqual(queue.second_core_row(pr(), [review("core-one", commit=None)], CORE)["status"], "unknown")

    def test_author_action_reports_old_unanswered_request(self) -> None:
        timeline = [event("labeled", "2026-08-20T00:00:00Z", label="needs-author-action")]
        row = queue.author_action_row(pr(), timeline, NOW, 7)
        self.assertEqual(row["status"], "candidate")
        self.assertEqual(row["wait_days"], 9.0)

    def test_author_action_omits_young_request(self) -> None:
        timeline = [event("labeled", "2026-08-28T00:00:00Z", label="needs-author-action")]
        self.assertIsNone(queue.author_action_row(pr(), timeline, NOW, 7))

    def test_author_action_is_unknown_after_author_activity(self) -> None:
        timeline = [
            event("labeled", "2026-08-20T00:00:00Z", label="needs-author-action"),
            event("commented", "2026-08-21T00:00:00Z", actor="AUTHOR"),
        ]
        row = queue.author_action_row(pr(), timeline, NOW, 7)
        self.assertEqual(row["status"], "unknown")
        self.assertIn("uncertain", row["detail"])

    def test_author_action_is_unknown_after_commit_activity(self) -> None:
        timeline = [
            event("labeled", "2026-08-20T00:00:00Z", label="needs-author-action"),
            {
                "event": "committed",
                "sha": "abc123",
                "created_at": "2026-08-21T00:00:00Z",
                "author": {"name": "Patch Author", "email": "author@example.invalid", "date": "2026-08-10T00:00:00Z"},
            },
        ]
        row = queue.author_action_row(pr(), timeline, NOW, 7)
        self.assertEqual(row["status"], "unknown")
        self.assertIn("commit activity", row["detail"])

    def test_author_action_ignores_commit_before_current_label_interval(self) -> None:
        timeline = [
            {"event": "committed", "sha": "abc123", "author": {"name": "Patch Author"}},
            event("labeled", "2026-08-20T00:00:00Z", label="needs-author-action"),
        ]
        row = queue.author_action_row(pr(), timeline, NOW, 7)
        self.assertEqual(row["status"], "candidate")
        self.assertEqual(row["wait_days"], 9.0)

    def test_author_action_uses_current_label_interval(self) -> None:
        timeline = [
            event("labeled", "2026-08-10T00:00:00Z", label="needs-author-action"),
            event("unlabeled", "2026-08-12T00:00:00Z", label="needs-author-action"),
            event("labeled", "2026-08-25T00:00:00Z", label="needs-author-action"),
        ]
        self.assertIsNone(queue.author_action_row(pr(), timeline, NOW, 7))

    def test_missing_label_event_is_unknown(self) -> None:
        self.assertEqual(queue.author_action_row(pr(), [], NOW, 7)["status"], "unknown")

    def test_light_lane_does_not_fetch_details(self) -> None:
        rows = queue.detail_rows("maintainer", [pr()], 7, NOW, lambda *args: self.fail("unexpected read"), CORE)
        self.assertEqual(rows[0]["detail"], "search match")

    def test_near_ready_lane_does_not_fetch_details(self) -> None:
        rows = queue.detail_rows("near-ready", [pr()], 7, NOW, lambda *args: self.fail("unexpected read"), CORE)
        self.assertEqual(rows[0]["detail"], "search match")

    def test_detail_reads_slurp_paginated_responses(self) -> None:
        calls: list[tuple[str, ...]] = []

        def fake(*args: str) -> object:
            calls.append(args)
            return [[]]

        queue.fetch_reviews(pr(), fake)
        queue.fetch_timeline(pr(), fake)
        self.assertTrue(all("--paginate" in call and "--slurp" in call for call in calls))

    def test_published_core_roster_is_parseable(self) -> None:
        self.assertGreaterEqual(len(queue.load_core_roster()), 2)

    def test_core_roster_ignores_other_markdown_tables(self) -> None:
        from pathlib import Path
        from tempfile import TemporaryDirectory

        with TemporaryDirectory() as directory:
            path = Path(directory) / "communication.md"
            path.write_text(
                "| Handle | Role |\n|---|---|\n| [@core-one](https://example.invalid/core) | Core Team |\n"
                "| [@not-core](https://example.invalid/other) | Community |\n"
            )
            self.assertEqual(queue.load_core_roster(path), {"core-one"})

    def test_missing_core_roster_degrades_second_core_to_unknown(self) -> None:
        with patch.object(queue, "load_core_roster", side_effect=queue.GitHubReadError("format changed")):
            with patch.object(queue, "discover", return_value=[pr()]):
                rows = queue.collect("second-core", None, 7, lambda *args: self.fail("unexpected detail read"), NOW)
        self.assertEqual(rows[0]["status"], "unknown")
        self.assertIn("Core roster unavailable", rows[0]["detail"])

    def test_all_adds_mine_only_when_author_is_present(self) -> None:
        seen: list[str] = []

        def fake_discover(lane: str, author: str | None, gh: object) -> list[dict[str, object]]:
            seen.append(lane)
            return []

        with patch.object(queue, "discover", side_effect=fake_discover):
            queue.collect("all", None, 7, lambda *args: [], NOW, CORE)
            self.assertEqual(seen, ["maintainer", "second-core", "author-action", "stacked"])
            seen.clear()
            queue.collect("all", "maintainer-one", 7, lambda *args: [], NOW, CORE)
            self.assertEqual(seen[-1], "mine")

    def test_renderers_expose_uncertainty_and_links(self) -> None:
        row = queue.base_row(pr(), "maintainer", "unknown", "evidence unavailable")
        table = queue.render_table([row])
        self.assertIn("unknown", table)
        self.assertIn("https://github.com/zeroclaw-labs/zeroclaw/pull/1", table)
        self.assertIn("mine: omitted", queue.render_links("all", None))

    def test_json_rows_are_serializable(self) -> None:
        self.assertEqual(json.loads(json.dumps([queue.base_row(pr(), "stacked")]))[0]["queue"], "stacked")

    def test_terminal_text_is_sanitized(self) -> None:
        row = queue.base_row(pr(title="unsafe\n\x1b[31m"), "maintainer")
        self.assertEqual(row["title"], "unsafe\\n\\u001b[31m")

    def test_run_gh_preserves_error_and_timeout_context(self) -> None:
        failed = subprocess.CalledProcessError(1, ["gh"], stderr="denied")
        with patch("subprocess.run", side_effect=failed):
            with self.assertRaisesRegex(queue.GitHubReadError, "denied"):
                queue.run_gh("api", "x")
        with patch("subprocess.run", side_effect=subprocess.TimeoutExpired(["gh"], 30)):
            with self.assertRaisesRegex(queue.GitHubReadError, "30s"):
                queue.run_gh("api", "x")

    def test_main_requires_author_for_mine(self) -> None:
        self.assertEqual(queue.main(["--queue", "mine"]), 2)

    def test_links_format_needs_no_live_reads(self) -> None:
        result = queue.main(["--queue", "maintainer", "--format", "links"], lambda *args: self.fail("unexpected read"))
        self.assertEqual(result, 0)

    def test_threshold_rejects_nonfinite_and_negative_values(self) -> None:
        for value in ("nan", "inf", "-1"):
            with self.assertRaises(Exception):
                queue.finite_nonnegative(value)


if __name__ == "__main__":
    unittest.main()
