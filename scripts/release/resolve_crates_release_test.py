#!/usr/bin/env python3
"""Exercise crates.io release resolution against real git repositories."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parent / "resolve_crates_release.sh"
DIGEST = "a" * 64


class ResolveCratesReleaseTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="resolve-crates-release-")
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.git("init", "-q", "-b", "main")
        self.first = self.commit("1.2.3")
        self.head = self.commit("1.2.3")

    def git(self, *args):
        env = {**os.environ, "GIT_AUTHOR_NAME": "fixture", "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
               "GIT_COMMITTER_NAME": "fixture", "GIT_COMMITTER_EMAIL": "fixture@example.invalid"}
        return subprocess.run(["git", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false", *args],
                              cwd=self.repo, env=env, text=True, capture_output=True,
                              check=True).stdout.strip()

    def commit(self, version):
        cargo = self.repo / "Cargo.toml"
        count = len(cargo.read_text().splitlines()) if cargo.exists() else 0
        cargo.write_text(f'[workspace.package]\nversion = "{version}"\nrust-version = "1.98"\n'
                         + "# pad\n" * count)
        self.git("add", "Cargo.toml")
        self.git("commit", "-q", "-m", f"commit {count}")
        return self.git("rev-parse", "HEAD")

    def resolve(self, stage=None, sha=None, digest=None, tag="v1.2.3", tooling=None):
        env = {k: v for k, v in os.environ.items()
               if k not in ("STAGE", "RELEASE_SHA", "VERIFIED_WEB_DIST_DIGEST", "TOOLING_SHA")}
        if tooling is not None:
            env["TOOLING_SHA"] = tooling
        env["RELEASE_TAG"] = tag
        if stage is not None:
            env["STAGE"] = stage
        if sha is not None:
            env["RELEASE_SHA"] = sha
        if digest is not None:
            env["VERIFIED_WEB_DIST_DIGEST"] = digest
        return subprocess.run(["bash", str(SCRIPT)], cwd=self.repo, env=env, text=True,
                              capture_output=True, timeout=15)

    def outputs(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        return dict(line.split("=", 1) for line in result.stdout.splitlines())

    def assert_fails(self, result, message):
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(message, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_preflight_verifies_release_commit_before_the_tag_exists(self):
        out = self.outputs(self.resolve("preflight", self.head))
        self.assertEqual(out, {"stage": "preflight", "version": "1.2.3",
                               "sha": self.head, "tooling_sha": self.head, "msrv": "1.98"})

    def test_preflight_rejects_a_checkout_other_than_the_release_commit(self):
        self.assert_fails(self.resolve("preflight", self.first),
                          "resolves to " + self.head)

    def test_preflight_requires_the_release_commit(self):
        self.assert_fails(self.resolve("preflight"), "stage preflight requires release_sha")

    def test_preflight_rejects_an_existing_tag_at_another_commit(self):
        self.git("tag", "v1.2.3", self.first)
        self.assert_fails(self.resolve("preflight", self.head), "resolves to " + self.first)

    def test_preflight_accepts_an_existing_tag_at_the_release_commit(self):
        # A tag-push release already has its tag during the early preflight.
        self.git("tag", "-a", "v1.2.3", "-m", "release")
        self.assertEqual(self.outputs(self.resolve("preflight", self.head))["sha"], self.head)

    def test_publish_requires_the_tag_the_github_release_created(self):
        self.assert_fails(self.resolve("publish", self.head, DIGEST),
                          "v1.2.3 is not a tag in this repository")

    def test_publish_rejects_a_tag_created_at_another_commit(self):
        self.git("tag", "v1.2.3", self.first)
        self.assert_fails(self.resolve("publish", self.head, DIGEST), "resolves to " + self.first)

    def test_publish_uploads_only_with_the_preflight_digest(self):
        self.git("tag", "-a", "v1.2.3", "-m", "release")
        self.assertEqual(self.outputs(self.resolve("publish", self.head, DIGEST))["stage"], "publish")
        for digest in (None, "", "A" * 64, "a" * 63):
            with self.subTest(digest=digest):
                self.assert_fails(self.resolve("publish", self.head, digest),
                                  "requires the web/dist digest")

    def test_digest_is_rejected_outside_the_publish_stage(self):
        self.git("tag", "v1.2.3")
        for stage in (None, "all", "preflight"):
            with self.subTest(stage=stage):
                self.assert_fails(self.resolve(stage, self.head, DIGEST),
                                  "only valid with stage publish")

    def test_standalone_run_needs_only_the_tag(self):
        self.git("tag", "v1.2.3")
        out = self.outputs(self.resolve())
        self.assertEqual((out["stage"], out["sha"]), ("all", self.head))

    def test_standalone_run_rejects_a_branch_named_like_the_tag(self):
        self.git("branch", "v1.2.3")
        self.assert_fails(self.resolve(), "v1.2.3 is not a tag in this repository")

    def test_standalone_run_rejects_a_checkout_that_is_not_the_tag(self):
        self.git("tag", "v1.2.3", self.first)
        self.assert_fails(self.resolve(), "The checkout is " + self.head)

    def test_version_and_stage_are_validated(self):
        self.assert_fails(self.resolve("preflight", self.head, tag="v1.2.4"),
                          "does not match workspace version 1.2.3")
        self.assert_fails(self.resolve("preflight", self.head, tag="1.2.3"),
                          "release_tag must be vX.Y.Z")
        self.assert_fails(self.resolve("publish-now", self.head), "stage must be all, preflight, or publish")


    def released_then_fixed_on_master(self):
        """Tag the release, land a later fix on master, and check the tag out."""
        self.git("tag", "-a", "v1.2.3", "-m", "release")
        fix = self.commit("1.2.3")
        self.git("update-ref", "refs/remotes/origin/master", fix)
        self.git("checkout", "-q", "--detach", "v1.2.3")
        return fix

    def test_recovery_packages_the_tag_with_fixed_master_tooling(self):
        fix = self.released_then_fixed_on_master()
        out = self.outputs(self.resolve(tooling=fix))
        self.assertEqual((out["stage"], out["sha"], out["tooling_sha"]), ("all", self.head, fix))

    def test_recovery_tooling_must_be_on_master(self):
        self.released_then_fixed_on_master()
        self.git("checkout", "-q", "-b", "unreviewed")
        unreviewed = self.commit("1.2.3")
        self.git("checkout", "-q", "--detach", "v1.2.3")
        self.assert_fails(self.resolve(tooling=unreviewed), "is not on master")

    def test_recovery_tooling_must_contain_the_release(self):
        # Older master tooling would reintroduce bugs the release already fixed.
        self.released_then_fixed_on_master()
        self.assert_fails(self.resolve(tooling=self.first), "does not contain release commit")

    def test_recovery_tooling_must_exist(self):
        self.released_then_fixed_on_master()
        self.assert_fails(self.resolve(tooling="f" * 40), "is not available in this checkout")

    def test_release_runs_always_use_their_own_tooling(self):
        fix = self.released_then_fixed_on_master()
        self.assert_fails(self.resolve("preflight", self.head, tooling=fix),
                          "stage preflight must use the release commit's own tooling")
        self.assert_fails(self.resolve("publish", self.head, DIGEST, tooling=fix),
                          "stage publish must use the release commit's own tooling")


if __name__ == "__main__":
    unittest.main()
