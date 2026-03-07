#!/usr/bin/env python3
"""Behavior tests for root install.sh entrypoint defaults."""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[3]
INSTALL_SH = ROOT / "install.sh"


def run_cmd(
    cmd: list[str],
    *,
    cwd: Path,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=str(cwd),
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


class InstallEntrypointTest(unittest.TestCase):
    maxDiff = None

    def setUp(self) -> None:
        self.tmp = Path(tempfile.mkdtemp(prefix="zc-install-entrypoint-"))
        self.addCleanup(lambda: shutil.rmtree(self.tmp, ignore_errors=True))

        self.install = self.tmp / "install.sh"
        self.bootstrap = self.tmp / "mock-bootstrap.sh"

        self.install.write_text(INSTALL_SH.read_text(encoding="utf-8"), encoding="utf-8")
        self.install.chmod(0o755)

        self.bootstrap.write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env bash
                set -euo pipefail
                echo "[mock-bootstrap] received args: $*"
                """
            ),
            encoding="utf-8",
        )
        self.bootstrap.chmod(0o755)

    def test_non_tty_no_args_does_not_force_interactive_defaults(self) -> None:
        env = {
            "PATH": os.environ.get("PATH") or "/usr/bin:/bin",
            "ZEROCLAW_BOOTSTRAP_URL": f"file://{self.bootstrap}",
        }
        proc = run_cmd(["bash", str(self.install)], cwd=self.tmp, env=env)
        self.assertEqual(proc.returncode, 0, msg=f"{proc.stderr}\n{proc.stdout}")
        self.assertIn("[mock-bootstrap] received args:", proc.stdout)
        self.assertNotIn("--interactive-onboard", proc.stdout)
        self.assertNotIn("--install-system-deps", proc.stdout)
        self.assertNotIn("--install-rust", proc.stdout)

    def test_explicit_args_are_forwarded_unchanged(self) -> None:
        env = {
            "PATH": os.environ.get("PATH") or "/usr/bin:/bin",
            "ZEROCLAW_BOOTSTRAP_URL": f"file://{self.bootstrap}",
        }
        proc = run_cmd(
            [
                "bash",
                str(self.install),
                "--onboard",
                "--api-key",
                "demo-key",
                "--provider",
                "openai",
            ],
            cwd=self.tmp,
            env=env,
        )
        self.assertEqual(proc.returncode, 0, msg=f"{proc.stderr}\n{proc.stdout}")
        self.assertIn(
            "[mock-bootstrap] received args: --onboard --api-key demo-key --provider openai",
            proc.stdout,
        )

    def test_tty_no_args_forwards_all_in_one_defaults(self) -> None:
        script_cmd = shutil.which("script")
        if script_cmd is None:
            self.skipTest("`script` command is required for PTY-based installer test")

        env = {
            "PATH": os.environ.get("PATH") or "/usr/bin:/bin",
        }
        pty_cmd = (
            f'ZEROCLAW_BOOTSTRAP_URL="file://{self.bootstrap}" '
            f"bash {self.install}"
        )
        proc = run_cmd([script_cmd, "-q", "/dev/null", "bash", "-lc", pty_cmd], cwd=self.tmp, env=env)
        self.assertEqual(proc.returncode, 0, msg=f"{proc.stderr}\n{proc.stdout}")
        self.assertIn(
            "[mock-bootstrap] received args: --install-system-deps --install-rust --interactive-onboard",
            proc.stdout,
        )


if __name__ == "__main__":
    unittest.main()
