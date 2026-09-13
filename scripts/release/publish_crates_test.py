#!/usr/bin/env python3
"""Exercise the publisher process with local Cargo/registry doubles; never upload."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


RELEASE = Path(__file__).resolve().parent
VERSION = "1.2.3"


def dependency(name, kind=None, req="^1.2.3", **fields):
    return {"name": name, "kind": kind, "req": req, "rename": None,
            "target": None, **fields}


def package(name, dependencies=(), publish=None, version=VERSION):
    return {"name": name, "version": version, "publish": publish,
            "dependencies": list(dependencies)}


DOUBLE = r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys

root = Path(os.environ["PUBLISH_TEST_ROOT"])
tool = Path(sys.argv[0]).name
args = sys.argv[1:]
if tool == "cargo" and args[0] == "metadata":
    sys.stdout.write((root / "metadata.json").read_text())
elif tool == "cargo" and args[0] == "publish":
    with (root / "publishes.jsonl").open("a") as log:
        log.write(json.dumps(args) + "\n")
    if "--dry-run" not in args:
        name = args[args.index("-p") + 1]
        (root / "registry" / name).touch()
elif tool == "curl":
    with (root / "queries.jsonl").open("a") as log:
        log.write(json.dumps(args) + "\n")
    suffix = args[-1].split("/api/v1/crates/")[1].split("/")
    # Existing crate names, but only recorded versions have been uploaded.
    print("200" if len(suffix) == 1 or (root / "registry" / suffix[0]).exists() else "404")
elif tool == "git" and args[0] in ("diff", "ls-files"):
    pass
else:
    sys.exit("unexpected external command: " + tool + " " + repr(args))
'''


class PublishCratesTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="publish-crates-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        script_dir = self.root / "scripts/release"
        script_dir.mkdir(parents=True)
        for filename in ("publish-crates.sh", "publish_order.py"):
            source = RELEASE / filename
            if source.exists():
                shutil.copyfile(source, script_dir / filename)
        (self.root / "Cargo.toml").write_text(
            '[workspace.package]\nversion = "1.2.3"\n'
        )
        (self.root / "web/dist").mkdir(parents=True)
        (self.root / "web/dist/index.html").write_text("fixture")
        (self.root / "registry").mkdir()
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        for tool in ("cargo", "curl", "git"):
            path = bin_dir / tool
            path.write_text(DOUBLE)
            path.chmod(0o755)
        self.env = {**os.environ, "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                    "PUBLISH_TEST_ROOT": str(self.root), "PUBLISH_DELAY_SECONDS": "0"}
        self.env.pop("CARGO_REGISTRY_TOKEN", None)

    def run_publisher(self, packages=None, *, raw=None, execute=False, token=True):
        # Match the real workspace's private and independent members. Bash 3.2
        # cannot expand empty arrays under nounset in the existing summary.
        metadata = raw if raw is not None else json.dumps({"packages": [
            *packages, package("fixture-private", publish=[]),
            package("fixture-independent", version="0.1.0")
        ]})
        (self.root / "metadata.json").write_text(metadata)
        env = self.env.copy()
        if execute and token:
            env["CARGO_REGISTRY_TOKEN"] = "fixture-not-a-credential"
        args = ["bash", str(self.root / "scripts/release/publish-crates.sh")]
        if execute:
            args.append("--execute")
        return subprocess.run(args, env=env, text=True, capture_output=True, timeout=15)

    def calls(self, filename):
        path = self.root / filename
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def uploads(self):
        return [args[args.index("-p") + 1] for args in self.calls("publishes.jsonl")
                if "--dry-run" not in args]

    def assert_preflight_failure(self, result, message):
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(message, result.stderr)
        self.assertEqual(self.calls("publishes.jsonl"), [])
        self.assertEqual(self.calls("queries.jsonl"), [])

    def test_versioned_dev_dependency_uploads_before_consumer(self):
        result = self.run_publisher([
            package("a-runtime", [dependency("z-relay", "dev")]), package("z-relay")
        ], execute=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.uploads(), ["z-relay", "a-runtime"])

    def test_normal_build_target_and_renamed_edges_are_ordered(self):
        result = self.run_publisher([
            package("a-app", [dependency("z-normal"), dependency("z-build", "build"),
                              dependency("z-target", "dev", target="cfg(windows)", rename="alias")]),
            package("z-target"), package("z-build"), package("z-normal")
        ], execute=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.uploads(), ["z-build", "z-normal", "z-target", "a-app"])

    def test_cycle_fails_before_registry_queries_in_both_modes(self):
        packages = [package("a", [dependency("b", "dev")]),
                    package("b", [dependency("a")])]
        for execute in (False, True):
            with self.subTest(execute=execute):
                result = self.run_publisher(packages, execute=execute)
                self.assert_preflight_failure(result, "dependency cycle: a -> b -> a")

    def test_private_versioned_dev_dependency_fails_before_registry_queries(self):
        for version in (VERSION, "0.1.0"):
            with self.subTest(version=version):
                result = self.run_publisher([
                    package("a", [dependency("private", "dev")]),
                    package("private", publish=[], version=version)
                ], execute=True)
                self.assert_preflight_failure(result, "a -> private")

    def test_private_normal_dependency_fails_before_registry_queries(self):
        result = self.run_publisher([
            package("a", [dependency("private")]), package("private", publish=[])
        ], execute=True)
        self.assert_preflight_failure(result, "a -> private")

    def test_malformed_metadata_fails_before_registry_queries(self):
        for metadata in ("{broken", '{"packages": [{}]}',
                         json.dumps({"packages": [package("a", [{"name": "b"}])]}),
                         json.dumps({"packages": [package("a", [dependency("b", "unknown")])]}),
                         json.dumps({"packages": [package("a", [dependency("b", req=123)])]})):
            with self.subTest(metadata=metadata):
                self.assert_preflight_failure(
                    self.run_publisher(raw=metadata, execute=True), "could not compute publish order"
                )

    def test_large_metadata_crosses_real_process_boundary(self):
        large = json.dumps({"packages": [package("a"), package("fixture-private", publish=[]),
                                          package("fixture-independent", version="0.1.0")],
                            "padding": "x" * 262144})
        self.assertGreater(len(large.encode()), 214753)
        result = self.run_publisher(raw=large)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.calls("publishes.jsonl")), 1)
        self.assertIn("--dry-run", self.calls("publishes.jsonl")[0])

    def test_tokenless_dry_run_excludes_private_and_independent_packages(self):
        result = self.run_publisher([package("a"), package("private", publish=[]),
                                     package("independent", version="0.1.0")])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls("publishes.jsonl"), [
            ["publish", "--dry-run", "--locked", "--allow-dirty", "-p", "a"]
        ])

    def test_execute_requires_token(self):
        result = self.run_publisher([package("a")], execute=True, token=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("CARGO_REGISTRY_TOKEN is required", result.stderr)
        self.assertEqual(self.calls("publishes.jsonl"), [])

    def test_execute_resumes_and_skips_existing_dependency(self):
        (self.root / "registry/z-relay").touch()
        result = self.run_publisher([
            package("a-runtime", [dependency("z-relay", "dev")]), package("z-relay")
        ], execute=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.uploads(), ["a-runtime"])
        self.assertIn("skipped 1 already present", result.stdout)
        self.assertEqual(self.calls("publishes.jsonl")[0],
                         ["publish", "-p", "a-runtime", "--locked", "--no-verify", "--allow-dirty"])

    def test_path_only_dev_cycle_is_stripped_but_explicit_version_cycle_is_rejected(self):
        # Both declarations produce req="*" in cargo metadata. Use real Cargo
        # here so a synthetic fixture cannot hide that loss of information.
        cargo = shutil.which("cargo")
        (self.root / "Cargo.toml").write_text(
            '[workspace]\nmembers = ["a", "b"]\nresolver = "2"\n'
            '[workspace.package]\nversion = "1.2.3"\n'
        )
        for name in ("a", "b"):
            (self.root / name / "src").mkdir(parents=True)
            (self.root / name / "src/lib.rs").write_text("")
        (self.root / "b/Cargo.toml").write_text(
            '[package]\nname = "b"\nversion.workspace = true\nedition = "2021"\n'
            '[dependencies]\na = { path = "../a", version = "1.2.3" }\n'
        )
        for explicit, inherited in ((False, False), (True, False), (False, True), (True, True)):
            with self.subTest(explicit=explicit, inherited=inherited):
                (self.root / "Cargo.toml").write_text(
                    '[workspace]\nmembers = ["a", "b"]\nresolver = "2"\n'
                    '[workspace.package]\nversion = "1.2.3"\n'
                    '[workspace.dependencies]\nalias = { package = "b", path = "b"'
                    + (', version = "*"' if explicit else '') + ' }\n'
                )
                (self.root / "a/Cargo.toml").write_text(
                    '[package]\nname = "a"\nversion.workspace = true\nedition = "2021"\n'
                    '[target.\'cfg(windows)\'.dev-dependencies]\n'
                    + ('alias = { workspace = true }\n' if inherited else
                       'alias = { package = "b", path = "../b"'
                       + (', version = "*"' if explicit else '') + ' }\n')
                )
                metadata = subprocess.run(
                    [cargo, "metadata", "--format-version", "1", "--no-deps", "--offline"],
                    cwd=self.root, text=True, capture_output=True, check=True, timeout=15
                )
                packages = json.loads(metadata.stdout)["packages"]
                self.assertEqual(packages[0]["dependencies"][0]["req"], "*")
                for log in ("publishes.jsonl", "queries.jsonl"):
                    (self.root / log).unlink(missing_ok=True)
                result = self.run_publisher(packages)
                if explicit:
                    self.assert_preflight_failure(result, "dependency cycle: a -> b -> a")
                else:
                    self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
