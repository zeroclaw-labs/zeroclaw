#!/usr/bin/env python3
"""Exercise the size gate as a process, with sparse files and no build."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("check_binary_size.sh")
MIB = 1024 * 1024


class BinarySizeGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="zeroclaw-size-gate-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.binary = self.root / "test binary"

    def run_gate(self, size, *, limit=None, label=None, summary=None):
        if size is not None:
            with self.binary.open("wb") as stream:
                stream.truncate(size)
        env = os.environ.copy()
        env.pop("BINARY_SIZE_HARD_LIMIT", None)
        env.pop("GITHUB_STEP_SUMMARY", None)
        if limit is not None:
            env["BINARY_SIZE_HARD_LIMIT"] = str(limit)
        if summary is not None:
            env["GITHUB_STEP_SUMMARY"] = str(summary)
        args = ["bash", str(SCRIPT), str(self.binary)]
        if label is not None:
            args.append(label)
        return subprocess.run(args, env=env, capture_output=True, text=True, timeout=10)

    def test_default_accepts_through_limit_without_stale_warnings(self):
        for size in [0, 5 * MIB + 1, 15 * MIB + 1, 20 * MIB]:
            with self.subTest(size=size):
                result = self.run_gate(size)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn(f"({size} bytes)", result.stdout)
                self.assertNotIn("::warning::", result.stdout + result.stderr)
                self.assertNotIn("::error::", result.stdout + result.stderr)

    def test_default_rejects_one_byte_over(self):
        result = self.run_gate(20 * MIB + 1)
        self.assertEqual(result.returncode, 1)
        self.assertIn("::error::Binary exceeds 20MB safeguard", result.stdout)

    def test_release_override_keeps_exact_boundary(self):
        for size in [80 * MIB - 1, 80 * MIB, 80 * MIB + 1]:
            with self.subTest(size=size):
                result = self.run_gate(size, limit=80 * MIB)
                self.assertEqual(result.returncode, int(size > 80 * MIB), result.stdout + result.stderr)
                self.assertIn(f"({size} bytes)", result.stdout)
                self.assertNotIn("::warning::", result.stdout + result.stderr)
                self.assertEqual("::error::" in result.stdout, size > 80 * MIB)

    def test_smaller_override_is_enforced(self):
        result = self.run_gate(1025, limit=1024)
        self.assertEqual(result.returncode, 1)
        self.assertIn("::error::Binary exceeds", result.stdout)

    def test_missing_binary_fails(self):
        result = self.run_gate(None)
        self.assertEqual(result.returncode, 1)
        self.assertIn("::error::Binary not found", result.stdout)

    def test_summary_records_exact_bytes_even_on_rejection(self):
        for size in [20 * MIB, 20 * MIB + 1]:
            with self.subTest(size=size):
                summary = self.root / f"summary-{size}.md"
                result = self.run_gate(size, label="test-target", summary=summary)
                self.assertEqual(result.returncode, int(size > 20 * MIB))
                self.assertEqual(summary.read_text(), f"### Binary Size: test-target\n- Size: 20MB ({size} bytes)\n")

    def test_summary_without_label_is_not_written(self):
        summary = self.root / "summary.md"
        result = self.run_gate(1, summary=summary)
        self.assertEqual(result.returncode, 0)
        self.assertFalse(summary.exists())


if __name__ == "__main__":
    unittest.main()
