#!/usr/bin/env python3
"""Hermetic stable promotion tests: real files, mocked GitHub transport only."""

import base64
import copy
import importlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest import mock


REPO = Path(__file__).resolve().parents[2]
MASTER_SHA = "a" * 40
TAG_SHA = "b" * 40
TAG = "v0.10.0"


def content(text):
    return {"encoding": "base64", "content": base64.b64encode(text.encode()).decode()}


class WorkflowTest(unittest.TestCase):
    def test_promotion_has_separate_job_without_build_tools(self):
        workflow = (REPO / ".github/workflows/docs-deploy.yml").read_text()
        self.assertIn("default: build", workflow)
        self.assertIn("if: github.event_name != 'workflow_dispatch' || inputs.mode == 'build'", workflow)
        promotion = workflow.split("  promote-stable:\n", 1)[1]
        self.assertIn("inputs.mode == 'promote-stable'", promotion)
        self.assertIn("ref: ${{ github.sha }}", promotion)
        self.assertIn("--check-only", promotion)
        self.assertIn("--force-with-lease=refs/heads/gh-pages:", promotion)
        self.assertIn("group: gh-pages\n  cancel-in-progress: false", workflow)
        for build_tool in ["cargo ", "rust-toolchain", "rust-cache", "apt-get", "mdbook build"]:
            self.assertNotIn(build_tool, promotion)

    def test_build_receipt_records_the_actual_checkout(self):
        workflow = (REPO / ".github/workflows/docs-deploy.yml").read_text()
        self.assertIn('git rev-parse HEAD > "$TAG_SRC/.docs-source-commit"', workflow)


