"""Check build preflight without compiling dependencies."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib
import unittest


class BuildPreflightTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        source = Path(__file__).with_name("build.sh")
        shutil.copy(source, self.root / "build.sh")
        shutil.copy(source.with_name("Cargo.lock"), self.root / "Cargo.lock")
        self.tools = self.root / "tools"
        self.tools.mkdir()
        for name in ("bash", "dirname", "grep", "sed", "head", "touch"):
            self.tools.joinpath(name).symlink_to(shutil.which(name))
        self.environment = dict(os.environ, PATH=str(self.tools))
        self.marker = self.root / "cargo-called"
        self.tool("rustup", "echo wasm32-unknown-unknown")
        self.tool("cargo", f"touch '{self.marker}'; exit 99")
        lock = tomllib.loads((self.root / "Cargo.lock").read_text())
        self.version = next(package["version"] for package in lock["package"]
                            if package["name"] == "wasm-bindgen")

    def tool(self, name, body):
        tool = self.tools / name
        tool.write_text(f"#!/bin/sh\n{body}\n")
        tool.chmod(0o755)

    def run_build(self):
        return subprocess.run(
            [str(self.tools / "bash"), str(self.root / "build.sh")],
            env=self.environment, capture_output=True, text=True, check=False,
        )

    def test_missing_cli_stops_before_cargo(self):
        result = self.run_build()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertFalse(self.marker.exists())
        self.assertIn("does not match Cargo.lock", result.stderr)

    def test_mismatched_cli_stops_before_cargo(self):
        for version in ("", f"wasm-bindgen {self.version}0", "wasm-bindgen invalid"):
            with self.subTest(version=version):
                self.tool("wasm-bindgen", f"printf '%s\\n' '{version}'")
                result = self.run_build()
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertFalse(self.marker.exists())
                self.assertIn("does not match Cargo.lock", result.stderr)

    def test_matching_cli_reaches_locked_wasm_build(self):
        self.tool("wasm-bindgen", f"echo wasm-bindgen {self.version}")
        self.tool("cargo", f"printf '%s\\n' \"$@\" > '{self.marker}'; exit 99")
        result = self.run_build()
        self.assertEqual(result.returncode, 99, result.stderr)
        self.assertEqual(self.marker.read_text().splitlines(),
                         ["build", "--locked", "--release", "--target", "wasm32-unknown-unknown"])

    def test_missing_wasm_target_stops_before_cargo(self):
        self.tool("rustup", "echo x86_64-unknown-linux-gnu")
        self.tool("wasm-bindgen", f"echo wasm-bindgen {self.version}")
        result = self.run_build()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertFalse(self.marker.exists())
        self.assertIn("MISSING TARGET", result.stderr)

    def test_script_has_valid_bash_syntax(self):
        subprocess.run(
            [str(self.tools / "bash"), "-n", str(self.root / "build.sh")], check=True
        )


if __name__ == "__main__":
    unittest.main()
