#!/usr/bin/env python3
"""Static contract tests for the public maintainer dashboard."""

from __future__ import annotations

import json
from pathlib import Path
import re
import unittest
from urllib.parse import urlparse


ROOT = Path(__file__).resolve().parents[2]
BOOK = ROOT / "docs/book"
PAGE = BOOK / "src/maintainers/dashboard.md"
ZT5 = BOOK / "src/maintainers/zt5-public-status.json"
SCRIPT = BOOK / "theme/maintainer-dashboard.js"


class MaintainerDashboardTest(unittest.TestCase):
    def test_page_is_registered_and_script_is_loaded(self) -> None:
        summary = (BOOK / "src/SUMMARY.md").read_text()
        config = (BOOK / "book.toml").read_text()
        page = PAGE.read_text()

        self.assertIn("./maintainers/dashboard.md", summary)
        self.assertIn('"theme/maintainer-dashboard.js"', config)
        self.assertIn('id="maintainer-dashboard-app"', page)
        self.assertIn('data-zt5-url="zt5-public-status.json"', page)

    def test_script_is_page_scoped_and_has_failure_fallbacks(self) -> None:
        script = SCRIPT.read_text()

        self.assertIn('document.getElementById("maintainer-dashboard-app")', script)
        self.assertIn("https://api.github.com/search/issues", script)
        self.assertIn("Open GitHub search", script)
        self.assertIn("Unavailable", script)
        self.assertNotIn("Authorization", script)
        self.assertNotIn("api.github.com/graphql", script)

    def test_refreshes_are_bounded_and_stale_results_are_ignored(self) -> None:
        script = SCRIPT.read_text()

        self.assertIn("var searchCache = new Map()", script)
        self.assertIn("var controller = new AbortController()", script)
        self.assertIn("signal: controller.signal", script)
        self.assertIn("if (generation !== renderGeneration)", script)
        self.assertIn("Responses are cached for up to one minute", script)
        self.assertNotIn("searchCache.delete(definition.query)", script)

    def test_dashboard_queries_remain_report_only(self) -> None:
        script = SCRIPT.read_text()

        self.assertIn('label:\"needs-maintainer-review\"', script)
        self.assertIn('label:\"needs-author-action\"', script)
        self.assertIn('label:\"risk:high\",\"domain:security\" review:approved', script)
        self.assertIn("is:issue is:open assignee:", script)
        self.assertNotRegex(script, re.compile(r'fetch\([^)]*method\s*:\s*["\'](?:POST|PUT|PATCH|DELETE)', re.I | re.S))

    def test_public_zt5_snapshot_uses_only_allowlisted_fields(self) -> None:
        payload = json.loads(ZT5.read_text())
        self.assertEqual(payload["schema_version"], 1)
        self.assertRegex(payload["as_of"], r"^\d{4}-\d{2}-\d{2}$")
        self.assertGreater(len(payload["capabilities"]), 0)
        self.assertEqual(set(payload), {"schema_version", "as_of", "disclosure", "capabilities"})

        allowed_capability = {"name", "status", "score", "target", "summary", "links"}
        allowed_link = {"label", "url"}
        for capability in payload["capabilities"]:
            self.assertEqual(set(capability), allowed_capability)
            self.assertIsNone(capability["score"])
            self.assertEqual(capability["target"], 5)
            self.assertGreater(len(capability["links"]), 0)
            for link in capability["links"]:
                self.assertEqual(set(link), allowed_link)
                parsed = urlparse(link["url"])
                self.assertEqual(parsed.scheme, "https")
                self.assertEqual(parsed.netloc, "github.com")
                self.assertTrue(parsed.path.startswith("/zeroclaw-labs/zeroclaw/pull/"))

    def test_browser_rejects_non_public_zt5_shapes(self) -> None:
        script = SCRIPT.read_text()

        self.assertIn('capability.score === null', script)
        self.assertIn('capability.target === 5', script)
        self.assertIn('hasExactKeys(payload, ["schema_version", "as_of", "disclosure", "capabilities"])', script)
        self.assertIn('hasExactKeys(capability, ["name", "status", "score", "target", "summary", "links"])', script)
        self.assertIn('url.hostname === "github.com"', script)

    def test_public_snapshot_contains_no_machine_local_paths(self) -> None:
        text = ZT5.read_text()
        for marker in ("/Users/", "/Volumes/", "file://", "localhost", "127.0.0.1"):
            self.assertNotIn(marker, text)


if __name__ == "__main__":
    unittest.main()