class PromotionTest(unittest.TestCase):
    def setUp(self):
        self.helper = importlib.import_module("promote_stable")
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.pages = Path(self.temp.name)
        for tag in ["master", "v0.9.0", TAG]:
            for locale in ["en", "fr"]:
                self.write(f"{tag}/{locale}/index.html", f"<h1>{tag}/{locale}</h1>")
                self.write(f"{tag}/{locale}/chapter.html", "existing locale payload")
        for name in ["_shared/theme.js", "api/index.html", "sitemap.xml", "robots.txt", "CNAME", ".nojekyll"]:
            self.write(name, f"preserve {name}")
        self.write(f"{TAG}/.docs-source-commit", TAG_SHA + "\n")
        self.write("stable-version.txt", "v0.9.0\n")
        self.write("index.html", "old redirect")
        self.metadata = {
            "stable": "v0.9.0",
            "versions": [
                {"tag": "master", "label": "Development (master)"},
                {"tag": TAG, "label": TAG},
                {"tag": "v0.9.0", "label": "Stable (latest release)"},
            ],
        }
        self.write_metadata()
        self.responses = {
            "branches/master": {"protected": True, "commit": {"sha": MASTER_SHA}},
            f"contents/docs/book/stable-version.txt?ref={MASTER_SHA}": content(TAG + "\n"),
            "releases/latest": {"tag_name": TAG, "draft": False, "prerelease": False, "published_at": "2026-09-12T00:00:00Z"},
            f"git/ref/tags/{TAG}": {"object": {"type": "commit", "sha": TAG_SHA}},
            f"contents/locales.toml?ref={TAG_SHA}": content('[[locale]]\ncode = "en"\n[[locale]]\ncode = "fr"\n'),
        }
        self.api = mock.patch.object(self.helper, "gh_json", side_effect=self.github).start()
        self.addCleanup(mock.patch.stopall)

    def write(self, name, text):
        target = self.pages / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def write_metadata(self):
        self.write("versions.json", json.dumps(self.metadata))

    def github(self, repo, endpoint):
        self.assertEqual(repo, "zeroclaw-labs/zeroclaw")
        return copy.deepcopy(self.responses[endpoint])

    def snapshot(self):
        return {str(p.relative_to(self.pages)): p.read_bytes() for p in self.pages.rglob("*") if p.is_file()}

    def promote(self, **overrides):
        args = dict(pages=self.pages, repo="zeroclaw-labs/zeroclaw", master_sha=MASTER_SHA,
                    tag=TAG, workflow_ref="refs/heads/master", minimum="v0.7.5", check_only=False)
        args.update(overrides)
        self.helper.promote(**args)

    def rejected(self, error, **overrides):
        before = self.snapshot()
        with self.assertRaisesRegex((ValueError, OSError), error):
            self.promote(**overrides)
        self.assertEqual(self.snapshot(), before, "rejected promotion changed site files")

    def test_promotes_ten_over_nine_and_preserves_payload_and_chrome(self):
        before = self.snapshot()
        self.promote()
        after = self.snapshot()
        changed = {name for name in before.keys() | after.keys() if before.get(name) != after.get(name)}
        self.assertEqual(changed, {"stable-version.txt", "versions.json", "index.html"})
        self.assertEqual(after["stable-version.txt"], b"v0.10.0\n")
        self.assertEqual(after["index.html"].decode(),
                         '<!doctype html>\n<meta charset="utf-8">\n'
                         '<meta http-equiv="refresh" content="0; url=./v0.10.0/en/">\n'
                         '<link rel="canonical" href="./v0.10.0/en/">\n<title>ZeroClaw Docs</title>\n')
        metadata = json.loads(after["versions.json"])
        self.assertEqual(metadata["stable"], TAG)
        self.assertEqual(metadata["versions"], [
            {"tag": "master", "label": "Development (master)"},
            {"tag": TAG, "label": "Stable (latest release)"},
            {"tag": "v0.9.0", "label": "v0.9.0"},
        ])
        self.promote()  # Retry is idempotent.
        self.assertEqual(self.snapshot(), after)

    def test_check_only_validates_without_writes(self):
        before = self.snapshot()
        self.promote(check_only=True)
        self.assertEqual(self.snapshot(), before)

    def test_final_tags_only(self):
        for tag in ["v0.10.0-rc.1", "master", "v01.2.3", "v1.2.3+build", "../../bad"]:
            with self.subTest(tag=tag):
                self.rejected("final vX.Y.Z", tag=tag)

    def test_workflow_must_run_on_master(self):
        self.rejected("run on master", workflow_ref="refs/tags/v0.10.0")

    def test_below_minimum_rejected(self):
        self.rejected("minimum", minimum="v0.11.0")

    def test_unprotected_master_rejected(self):
        self.responses["branches/master"]["protected"] = False
        self.rejected("protected master")

    def test_stale_master_run_rejected(self):
        self.responses["branches/master"]["commit"]["sha"] = "c" * 40
        self.rejected("master advanced")

    def test_committed_pointer_mismatch_rejected(self):
        self.responses[f"contents/docs/book/stable-version.txt?ref={MASTER_SHA}"] = content("v0.9.0\n")
        self.rejected("committed pointer")

    def test_latest_release_mismatch_rejected(self):
        self.responses["releases/latest"]["tag_name"] = "v0.11.0"
        self.rejected("GitHub Latest")

    def test_nonpublic_or_prerelease_latest_rejected(self):
        for field, value in [("draft", True), ("prerelease", True), ("published_at", None), ("draft", None)]:
            with self.subTest(field=field, value=value):
                original = self.responses["releases/latest"][field]
                self.responses["releases/latest"][field] = value
                self.rejected("public final release")
                self.responses["releases/latest"][field] = original

    def test_annotated_tag_resolves_to_deployed_commit(self):
        self.responses[f"git/ref/tags/{TAG}"]["object"] = {"type": "tag", "sha": "d" * 40}
        self.responses[f"git/tags/{'d' * 40}"] = {"object": {"type": "commit", "sha": TAG_SHA}}
        self.promote()

    def test_invalid_tag_object_rejected(self):
        for obj in [{"type": "tree", "sha": TAG_SHA}, {"type": "commit", "sha": "invalid"}]:
            self.responses[f"git/ref/tags/{TAG}"]["object"] = obj
            self.write(f"{TAG}/.docs-source-commit", obj["sha"])
            self.responses[f"contents/locales.toml?ref={obj['sha']}"] = content('[[locale]]\ncode = "en"')
            self.rejected("tag commit")

    def test_recursive_annotated_tag_rejected(self):
        obj = {"type": "tag", "sha": "d" * 40}
        self.responses[f"git/ref/tags/{TAG}"]["object"] = obj
        self.responses[f"git/tags/{'d' * 40}"] = {"object": obj}
        self.rejected("tag commit")

    def test_missing_deployed_source_receipt_rejected(self):
        (self.pages / TAG / ".docs-source-commit").unlink()
        self.rejected("regular file")

    def test_wrong_deployed_source_rejected(self):
        self.write(f"{TAG}/.docs-source-commit", "e" * 40)
        self.rejected("deployed source")

    def test_missing_target_rejected(self):
        (self.pages / TAG).rename(self.pages / "unpublished")
        self.rejected("regular file")

    def test_symlinked_target_rejected(self):
        (self.pages / TAG).rename(self.pages / "elsewhere")
        (self.pages / TAG).symlink_to(self.pages / "elsewhere")
        self.rejected("symlink")

    def test_missing_locale_index_rejected(self):
        (self.pages / TAG / "fr/index.html").unlink()
        self.rejected("regular file")

    def test_empty_locale_index_rejected(self):
        self.write(f"{TAG}/fr/index.html", "")
        self.rejected("empty landing page")

    def test_invalid_locale_registry_rejected(self):
        for data in ['locale = []', '[[locale]]\ncode = "../en"', '[[locale]]\ncode = "fr"', '[[locale]]\ncode = "en"\n[[locale]]\ncode = "en"']:
            self.responses[f"contents/locales.toml?ref={TAG_SHA}"] = content(data)
            self.rejected("locale registry")

    def test_nine_cannot_replace_ten(self):
        older = "v0.9.0"
        self.write("stable-version.txt", TAG + "\n")
        self.metadata["stable"] = TAG
        self.write_metadata()
        self.responses[f"contents/docs/book/stable-version.txt?ref={MASTER_SHA}"] = content(older)
        self.responses["releases/latest"]["tag_name"] = older
        self.responses[f"git/ref/tags/{older}"] = {"object": {"type": "commit", "sha": TAG_SHA}}
        self.write(f"{older}/.docs-source-commit", TAG_SHA)
        self.rejected("downgrade.*rollback", tag=older)

    def test_patch_and_major_ordering(self):
        self.assertGreater(self.helper.final_version("v1.0.0"), self.helper.final_version("v0.99.99"))
        self.assertGreater(self.helper.final_version("v0.9.10"), self.helper.final_version("v0.9.9"))

    def test_inconsistent_live_metadata_rejected(self):
        self.metadata["stable"] = "v0.8.0"
        self.write_metadata()
        self.rejected("live pointer")

    def test_target_missing_from_metadata_rejected(self):
        self.metadata["versions"] = [v for v in self.metadata["versions"] if v["tag"] != TAG]
        self.write_metadata()
        self.rejected("version entries")

    def test_current_missing_from_metadata_rejected(self):
        self.metadata["versions"] = [v for v in self.metadata["versions"] if v["tag"] != "v0.9.0"]
        self.write_metadata()
        self.rejected("version entries")

    def test_duplicate_version_entries_rejected(self):
        self.metadata["versions"].append(self.metadata["versions"][1])
        self.write_metadata()
        self.rejected("version entries")

    def test_symlinked_metadata_rejected(self):
        (self.pages / "index.html").unlink()
        (self.pages / "index.html").symlink_to(self.pages / "api/index.html")
        self.rejected("regular file")

    def test_symlinked_locale_rejected(self):
        self.pages.joinpath(TAG, "fr").rename(self.pages / "locale-backup")
        self.pages.joinpath(TAG, "fr").symlink_to(self.pages / "locale-backup")
        self.rejected("symlink")

    def test_github_error_prevents_any_write(self):
        self.api.side_effect = OSError("GitHub unavailable")
        self.rejected("GitHub unavailable")

    def test_check_only_rechecks_latest_before_publication(self):
        self.promote()
        self.responses["releases/latest"]["tag_name"] = "v0.11.0"
        self.rejected("GitHub Latest", check_only=True)

    def test_unknown_content_encoding_rejected(self):
        self.responses[f"contents/docs/book/stable-version.txt?ref={MASTER_SHA}"]["encoding"] = "none"
        self.rejected("base64 encoded")

    def test_malformed_base64_rejected(self):
        self.responses[f"contents/docs/book/stable-version.txt?ref={MASTER_SHA}"]["content"] = "invalid?"
        self.rejected("base64")

    def test_malformed_metadata_prevents_any_write(self):
        self.write("versions.json", "not json")
        self.rejected("Expecting value")

    def test_actual_workflow_publishes_only_metadata_to_local_git_remote(self):
        # Run the workflow's actual shell and CLI against a disposable local
        # remote. Only gh's HTTP boundary is replaced; no Pages deployment.
        with tempfile.TemporaryDirectory() as workspace:
            root = Path(workspace)
            remote = root / "remote.git"
            source = root / "source"
            source.mkdir()
            git_env = {**os.environ, "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull}

            def git(cwd, *args):
                return subprocess.run(["git", *args], cwd=cwd, check=True,
                                      env=git_env, capture_output=True, text=True).stdout.strip()

            git(root, "init", "--bare", str(remote))
            for checkout in [self.pages, source]:
                git(checkout, "init", "-b", "master")
                git(checkout, "config", "user.name", "Docs Test")
                git(checkout, "config", "user.email", "docs-test@example.invalid")
                git(checkout, "remote", "add", "origin", str(remote))
            git(self.pages, "add", "--all")
            git(self.pages, "commit", "-m", "existing site")
            before_sha = git(self.pages, "rev-parse", "HEAD")
            git(self.pages, "push", "origin", "HEAD:refs/heads/gh-pages")
            helper_path = source / "scripts/docs/promote_stable.py"
            helper_path.parent.mkdir(parents=True)
            shutil.copyfile(REPO / "scripts/docs/promote_stable.py", helper_path)
            git(source, "add", "--all")
            git(source, "commit", "-m", "workflow source")
            master_sha = git(source, "rev-parse", "HEAD")
            self.responses["branches/master"]["commit"]["sha"] = master_sha
            self.responses[f"contents/docs/book/stable-version.txt?ref={master_sha}"] = content(TAG)
            fixtures = root / "api.json"
            fixtures.write_text(json.dumps(self.responses))
            executable_dir = root / "bin"
            executable_dir.mkdir()
            gh = executable_dir / "gh"
            gh.write_text('#!/usr/bin/env python3\nimport json, os, sys\n'
                          'with open(os.environ["DOCS_TEST_API"]) as f: data = json.load(f)\n'
                          'endpoint = sys.argv[-1].removeprefix("repos/zeroclaw-labs/zeroclaw/")\n'
                          'print(json.dumps(data[endpoint]))\n')
            gh.chmod(0o755)
            workflow = (REPO / ".github/workflows/docs-deploy.yml").read_text()
            body = workflow.split("      - name: Promote deployed stable docs\n", 1)[1].split("        run: |\n", 1)[1]
            script = "\n".join(line[10:] for line in body.splitlines())
            env = {**git_env, "PATH": str(executable_dir) + os.pathsep + os.environ["PATH"],
                   "DOCS_TEST_API": str(fixtures), "TMPDIR": str(root),
                   "GITHUB_REPOSITORY": "zeroclaw-labs/zeroclaw", "GITHUB_SHA": master_sha,
                   "GITHUB_REF": "refs/heads/master", "TAG": TAG, "DOCS_MIN_VERSION": "v0.7.5"}
            result = subprocess.run(["bash", "-c", script], cwd=source, env=env,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(git(remote, "rev-list", "--count", "gh-pages"), "1")
            changed = git(remote, "diff", "--name-only", before_sha, "gh-pages").splitlines()
            self.assertEqual(changed, ["index.html", "stable-version.txt", "versions.json"])
            self.assertEqual(git(remote, "show", "gh-pages:stable-version.txt"), TAG)
            self.assertEqual(git(source, "rev-parse", "HEAD"), master_sha)


class TransportTest(unittest.TestCase):
    def test_gh_failure_is_not_treated_as_missing_release(self):
        helper = importlib.import_module("promote_stable")
        with mock.patch.object(helper.subprocess, "run", side_effect=subprocess.CalledProcessError(1, ["gh"])):
            with self.assertRaises(subprocess.CalledProcessError):
                helper.gh_json("zeroclaw-labs/zeroclaw", "releases/latest")

    def test_cli_errors_return_failure(self):
        helper_path = REPO / "scripts/docs/promote_stable.py"
        result = subprocess.run(["python3", str(helper_path), "--pages", "/nonexistent",
                                 "--repo", "zeroclaw-labs/zeroclaw", "--master-sha", MASTER_SHA,
                                 "--tag", "v0.10.0-rc.1"], capture_output=True, text=True,
                                env={**os.environ, "GITHUB_REF": "refs/heads/master"})
        self.assertEqual(result.returncode, 1)
        self.assertIn("final vX.Y.Z", result.stderr)


if __name__ == "__main__":
    unittest.main()
