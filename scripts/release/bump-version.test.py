"""Exercise release-mode failures through the actual version-bump script."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("bump-version.sh")


class ReleaseBumpTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "repo"
        self.bin = Path(self.temp.name) / "bin"
        self.bin.mkdir()
        (self.root / "scripts/release").mkdir(parents=True)
        (self.root / "scripts/dev").mkdir()
        (self.root / "docs/book/src").mkdir(parents=True)
        (self.root / "docs/book/src/intro.md").write_text("# Test release documentation\n")
        shutil.copyfile(SCRIPT, self.root / "scripts/release/bump-version.sh")
        (self.root / "Cargo.toml").write_text(
            '[workspace.package]\nversion = "0.8.5"\nrust-version = "1.96.0"\n'
        )
        (self.root / "Cargo.lock").write_text("# fixture lock\n")
        (self.root / "README.md").write_text(
            '<img src="version-v0.8.5-blue" alt="Version v0.8.5">\n'
        )
        self.calls = Path(self.temp.name) / "calls"
        self.env = {**os.environ, "PATH": str(self.bin), "BUMP_TEST_CALLS": str(self.calls)}
        # A closed PATH makes missing prerequisites deterministic even on hosts
        # with a complete release toolchain. Only generators are mocked.
        for name in ("dirname", "sed", "head", "awk", "grep", "perl", "sha256sum", "find", "cat", "mv"):
            executable = shutil.which(name)
            self.assertIsNotNone(executable, f"test needs {name}")
            (self.bin / name).symlink_to(executable)
        self.stub("jq", "exit 0")
        self.stub("nix-prefetch-git", "exit 0")
        self.stub("cargo", """
echo "$*" >> "$BUMP_TEST_CALLS"
case "$*" in
  'update --workspace --offline') exit "${BUMP_TEST_OFFLINE:-0}" ;;
  'update --workspace') exit "${BUMP_TEST_ONLINE:-0}" ;;
  'generate installers') exit "${BUMP_TEST_INSTALLERS:-0}" ;;
  *) exit 90 ;;
esac
""")
        self.refresh = self.root / "scripts/dev/refresh-nix-hashes.sh"
        self.refresh.write_text('#!/bin/sh\necho nix >> "$BUMP_TEST_CALLS"\nexit "${BUMP_TEST_NIX:-0}"\n')
        self.refresh.chmod(0o700)
        # The real script invokes its refresh helper via bash.
        (self.bin / "bash").symlink_to(shutil.which("bash"))

    def stub(self, name, body):
        path = self.bin / name
        path.write_text("#!/bin/sh\n" + body + "\n")
        path.chmod(0o700)

    def run_bump(self, *args, **env):
        return subprocess.run(
            ["/bin/bash", str(self.root / "scripts/release/bump-version.sh"), *args],
            cwd=self.temp.name, env={**self.env, **env}, text=True,
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=15,
        )

    def assert_incomplete(self, result, message):
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(message, result.stdout)
        self.assertNotIn("Done.", result.stdout)

    def test_release_success_runs_all_generators_from_repo(self):
        result = self.run_bump("--release", "0.8.6")
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.calls.read_text().splitlines(), ["update --workspace --offline", "nix", "generate installers"])
        self.assertIn('version = "0.8.6"', (self.root / "Cargo.toml").read_text())
        self.assertIn("version-v0.8.6-blue", (self.root / "README.md").read_text())

    def test_missing_tools_fail_before_mutation(self):
        for name in ("cargo", "jq", "nix-prefetch-git"):
            with self.subTest(name=name):
                path = self.bin / name
                saved = path.read_bytes()
                path.unlink()
                before = (self.root / "Cargo.toml").read_bytes()
                result = self.run_bump("--release", "0.8.6")
                self.assert_incomplete(result, f"requires {name}")
                self.assertEqual(before, (self.root / "Cargo.toml").read_bytes())
                self.assertFalse(self.calls.exists())
                path.write_bytes(saved)
                path.chmod(0o700)

    def test_missing_lock_fails_before_mutation(self):
        (self.root / "Cargo.lock").unlink()
        result = self.run_bump("--release", "0.8.6")
        self.assert_incomplete(result, "requires Cargo.lock")
        self.assertIn('version = "0.8.5"', (self.root / "Cargo.toml").read_text())

    def test_missing_refresh_helper_fails_before_mutation(self):
        self.refresh.unlink()
        result = self.run_bump("--release", "0.8.6")
        self.assert_incomplete(result, "requires Cargo.lock and executable")
        self.assertFalse(self.calls.exists())

    def test_failed_lock_refresh_prevents_later_generators(self):
        result = self.run_bump("--release", "0.8.6", BUMP_TEST_OFFLINE="1", BUMP_TEST_ONLINE="1")
        self.assert_incomplete(result, "cargo update --workspace failed")
        self.assertEqual(self.calls.read_text().splitlines(), ["update --workspace --offline", "update --workspace"])

    def test_online_fallback_is_retained(self):
        result = self.run_bump("--release", "0.8.6", BUMP_TEST_OFFLINE="1")
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("update --workspace\n", self.calls.read_text())

    def test_nix_failure_prevents_installer_generation(self):
        result = self.run_bump("--release", "0.8.6", BUMP_TEST_NIX="1")
        self.assert_incomplete(result, "refresh-nix-hashes.sh failed")
        self.assertNotIn("generate installers", self.calls.read_text())

    def test_installer_failure_is_not_success(self):
        result = self.run_bump("--release", "0.8.6", BUMP_TEST_INSTALLERS="1")
        self.assert_incomplete(result, "cargo generate installers failed")

    def test_regular_local_mode_remains_best_effort(self):
        result = self.run_bump("0.8.6", BUMP_TEST_INSTALLERS="1")
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("warn: cargo generate installers failed", result.stdout)

    def test_unknown_and_extra_arguments_fail_before_mutation(self):
        for args in (("--typo",), ("0.8.6", "0.8.7")):
            result = self.run_bump(*args)
            self.assert_incomplete(result, "error:")
            self.assertFalse(self.calls.exists())

    def test_release_defaults_to_canonical_version(self):
        result = self.run_bump("--release")
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("Done.", result.stdout)
        self.assertEqual((self.root / "docs/book/stable-version.txt").read_text(), "v0.8.5\n")

    def test_tauri_generation_error_is_fatal_in_release_mode(self):
        config = self.root / "apps/tauri/tauri.conf.json"
        config.parent.mkdir(parents=True)
        config.write_text('{"version":"0.8.5"}\n')
        self.stub("jq", "exit 1")
        result = self.run_bump("--release", "0.8.6")
        self.assert_incomplete(result, "Tauri version update failed")
        self.assertEqual(config.read_text(), '{"version":"0.8.5"}\n')
        self.assertFalse(self.calls.exists())


if __name__ == "__main__":
    unittest.main()
