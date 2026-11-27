#!/usr/bin/env python3
"""Exercise the Nix hash drift gate through its command-line interface."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("list_git_dep_keys.py")
REFRESH = SCRIPT.parent.parent / "dev" / "refresh-nix-hashes.sh"
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

    @unittest.skipUnless(shutil.which("cargo") and shutil.which("jq"), "requires cargo and jq")
    def test_refresh_hashes_uses_locked_git_revisions(self):
        repourl = "https://example.invalid/fixtures"
        revisions = {
            "branch": ("?branch=main", "a" * 40),
            "tag": ("?tag=v1", "a" * 40),
            "rev": ("?rev=bbbbbbb", "b" * 40),
            "default": ("", "c" * 40),
        }
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            refresh = root / "scripts/dev/refresh-nix-hashes.sh"
            refresh.parent.mkdir(parents=True)
            shutil.copyfile(REFRESH, refresh)
            hashes = root / "nix/hashes.json"
            hashes.parent.mkdir()
            bin_dir = root / "bin"
            bin_dir.mkdir()
            prefetch = bin_dir / "nix-prefetch-git"
            prefetch.write_text(
                '#!/bin/sh\nset -eu\n[ "$#" -eq 3 ] && [ "$2" = --rev ] || exit 1\n'
                'printf "%s %s\\n" "$1" "$3" >> "$PREFETCH_CALLS"\n'
                'printf \'{"hash":"fixture-%s"}\\n\' "$3"\n'
            )
            prefetch.chmod(0o755)
            calls = root / "prefetch-calls"
            lock = root / "Cargo.lock"
            lock.write_text(REGISTRY_PACKAGE + "".join(
                f'\n[[package]]\nname = "{name}"\nversion = "1.0.0"\n'
                f'source = "git+{repourl}{selector}#{sha}"\n'
                for name, (selector, sha) in revisions.items()
            ))
            result = subprocess.run(
                ["bash", str(refresh)], capture_output=True, text=True, check=False,
                env={**os.environ, "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                     "PREFETCH_CALLS": str(calls)},
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(json.loads(hashes.read_text()), {
                f"{name}-1.0.0": f"fixture-{sha}" for name, (_, sha) in revisions.items()
            })
            self.assertCountEqual(calls.read_text().splitlines(),
                                  [f"{repourl} {sha * 40}" for sha in "abc"])

if __name__ == "__main__":
    unittest.main()
