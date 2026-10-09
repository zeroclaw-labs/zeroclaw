"""Cheap selector and fixture contracts; does not require built applications."""

import importlib.util
import json
import os
import re
import tempfile
import textwrap
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

from fixtures import ModelFixture
from support import Installation
import workflow_policy

spec = importlib.util.spec_from_file_location("acceptance_selection", Path(__file__).with_name("scope.py"))
selection = importlib.util.module_from_spec(spec)
spec.loader.exec_module(selection)


class SelectionTests(unittest.TestCase):
    def test_documentation_only(self):
        self.assertEqual(selection.classify(["docs/book/src/agents/overview.md", "README.md"], [], "pull_request")[0], "skip")

    def test_ordinary_code_and_mixed_docs(self):
        for paths in (["crates/zeroclaw-providers/src/openai.rs"], ["README.md", "tests/system/full_stack.rs"]):
            self.assertEqual(selection.classify(paths, [], "pull_request")[0], "core")

    def test_sensitive_paths(self):
        for path in ("src/main.rs", "Cargo.lock", "crates/zeroclaw-config/Cargo.toml", "apps/zerocode/src/client.rs",
                     ".github/workflows/ci.yml", "tests/system/runtime_acceptance/fixtures.py",
                     "crates/zeroclaw-runtime/src/rpc/auth.rs", "crates/zeroclaw-gateway/src/lib.rs"):
            with self.subTest(path=path):
                self.assertEqual(selection.classify([path], [], "pull_request")[0], "full")

    def test_labels_also_escalate_docs(self):
        for label in selection.HIGH_RISK:
            self.assertEqual(selection.classify(["README.md"], [label], "pull_request")[0], "full")

    def test_unknown_and_malformed_inputs(self):
        for paths in (None, [], [""], [None], ["../README.md"], ["new-package/code.rs"], ["crates/new-package/src/lib.rs"]):
            self.assertEqual(selection.classify(paths, [], "pull_request")[0], "full")
        self.assertEqual(selection.classify(["README.md"], None, "pull_request")[0], "full")
        self.assertEqual(selection.from_event("pull_request", {})[0], "full")

    def test_non_pr_runs(self):
        for event in ("merge_group", "push", "workflow_dispatch"):
            self.assertEqual(selection.classify(["README.md"], [], event)[0], "full")

    def test_fixture_markdown_is_not_documentation(self):
        self.assertEqual(selection.classify(["tests/fixtures/SOP.md"], [], "pull_request")[0], "core")

    def test_event_labels_and_diff(self):
        payload = {"pull_request": {"base": {"sha": "a" * 40}, "labels": []}}
        with patch.object(selection.subprocess, "check_output", return_value=b"README.md\0") as diff:
            self.assertEqual(selection.from_event("pull_request", payload)[0], "skip")
            self.assertIn("--no-renames", diff.call_args[0][0])
            payload["pull_request"]["labels"] = [{"name": "priority:p1"}]
            self.assertEqual(selection.from_event("pull_request", payload)[0], "full")
        with patch.object(selection.subprocess, "check_output", side_effect=subprocess.CalledProcessError(1, "git")):
            self.assertEqual(selection.from_event("pull_request", payload)[0], "full")
        for malformed in (None, [], {"pull_request": {}}, {"pull_request": {"base": {"sha": "bad"}}}):
            self.assertEqual(selection.from_event("pull_request", malformed)[0], "full")

    def test_renamed_sensitive_source_still_escalates(self):
        self.assertEqual(selection.classify(["crates/zeroclaw-runtime/src/rpc/old.rs", "tests/new.rs"], [], "pull_request")[0], "full")


