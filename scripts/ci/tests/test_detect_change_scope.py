#!/usr/bin/env python3
"""Focused tests for detect_change_scope.sh."""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / "scripts" / "ci" / "detect_change_scope.sh"


def run_cmd(cmd: list[str], *, cwd: Path, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=str(cwd),
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


def parse_github_output(output_path: Path) -> dict[str, str | list[str]]:
    lines = output_path.read_text(encoding="utf-8").splitlines()
    parsed: dict[str, str | list[str]] = {}
    i = 0
    while i < len(lines):
        line = lines[i]
        if line.endswith("<<EOF"):
            key = line.split("<<", 1)[0]
            i += 1
            values: list[str] = []
            while i < len(lines) and lines[i] != "EOF":
                if lines[i] != "":
                    values.append(lines[i])
                i += 1
            parsed[key] = values
        elif "=" in line:
            key, value = line.split("=", 1)
            parsed[key] = value
        i += 1
    return parsed


class DetectChangeScopeTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="zc-detect-scope-"))
        self.addCleanup(lambda: shutil.rmtree(self.tmp, ignore_errors=True))

        self._assert_cmd_ok(["git", "init", "-q"], "git init")
        self._assert_cmd_ok(["git", "checkout", "-q", "-b", "main"], "git checkout -b main")
        self._assert_cmd_ok(["git", "config", "user.name", "CI Test"], "git config user.name")
        self._assert_cmd_ok(["git", "config", "user.email", "ci@example.com"], "git config user.email")

    def _assert_cmd_ok(self, cmd: list[str], desc: str) -> None:
        proc = run_cmd(cmd, cwd=self.tmp)
        self.assertEqual(proc.returncode, 0, msg=f"{desc} failed: {proc.stderr}\n{proc.stdout}")

    def _commit(self, message: str) -> str:
        proc = run_cmd(["git", "commit", "-q", "-m", message], cwd=self.tmp)
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)
        sha = run_cmd(["git", "rev-parse", "HEAD"], cwd=self.tmp)
        self.assertEqual(sha.returncode, 0, msg=sha.stderr)
        return sha.stdout.strip()

    def _run_scope(self, *, event_name: str, base_sha: str) -> dict[str, str | list[str]]:
        output_path = self.tmp / "github_output.txt"
        env = {
            "PATH": os.environ.get("PATH") or "/usr/bin:/bin",
            "GITHUB_OUTPUT": str(output_path),
            "EVENT_NAME": event_name,
            "BASE_SHA": base_sha,
        }
        proc = run_cmd(["bash", str(SCRIPT)], cwd=self.tmp, env=env)
        self.assertEqual(proc.returncode, 0, msg=f"{proc.stderr}\n{proc.stdout}")
        return parse_github_output(output_path)

    def test_pull_request_merge_commit_uses_merge_parents(self) -> None:
        (self.tmp / "src").mkdir(parents=True, exist_ok=True)
        (self.tmp / "src" / "lib.rs").write_text("pub fn answer() -> i32 { 42 }\n", encoding="utf-8")
        self._assert_cmd_ok(["git", "add", "src/lib.rs"], "git add src/lib.rs")
        stale_base = self._commit("base")

        self._assert_cmd_ok(
            ["git", "checkout", "-q", "-b", "feature/workflow-only"],
            "git checkout -b feature/workflow-only",
        )
        (self.tmp / ".github" / "workflows").mkdir(parents=True, exist_ok=True)
        (self.tmp / ".github" / "workflows" / "ci-example.yml").write_text(
            "name: Example\non: pull_request\njobs: {}\n",
            encoding="utf-8",
        )
        self._assert_cmd_ok(
            ["git", "add", ".github/workflows/ci-example.yml"],
            "git add .github/workflows/ci-example.yml",
        )
        self._commit("feature: workflow only")

        self._assert_cmd_ok(["git", "checkout", "-q", "main"], "git checkout main")
        (self.tmp / "src" / "lib.rs").write_text("pub fn answer() -> i32 { 43 }\n", encoding="utf-8")
        self._assert_cmd_ok(["git", "add", "src/lib.rs"], "git add src/lib.rs")
        main_tip = self._commit("main: rust change after feature fork")

        merge_proc = run_cmd(
            ["git", "merge", "--no-ff", "-q", "feature/workflow-only", "-m", "merge feature"],
            cwd=self.tmp,
        )
        self.assertEqual(merge_proc.returncode, 0, msg=merge_proc.stderr)

        out = self._run_scope(event_name="pull_request", base_sha=stale_base)
        self.assertEqual(out["rust_changed"], "false")
        self.assertEqual(out["workflow_changed"], "true")
        self.assertEqual(out["docs_changed"], "false")
        self.assertEqual(out["docs_only"], "false")
        self.assertEqual(out["base_sha"], main_tip)
        self.assertEqual(out["docs_files"], [])

    def test_push_event_falls_back_to_merge_base(self) -> None:
        (self.tmp / "src").mkdir(parents=True, exist_ok=True)
        (self.tmp / "src" / "lib.rs").write_text("pub fn alpha() {}\n", encoding="utf-8")
        self._assert_cmd_ok(["git", "add", "src/lib.rs"], "git add src/lib.rs")
        common_base = self._commit("base")

        self._assert_cmd_ok(
            ["git", "checkout", "-q", "-b", "feature/rust-change"],
            "git checkout -b feature/rust-change",
        )
        (self.tmp / "src" / "lib.rs").write_text("pub fn alpha() {}\npub fn beta() {}\n", encoding="utf-8")
        self._assert_cmd_ok(["git", "add", "src/lib.rs"], "git add src/lib.rs")
        self._commit("feature: rust change")

        self._assert_cmd_ok(["git", "checkout", "-q", "main"], "git checkout main")
        (self.tmp / "README.md").write_text("# docs touch\n", encoding="utf-8")
        self._assert_cmd_ok(["git", "add", "README.md"], "git add README.md")
        advanced_base = self._commit("main advanced")

        self._assert_cmd_ok(
            ["git", "checkout", "-q", "feature/rust-change"],
            "git checkout feature/rust-change",
        )
        out = self._run_scope(event_name="push", base_sha=advanced_base)
        self.assertEqual(out["rust_changed"], "true")
        self.assertEqual(out["workflow_changed"], "false")
        self.assertEqual(out["docs_changed"], "false")
        self.assertEqual(out["docs_only"], "false")
        self.assertEqual(out["base_sha"], common_base)


if __name__ == "__main__":
    unittest.main()
