#!/usr/bin/env python3
"""Behavioral checks for release installer target selection helpers."""

from __future__ import annotations

import subprocess
import textwrap
import unittest
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
INSTALL_RELEASE = ROOT / "scripts" / "install-release.sh"
BOOTSTRAP = ROOT / "scripts" / "bootstrap.sh"
PUB_RELEASE = ROOT / ".github" / "workflows" / "pub-release.yml"


def run_cmd(cmd: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        text=True,
        capture_output=True,
        check=False,
    )


def extract_function(function_name: str, script_path: Path) -> str:
    lines = script_path.read_text(encoding="utf-8").splitlines()
    start = None
    for index, line in enumerate(lines):
        if line == f"{function_name}() {{":
            start = index
            break
    if start is None:
        raise AssertionError(f"could not find function {function_name} in {script_path}")

    body: list[str] = []
    for line in lines[start:]:
        body.append(line)
        if line == "}":
            break
    return "\n".join(body) + "\n"


def run_shell_function(script_path: Path, function_name: str, os_name: str, arch: str) -> list[str]:
    function_source = extract_function(function_name, script_path)
    shell = textwrap.dedent(
        f"""
        set -euo pipefail
        {function_source}
        uname() {{
          if [[ "${{1:-}}" == "-m" ]]; then
            printf '%s\\n' "{arch}"
          else
            printf '%s\\n' "{os_name}"
          fi
        }}
        {function_name}
        """
    )
    proc = run_cmd(["bash", "-lc", shell])
    if proc.returncode != 0:
        raise AssertionError(proc.stderr or proc.stdout)
    return [line for line in proc.stdout.splitlines() if line]


def workflow_target_os(target: str) -> str:
    workflow = PUB_RELEASE.read_text(encoding="utf-8")
    pattern = re.compile(
        rf"^\s+- os: (?P<os>.+)\n\s+target: {re.escape(target)}$",
        re.MULTILINE,
    )
    match = pattern.search(workflow)
    if match is None:
        raise AssertionError(f"could not find workflow target block for {target}")
    return match.group("os").strip()


class ReleaseInstallerTargetSelectionTest(unittest.TestCase):
    def test_install_release_prefers_musl_for_linux_x86_64(self) -> None:
        self.assertEqual(
            run_shell_function(INSTALL_RELEASE, "linux_triples", "Linux", "x86_64"),
            ["x86_64-unknown-linux-musl", "x86_64-unknown-linux-gnu"],
        )

    def test_install_release_prefers_musl_for_linux_aarch64(self) -> None:
        self.assertEqual(
            run_shell_function(INSTALL_RELEASE, "linux_triples", "Linux", "aarch64"),
            ["aarch64-unknown-linux-musl", "aarch64-unknown-linux-gnu"],
        )

    def test_bootstrap_prefers_musl_for_linux_x86_64(self) -> None:
        self.assertEqual(
            run_shell_function(BOOTSTRAP, "detect_release_targets", "Linux", "x86_64"),
            ["x86_64-unknown-linux-musl", "x86_64-unknown-linux-gnu"],
        )

    def test_bootstrap_preserves_non_linux_target_mapping(self) -> None:
        self.assertEqual(
            run_shell_function(BOOTSTRAP, "detect_release_targets", "Darwin", "arm64"),
            ["aarch64-apple-darwin"],
        )

    def test_pub_release_keeps_gnu_linux_targets_on_ubuntu_22_04(self) -> None:
        self.assertEqual(workflow_target_os("x86_64-unknown-linux-gnu"), "ubuntu-22.04")
        self.assertEqual(workflow_target_os("aarch64-unknown-linux-gnu"), "ubuntu-22.04")
        self.assertEqual(workflow_target_os("armv7-unknown-linux-gnueabihf"), "ubuntu-22.04")

    def test_pub_release_keeps_musl_linux_targets_on_self_hosted_runner(self) -> None:
        expected = "[self-hosted, Linux, X64, blacksmith-2vcpu-ubuntu-2404]"
        self.assertEqual(workflow_target_os("x86_64-unknown-linux-musl"), expected)
        self.assertEqual(workflow_target_os("aarch64-unknown-linux-musl"), expected)

    def test_scripts_remain_shell_parseable(self) -> None:
        proc = run_cmd(["bash", "-n", str(INSTALL_RELEASE), str(BOOTSTRAP)])
        self.assertEqual(proc.returncode, 0, msg=proc.stderr)


if __name__ == "__main__":
    unittest.main()
