#!/usr/bin/env python3
"""Exercise the Nix hash drift gate through its command-line interface."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("list_git_dep_keys.py")
GIT_PACKAGE = '''
[[package]]
name = "fixture"
version = "1.0.0"
source = "git+https://example.invalid/fixture?rev=0123456789abcdef#0123456789abcdef"
'''
REGISTRY_PACKAGE = '''
[[package]]
name = "fixture"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
'''


class HashDriftTest(unittest.TestCase):
    def run_gate(self, lock, hashes):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.lock").write_text(lock)
            (root / "hashes.json").write_text(json.dumps(hashes))
            return subprocess.run(
                [sys.executable, str(SCRIPT), str(root / "Cargo.lock"),
                 "--check-hashes", str(root / "hashes.json")],
                capture_output=True, text=True, check=False,
            )

    def test_empty_git_dependencies_accept_empty_hashes(self):
        result = self.run_gate(REGISTRY_PACKAGE, {})
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_registry_migration_rejects_stale_hash(self):
        result = self.run_gate(REGISTRY_PACKAGE, {"fixture-1.0.0": "fixture-hash"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Stale hash keys: fixture-1.0.0", result.stderr)

    def test_missing_hash_is_rejected(self):
        result = self.run_gate(GIT_PACKAGE, {})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Missing hash keys: fixture-1.0.0", result.stderr)

    def test_extra_hash_is_rejected_with_git_dependencies(self):
        result = self.run_gate(GIT_PACKAGE, {
            "fixture-1.0.0": "fixture-hash", "obsolete-0.1.0": "obsolete-hash",
        })
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Stale hash keys: obsolete-0.1.0", result.stderr)

    def test_matching_git_dependencies_pass(self):
        result = self.run_gate(GIT_PACKAGE, {"fixture-1.0.0": "fixture-hash"})
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
