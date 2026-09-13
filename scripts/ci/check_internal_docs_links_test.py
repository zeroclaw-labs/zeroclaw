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

    def test_reports_missing_target_in_anchored_include(self) -> None:
        result = self.run_checker(
            {
                "guides/index.md": "{{#include ../_snippets/shared.md:example}}\n",
                "_snippets/shared.md": (
                    "<!-- ANCHOR: example -->\n"
                    "[Missing](missing.md)\n"
                    "<!-- ANCHOR_END: example -->\n"
                    "<!-- ANCHOR: other -->\n"
                    "[Unrendered](unrendered.md)\n"
                    "<!-- ANCHOR_END: other -->\n"
                ),
            }
        )

        self.assertEqual(result.returncode, 1)
        self.assertIn("_snippets/shared.md:2", result.stdout)
        self.assertIn("guides/missing.md", result.stdout)
        self.assertNotIn("unrendered.md", result.stdout)

    def test_scopes_anchored_links_to_each_rendered_page(self) -> None:
        result = self.run_checker(
            {
                "guides/a.md": "{{#include ../_snippets/shared.md:a}}\n",
                "guides/b.md": "{{#include ../_snippets/shared.md:b}}\n",
                "guides/a-target.md": "# A\n",
                "_snippets/shared.md": (
                    "<!-- ANCHOR: a -->\n"
                    "[A](a-target.md)\n"
                    "<!-- ANCHOR_END: a -->\n"
                    "<!-- ANCHOR: b -->\n"
                    "[B](b-target.md)\n"
                    "<!-- ANCHOR_END: b -->\n"
                ),
            }
        )

        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout.count("guides/b-target.md"), 1)
        self.assertNotIn("guides/a-target.md", result.stdout)

    def test_resolves_nested_include_links_from_rendered_page(self) -> None:
        result = self.run_checker(
            {
                "guides/index.md": "{{#include ../_snippets/outer.md:chosen}}\n",
                "guides/guide.md": "# Guide\n",
                "_snippets/outer.md": (
                    "<!-- ANCHOR: chosen -->\n"
                    "{{#include inner.md:chosen}}\n"
                    "<!-- ANCHOR_END: chosen -->\n"
                    "<!-- ANCHOR: other -->\n"
                    "{{#include inner.md:other}}\n"
                    "<!-- ANCHOR_END: other -->\n"
                ),
                "_snippets/inner.md": (
                    "<!-- ANCHOR: chosen -->\n"
                    "[Guide](guide.md)\n"
                    "<!-- ANCHOR_END: chosen -->\n"
                    "<!-- ANCHOR: other -->\n"
                    "[Missing](missing.md)\n"
                    "<!-- ANCHOR_END: other -->\n"
                ),
            }
        )

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_resolves_line_range_from_rendered_page(self) -> None:
        result = self.run_checker(
            {
                "guides/index.md": "{{#include ../_snippets/shared.md:2:2}}\n",
                "guides/guide.md": "# Guide\n",
                "_snippets/shared.md": "ignored\n[Guide](guide.md)\n[Missing](missing.md)\n",
            }
        )

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_ignores_inline_and_nested_fenced_examples_but_checks_following_link(self) -> None:
        result = self.run_checker(
            {
                "index.md": (
                    "`[Inline](inline-placeholder.md)`\n"
                    "`[Multiline](multiline-placeholder.md\n"
                    "continues)`\n"
                    "```md\n"
                    "[Example](fenced-placeholder.md)\n"
                    "``` rust\n"
                    "[StillExample](trailing-fence-placeholder.md)\n"
                    "```\n"
                    "````md\n"
                    "```md\n"
                    "[NestedExample](nested-fenced-placeholder.md)\n"
                    "```\n"
                    "````\n"
                    "[Missing](missing-after-fence.md)\n"
                ),
            }
        )

        self.assertEqual(result.returncode, 1)
        self.assertIn("missing-after-fence.md", result.stdout)
        self.assertNotIn("inline-placeholder.md", result.stdout)
        self.assertNotIn("multiline-placeholder.md", result.stdout)
        self.assertNotIn("fenced-placeholder.md", result.stdout)
        self.assertNotIn("trailing-fence-placeholder.md", result.stdout)
        self.assertNotIn("nested-fenced-placeholder.md", result.stdout)

    def test_unmatched_or_escaped_backticks_do_not_hide_real_links(self) -> None:
        cases = (
            (
                "unmatched backtick in the same paragraph",
                "A literal ` character. [Missing](same-paragraph.md)\n",
                "same-paragraph.md",
            ),
            (
                "escaped backtick",
                "A literal \\` character. [Missing](escaped-backtick.md)\n",
                "escaped-backtick.md",
            ),
            (
                "backticks in separate paragraphs",
                "A literal ` character.\n\n"
                "[Missing](between-paragraphs.md)\n\n"
                "Another literal ` character.\n",
                "between-paragraphs.md",
            ),
            (
                "fenced block between an unmatched backtick and a link",
                "A literal ` character.\n"
                "```md\n[Example](fenced-example.md)\n```\n"
                "[Missing](after-fence.md)\n",
                "after-fence.md",
            ),
            (
                "blockquote after an unmatched backtick",
                "A literal ` character.\n> [Missing](after-blockquote.md)\n",
                "after-blockquote.md",
            ),
            (
                "heading after an unmatched backtick",
                "A literal ` character.\n# Heading\n[Missing](after-heading.md)\n",
                "after-heading.md",
            ),
            (
                "list after an unmatched backtick",
                "A literal ` character.\n- [Missing](after-list.md)\n",
                "after-list.md",
            ),
        )

        for name, content, expected_target in cases:
            with self.subTest(name=name):
                result = self.run_checker({"index.md": content})

                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn(expected_target, result.stdout)

    def test_preserves_multiline_inline_code_in_blockquotes(self) -> None:
        result = self.run_checker(
            {
                "index.md": (
                    "> `[Example](quoted-placeholder.md)\n"
                    "> continues`\n"
                    "> [Missing](missing-after-quote.md)\n"
                ),
            }
        )

        self.assertEqual(result.returncode, 1)
        self.assertIn("missing-after-quote.md", result.stdout)
        self.assertNotIn("quoted-placeholder.md", result.stdout)

    def test_preserves_multiline_inline_code_in_list_items(self) -> None:
        result = self.run_checker(
            {
                "index.md": (
                    "- `[Example](list-placeholder.md)\n"
                    "  continues`\n"
                    "- [Missing](missing-after-list-item.md)\n"
                ),
            }
        )

        self.assertEqual(result.returncode, 1)
        self.assertIn("missing-after-list-item.md", result.stdout)
        self.assertNotIn("list-placeholder.md", result.stdout)

    def test_even_backslash_run_does_not_escape_code_span_delimiter(self) -> None:
        result = self.run_checker(
            {"index.md": "A literal \\\\`[Example](inline-placeholder.md)`\n"}
        )

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