class WorkflowPolicyTests(unittest.TestCase):
    def test_only_relevant_labels_select_quality(self):
        for action in ("labeled", "unlabeled"):
            for label in selection.HIGH_RISK:
                self.assertTrue(workflow_policy.selected("pull_request", action, label))
            for label in ("ci", "docs", "size:XL", "status:ready", "risk:low"):
                self.assertFalse(workflow_policy.selected("pull_request", action, label))
            self.assertTrue(workflow_policy.selected("pull_request", action, None))
        for event in ("push", "merge_group", "workflow_dispatch"):
            self.assertTrue(workflow_policy.selected(event))
        for action in ("opened", "synchronize", "reopened", "ready_for_review"):
            self.assertTrue(workflow_policy.selected("pull_request", action, "docs"))
        self.assertFalse(workflow_policy.selected("workflow_dispatch", acceptance_cost=True))

    def test_materialized_filters_cover_every_job(self):
        workflow = (selection.ROOT / ".github/workflows/ci.yml").read_text()
        self.assertEqual(workflow_policy.materialize(workflow), workflow)
        jobs = workflow.split("\njobs:\n", 1)[1]
        blocks = re.split(r"(?m)(?=^  [a-z][a-z0-9-]*:\n)", jobs)
        checked = 0
        for block in blocks:
            name = re.match(r"  ([a-z][a-z0-9-]*):", block)
            if not name or name[1] == "acceptance-cost":
                continue
            condition = re.search(r"(?m)^    if: (.+)$", block)
            self.assertIsNotNone(condition, name[1])
            if name[1] == "crates-preflight":
                self.assertEqual(condition[1], "needs.crates-preflight-changes.outputs.run == 'true'")
                self.assertIn("    needs: [crates-preflight-changes]", block)
                continue
            if name[1] == "master-debounce":
                self.assertEqual(condition[1], "github.event_name == 'push'")
                continue
            self.assertIn(workflow_policy.expression(), condition[1], name[1])
            checked += 1
        self.assertGreater(checked, 30)
        self.assertIn("&& 'CI Required Gate' || 'Quality Gate not requested'", workflow)
        self.assertIn("&& 'quality' || github.run_id", workflow)
        # Removing a job guard or changing the canonical labels must be detected.
        damaged = workflow.replace("    if: ${{ " + workflow_policy.expression() + " }}\n", "", 1)
        self.assertNotEqual(workflow_policy.materialize(damaged), damaged)
        with patch.object(workflow_policy, "HIGH_RISK", selection.HIGH_RISK | {"risk:new"}):
            self.assertNotEqual(workflow_policy.materialize(workflow), workflow)

    def test_actual_build_script_selects_packages(self):
        workflow = (selection.ROOT / ".github/workflows/ci.yml").read_text()
        block = workflow.split("      - name: ${{ matrix.label }}\n", 1)[1]
        script = textwrap.dedent(block.split("        run: |\n", 1)[1].split("        env:\n", 1)[0])
        for token, value in (("matrix.label", "Build"), ("matrix.target", "x86_64-unknown-linux-gnu"),
                             ("matrix.cmd", "build"), ("runner.os", "Linux")):
            script = script.replace("${{ " + token + " }}", value)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cargo = root / "cargo"
            cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$CAPTURE_ARGS"\nexit "${CARGO_RESULT:-0}"\n')
            cargo.chmod(0o755)
            env = {**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"],
                   "RUNNER_TEMP": str(root), "CAPTURE_ARGS": str(root / "args"),
                   "GITHUB_STEP_SUMMARY": str(root / "summary")}
            for suite in ("skip", "core", "full", ""):
                subprocess.run(["bash", "-e", "-o", "pipefail", "-c", script], env={**env, "ACCEPTANCE_SUITE": suite}, check=True)
                arguments = (root / "args").read_text().splitlines()
                self.assertIn("--locked", arguments)
                self.assertEqual("zerocode" in arguments, suite != "skip")
            result = subprocess.run(["bash", "-e", "-o", "pipefail", "-c", script],
                                    env={**env, "ACCEPTANCE_SUITE": "full", "CARGO_RESULT": "19"})
            self.assertEqual(result.returncode, 19)


class FixtureTests(unittest.TestCase):
    def test_redaction_includes_displayed_pairing_codes(self):
        app = object.__new__(Installation)
        app.secrets = {"issued-token"}
        app.root = Path("/tmp/test-installation")
        raw = "│  random-pair-code  │\nX-Pairing-Code: random-pair-code\nissued-token"
        result = app.redact(raw)
        self.assertNotIn("random-pair-code", result)
        self.assertNotIn("issued-token", result)
        self.assertNotIn("hidden-key", app.redact("-----BEGIN PRIVATE KEY-----\nhidden-key\n-----END PRIVATE KEY-----"))
        self.assertNotIn("hidden-bearer", app.redact("Authorization: Bearer hidden-bearer"))

    def test_unexpected_request_cannot_pass(self):
        model = ModelFixture()
        try:
            model.enqueue("expected", text="reply")
            with self.assertRaises(AssertionError):
                model.respond("POST", "/chat/completions", json.dumps({"messages": [], "model": "acceptance-model"}).encode())
        finally:
            model.close()

    def test_unconsumed_script_cannot_pass(self):
        model = ModelFixture()
        try:
            model.enqueue("expected", text="reply")
            with self.assertRaises(AssertionError):
                model.verify()
        finally:
            model.close()


if __name__ == "__main__":
    unittest.main()
