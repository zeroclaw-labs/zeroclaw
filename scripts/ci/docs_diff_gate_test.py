#!/usr/bin/env python3

import os
import shlex
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


class DocsDiffGateTest(unittest.TestCase):
    SCRIPTS = Path(__file__).resolve().parent
    PAGE = "docs/book/src/page.md"
    GATES = ("docs_quality_gate.sh", "docs_links_gate.sh")
    GIT_ERROR = "fatal: simulated Git diff read error"

    @classmethod
    def setUpClass(cls) -> None:
        temp = tempfile.TemporaryDirectory()
        cls.addClassCleanup(temp.cleanup)
        directory = Path(temp.name).resolve()
        cls.repo = directory / "repo"
        cls.repo.mkdir()
        git = shutil.which("git")
        if git is None:
            raise RuntimeError("Git is required for the docs diff gate tests")
        cls.env = {
            key: value for key, value in os.environ.items() if not key.startswith("GIT_")
        }
        cls.env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)

        def run_git(*args: str) -> str:
            result = subprocess.run(
                [git, "-c", "user.name=Docs Gate Test", "-c",
                 "user.email=docs-gate@example.invalid", *args],
                cwd=cls.repo,
                env=cls.env,
                check=True,
                capture_output=True,
                text=True,
                timeout=20,
            )
            return result.stdout.strip()

        run_git("init", "--quiet", "--template=")
        page = cls.repo / cls.PAGE
        page.parent.mkdir(parents=True)
        page.write_text("# Page\n\nOld paragraph.\n", encoding="utf-8")
        (page.parent / "guide.md").write_text("# Guide\n", encoding="utf-8")
        run_git("add", "docs")
        run_git("commit", "--quiet", "-m", "base")
        cls.base = run_git("rev-parse", "HEAD")
        page.write_text("# Page\n\nRead the [guide](guide.md).\n", encoding="utf-8")
        run_git("add", cls.PAGE)
        run_git("commit", "--quiet", "-m", "edit docs")
        cls.head = run_git("rev-parse", "HEAD")

        # The link gate calls the collector and its own tests by relative path.
        scripts = cls.repo / "scripts" / "ci"
        scripts.mkdir(parents=True)
        for name in (*cls.GATES, "collect_changed_links.py", "collect_changed_links_test.py"):
            shutil.copyfile(cls.SCRIPTS / name, scripts / name)
        (cls.repo / ".markdownlint-cli2.yaml").write_text("config: {}\n", encoding="utf-8")

        bin_dir = directory / "bin"
        bin_dir.mkdir()

        def write_command(name: str, body: str) -> None:
            path = bin_dir / name
            path.write_text("#!/bin/sh\n" + body, encoding="utf-8")
            path.chmod(0o755)

        write_command("git", f"""\
if [ "$PWD" = {shlex.quote(str(cls.repo))} ] &&
   [ "$1" = diff ] && [ "$2" = "$DOCS_TEST_FAIL_DIFF" ]; then
    printf '%s\\n' {shlex.quote(cls.GIT_ERROR)} >&2
    exit 128
fi
exec {shlex.quote(git)} "$@"
""")
        # Make the quality gate enter its changed-line filter without Node or the network.
        write_command("npx", f"""\
printf '%s\\n' '{cls.PAGE}:1 error MD001/heading-increment fixture issue on an unchanged line'
exit 1
""")
        cls.env["PATH"] = f"{bin_dir}{os.pathsep}{cls.env['PATH']}"

    def run_gate(
        self, gate: str, *, fail_diff: str = "", empty: bool = False, files: str = ""
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", f"scripts/ci/{gate}"],
            cwd=self.repo,
            env={
                **self.env,
                "BASE_SHA": self.head if empty else self.base,
                "DOCS_FILES": files,
                "DOCS_TEST_FAIL_DIFF": fail_diff,
            },
            capture_output=True,
            text=True,
            timeout=30,
        )

    def test_git_errors_fail_both_gates_and_preserve_diagnostic(self) -> None:
        for gate in self.GATES:
            for phase in ("--name-only", "--unified=0"):
                with self.subTest(gate=gate, phase=phase):
                    result = self.run_gate(gate, fail_diff=phase)
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertIn(self.GIT_ERROR, result.stderr)

    def test_successful_empty_discovery_passes_both_gates(self) -> None:
        for gate, expected in zip(self.GATES, (
            "No docs files detected; skipping docs quality gate.",
            "No added links detected in changed docs lines.",
        )):
            with self.subTest(gate=gate):
                result = self.run_gate(gate, empty=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn(expected, result.stdout)

    def test_successful_empty_content_diff_passes_both_gates(self) -> None:
        for gate, expected in zip(self.GATES, (
            "No blocking markdown issues on changed lines.",
            "Collected 0 added link(s) from 1 docs file(s).",
        )):
            with self.subTest(gate=gate):
                result = self.run_gate(gate, empty=True, files=self.PAGE)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn(expected, result.stdout)


if __name__ == "__main__":
    unittest.main()
