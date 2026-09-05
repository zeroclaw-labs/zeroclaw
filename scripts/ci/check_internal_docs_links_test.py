#!/usr/bin/env python3

from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


class InternalDocsLinksTest(unittest.TestCase):
    SCRIPT = Path(__file__).with_name("check_internal_docs_links.py").resolve()

    def run_checker(self, files: dict[str, str]) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir) / "docs/book/src"
            for relative, content in files.items():
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content, encoding="utf-8")
            return subprocess.run(
                [sys.executable, str(self.SCRIPT), "--root", str(root)],
                check=False,
                capture_output=True,
                text=True,
            )

    def test_accepts_existing_markdown_link_and_summary_entry(self) -> None:
        result = self.run_checker(
            {
                "SUMMARY.md": "- [Guide](guide.md)\n",
                "index.md": "[Guide](guide.md)\n",
                "guide.md": "# Guide\n",
            }
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_reports_missing_target_with_source_location(self) -> None:
        result = self.run_checker({"index.md": "[Missing](missing.md)\n"})

        self.assertEqual(result.returncode, 1)
        self.assertIn("index.md:1", result.stdout)
        self.assertIn("missing.md", result.stdout)

    def test_ignores_generated_reference_targets(self) -> None:
        result = self.run_checker(
            {
                "SUMMARY.md": "- [CLI](reference/cli.md)\n",
                "index.md": "[Config](reference/config.md#providers)\n",
            }
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_ignores_code_fences_external_links_and_fragments(self) -> None:
        result = self.run_checker(
            {
                "index.md": (
                    "[Guide](guide.md#intro)\n"
                    "```md\n[Missing](missing.md)\n```\n"
                    "[External](https://example.com/missing.md)\n"
                ),
                "guide.md": "# Guide\n## Intro\n",
            }
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
