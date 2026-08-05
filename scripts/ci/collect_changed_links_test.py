#!/usr/bin/env python3

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from collect_changed_links import mdbook_link_escapes_source, normalize_link_target


class MdBookLinkBoundaryTest(unittest.TestCase):
    SCRIPT = Path(__file__).with_name("collect_changed_links.py").resolve()

    def run_git(self, root: Path, *args: str) -> str:
        result = subprocess.run(
            ["git", *args],
            cwd=root,
            check=True,
            capture_output=True,
            text=True,
        )
        return result.stdout.strip()

    def run_collector(
        self, source_path: str, content: str
    ) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as temp_dir:
            root = Path(temp_dir)
            self.run_git(root, "init", "--quiet")
            self.run_git(root, "config", "user.name", "Link Gate Test")
            self.run_git(root, "config", "user.email", "link-gate@example.invalid")

            target = root / "docs/security/slsa-provenance.md"
            target.parent.mkdir(parents=True)
            target.write_text("# SLSA provenance\n", encoding="utf-8")
            self.run_git(root, "add", target.relative_to(root).as_posix())
            self.run_git(root, "commit", "--quiet", "-m", "base")
            base = self.run_git(root, "rev-parse", "HEAD")

            source = root / source_path
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_text(content, encoding="utf-8")
            self.run_git(root, "add", source_path)
            self.run_git(root, "commit", "--quiet", "-m", "add docs link")

            return subprocess.run(
                [
                    sys.executable,
                    str(self.SCRIPT),
                    "--base",
                    base,
                    "--docs-files",
                    source_path,
                    "--output",
                    str(root / "links.txt"),
                    "--check-local-targets",
                ],
                cwd=root,
                check=False,
                capture_output=True,
                text=True,
            )

    def test_rejects_repository_relative_target_outside_book_source(self) -> None:
        source = "docs/book/src/maintainers/release-verification.md"
        target = normalize_link_target("../../../security/slsa-provenance.md", source)

        self.assertEqual(target, "docs/security/slsa-provenance.md")
        self.assertTrue(mdbook_link_escapes_source(source, target))

    def test_accepts_target_within_book_source(self) -> None:
        source = "docs/book/src/maintainers/release-verification.md"
        target = normalize_link_target("../security/model.md", source)

        self.assertEqual(target, "docs/book/src/security/model.md")
        self.assertFalse(mdbook_link_escapes_source(source, target))

    def test_does_not_apply_book_boundary_to_other_repository_docs(self) -> None:
        source = "docs/maintainers/release-attestation-runbook.md"
        target = normalize_link_target("../security/slsa-provenance.md", source)

        self.assertEqual(target, "docs/security/slsa-provenance.md")
        self.assertFalse(mdbook_link_escapes_source(source, target))

    def test_accepts_http_target(self) -> None:
        source = "docs/book/src/maintainers/release-verification.md"
        target = "https://github.com/zeroclaw-labs/zeroclaw"

        self.assertFalse(mdbook_link_escapes_source(source, target))

    def test_cli_rejects_mdbook_link_outside_source_root(self) -> None:
        result = self.run_collector(
            "docs/book/src/maintainers/release-verification.md",
            "[SLSA](../../../security/slsa-provenance.md)\n",
        )

        self.assertEqual(result.returncode, 1)
        self.assertIn(
            "Relative mdBook link target(s) outside docs/book/src:", result.stdout
        )
        self.assertIn(
            "docs/book/src/maintainers/release-verification.md -> "
            "docs/security/slsa-provenance.md",
            result.stdout,
        )

    def test_cli_accepts_repository_link_outside_book(self) -> None:
        result = self.run_collector(
            "docs/maintainers/release-attestation-runbook.md",
            "[SLSA](../security/slsa-provenance.md)\n",
        )

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
