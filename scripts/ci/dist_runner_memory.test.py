"""Exercise swapfile setup and cleanup in a temporary runner directory."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("dist_runner_memory.sh").resolve()
RUNNER_COMMANDS = r'''#!/bin/bash
set -euo pipefail
case "${0##*/}" in
  df) printf 'Available\n17179869184\n' ;;
  free) ;;
  stat) /usr/bin/stat -c '%d:%i:0:%h:%a' -- "${@: -1}" ;;
  swapon)
    if [[ "$1" == --show=NAME && -f "$SWAP_FIXTURE_ACTIVE" ]]; then
      cat "$SWAP_FIXTURE_ACTIVE"
    fi ;;
  sudo)
    operation="$1"
    shift
    case "$operation" in
      mktemp|chmod) command "$operation" "$@" ;;
      fallocate) : ;;
      mkswap) [[ "$SWAP_FIXTURE_FAILURE" != mkswap ]] || exit 31 ;;
      swapon)
        printf '%s\n' "${@: -1}" > "$SWAP_FIXTURE_ACTIVE"
        [[ "$SWAP_FIXTURE_FAILURE" != swapon ]] || exit 32 ;;
      swapoff)
        printf '%s\n' "${@: -1}" >> "$SWAP_FIXTURE_OFF_CALLS"
        [[ -f "$SWAP_FIXTURE_ACTIVE" ]] || exit 2
        rm -- "$SWAP_FIXTURE_ACTIVE" ;;
      dmesg) ;;
      *) exit 90 ;;
    esac ;;
  *) exit 91 ;;
esac
'''


@unittest.skipUnless(sys.platform.startswith("linux"), "requires Linux runner utilities")
class DistRunnerMemoryTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.runner_temp = self.root / "runner-temp"
        self.runner_temp.mkdir()
        commands = self.root / "bin"
        commands.mkdir()
        fixture = commands / "runner-commands"
        fixture.write_text(RUNNER_COMMANDS)
        fixture.chmod(0o700)
        for command in ("df", "free", "stat", "swapon", "sudo"):
            (commands / command).symlink_to(fixture)
        self.receipt = self.root / "output"
        self.active = self.root / "active-swap"
        self.off_calls = self.root / "swapoff-calls"
        self.env = {
            "GITHUB_ACTIONS": "true", "RUNNER_OS": "Linux",
            "RUNNER_TEMP": str(self.runner_temp), "GITHUB_OUTPUT": str(self.receipt),
            "PATH": f"{commands}{os.pathsep}{os.defpath}",
            "SWAP_FIXTURE_ACTIVE": str(self.active),
            "SWAP_FIXTURE_OFF_CALLS": str(self.off_calls), "SWAP_FIXTURE_FAILURE": "",
        }

    def run_script(self, action, **env):
        return subprocess.run(
            ["/bin/bash", str(SCRIPT), action], env={**self.env, **env},
            capture_output=True, text=True, timeout=5,
        )

    def test_failed_mkswap_removes_the_owned_inactive_file(self):
        result = self.run_script("prepare", SWAP_FIXTURE_FAILURE="mkswap")
        self.assertEqual(result.returncode, 31, result.stderr)
        self.assertFalse(self.receipt.exists())
        self.assertFalse(self.active.exists())
        self.assertEqual(len(list(self.runner_temp.iterdir())), 0, result.stderr)
        self.assertFalse(self.off_calls.exists())

    def test_failed_activation_still_deactivates_before_removal(self):
        result = self.run_script("prepare", SWAP_FIXTURE_FAILURE="swapon")
        self.assertEqual(result.returncode, 32, result.stderr)
        self.assertFalse(self.receipt.exists())
        self.assertFalse(self.active.exists())
        self.assertEqual(list(self.runner_temp.iterdir()), [])
        self.assertEqual(len(self.off_calls.read_text().splitlines()), 1)

    def test_successful_setup_still_cleans_up_from_its_receipt(self):
        prepared = self.run_script("prepare")
        self.assertEqual(prepared.returncode, 0, prepared.stderr)
        receipt = dict(line.split("=", 1) for line in self.receipt.read_text().splitlines())
        self.assertTrue(Path(receipt["swap_file"]).is_file())
        result = self.run_script(
            "cleanup", ZC_DIST_SWAP_FILE=receipt["swap_file"],
            ZC_DIST_SWAP_IDENTITY=receipt["swap_identity"],
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(self.active.exists())
        self.assertEqual(list(self.runner_temp.iterdir()), [])
        self.assertEqual(self.off_calls.read_text().splitlines(), [receipt["swap_file"]])


if __name__ == "__main__":
    unittest.main()
