#!/usr/bin/env python3
"""Hermetic checks for the early Apple release authentication boundary."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/release/apple_preflight.py"
SIGNING = ("APPLE_CERTIFICATE", "APPLE_CERTIFICATE_PASSWORD", "APPLE_SIGNING_IDENTITY")
NOTARY = ("APPLE_ID", "APPLE_PASSWORD", "APPLE_TEAM_ID")


def desktop_steps():
    workflow = (ROOT / ".github/workflows/release-stable-manual.yml").read_text()
    job = re.search(
        r"^  build-desktop:\n.*?(?=^  [a-zA-Z0-9_-]+:|\Z)",
        workflow,
        re.MULTILINE | re.DOTALL,
    ).group(0)
    return re.findall(r"^      - .*?(?=^      - |\Z)", job, re.MULTILINE | re.DOTALL)


class WorkflowTest(unittest.TestCase):
    def test_authentication_precedes_build_and_secret_export(self):
        steps = desktop_steps()
        preflight = [i for i, step in enumerate(steps) if "apple_preflight.py" in step]
        self.assertEqual(len(preflight), 1, "missing early Apple credential validation")
        index = preflight[0]
        self.assertLess(index, next(i for i, s in enumerate(steps) if "prepare-kernel.sh" in s))
        self.assertLess(index, next(i for i, s in enumerate(steps) if "GITHUB_ENV" in s))
        step = steps[index]
        self.assertIn("timeout-minutes: 5", step)
        self.assertNotIn("continue-on-error:", step)
        self.assertNotRegex(step, r"(?m)^        if:")
        for name in SIGNING + NOTARY:
            self.assertIn(name + ": ${{ secrets." + name + " }}", step)
        self.assertRegex(step, r"(?m)^        run: python3 scripts/release/apple_preflight.py$")


class PreflightTest(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location("apple_preflight", SCRIPT)
        self.helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.helper)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.calls = self.root / "calls"
        self.export = self.root / "github-env"
        self.export.write_text("EXISTING=value\n")
        # Runtime-generated opaque values: no actual credentials or identities.
        self.credentials = {key: secrets.token_hex(24) for key in SIGNING + NOTARY}
        self.env = {
            "PATH": str(self.bin),
            "GITHUB_ENV": str(self.export),
            "PREFLIGHT_TEST_CALLS": str(self.calls),
            "PREFLIGHT_TEST_RESPONSES": "[0]",
        }
        stub = self.bin / "xcrun"
        stub.write_text(
            f"#!{sys.executable}\n"
            "import json, os, pathlib, sys\n"
            "assert sys.argv[1:3] == ['notarytool', 'history']\n"
            "args = dict(zip(sys.argv[3::2], sys.argv[4::2]))\n"
            "assert args == {'--apple-id': os.environ['APPLE_ID'], "
            "'--password': os.environ['APPLE_PASSWORD'], "
            "'--team-id': os.environ['APPLE_TEAM_ID']}\n"
            "path = pathlib.Path(os.environ['PREFLIGHT_TEST_CALLS'])\n"
            "calls = path.read_text().splitlines() if path.exists() else []\n"
            "with path.open('a') as output: output.write('history\\n')\n"
            "statuses = json.loads(os.environ['PREFLIGHT_TEST_RESPONSES'])\n"
            "status = statuses[min(len(calls), len(statuses) - 1)]\n"
            "for key, value in os.environ.items():\n"
            "    if key.startswith('APPLE_'): print(value, file=sys.stderr)\n"
            "if status: print('HTTP status code: ' + str(status), file=sys.stderr)\n"
            "sys.exit(bool(status))\n"
        )
        stub.chmod(0o700)

    def run_preflight(self, credentials=None, responses=(0,)):
        values = self.credentials if credentials is None else credentials
        env = self.env | values | {"PREFLIGHT_TEST_RESPONSES": json.dumps(responses)}
        output = io.StringIO()
        # Clear inherited credentials and all other Apple/Tauri configuration.
        with mock.patch.dict(os.environ, env, clear=True), contextlib.redirect_stdout(output), \
                contextlib.redirect_stderr(output), mock.patch.object(self.helper.time, "sleep") as sleep:
            code = self.helper.main()
        log = output.getvalue()
        for value in values.values():
            if value.strip():
                self.assertNotIn(value, log)
        self.assertEqual(self.export.read_text(), "EXISTING=value\n")
        self.assertEqual(set(self.root.iterdir()), {self.bin, self.export} | ({self.calls} if self.calls.exists() else set()))
        return code, log, sleep

    def count_calls(self):
        return len(self.calls.read_text().splitlines()) if self.calls.exists() else 0

    def test_absent_credentials_keep_unsigned_build(self):
        code, log, sleep = self.run_preflight({})
        self.assertEqual(code, 0)
        self.assertIn("unsigned", log)
        self.assertEqual(self.count_calls(), 0)
        sleep.assert_not_called()

    def test_signing_only_does_not_require_notarization(self):
        code, log, _ = self.run_preflight({key: self.credentials[key] for key in SIGNING})
        self.assertEqual(code, 0)
        self.assertIn("not checked", log)
        self.assertEqual(self.count_calls(), 0)

    def test_signing_accepts_empty_or_absent_certificate_password(self):
        for password in ({"APPLE_CERTIFICATE_PASSWORD": ""}, {}):
            with self.subTest(password_present=bool(password)):
                signing = {key: self.credentials[key] for key in SIGNING if key != "APPLE_CERTIFICATE_PASSWORD"}
                code, log, _ = self.run_preflight(signing | password)
                self.assertEqual(code, 0)
                self.assertIn("Signing group complete", log)
                self.assertEqual(self.count_calls(), 0)

    def test_workflow_exports_empty_certificate_password_for_tauri(self):
        values = {key: self.credentials[key] for key in SIGNING} | {"APPLE_CERTIFICATE_PASSWORD": ""}
        code, _, _ = self.run_preflight(values)
        self.assertEqual(code, 0)
        export_step = next(step for step in desktop_steps() if "GITHUB_ENV" in step)
        command = re.search(r"        run: \|\n(.*)", export_step, re.DOTALL).group(1)
        result = subprocess.run(
            ["/bin/bash", "-e", "-c", command],
            cwd=ROOT, env=self.env | values | dict.fromkeys(NOTARY, ""),
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=10,
        )
        self.assertEqual(result.returncode, 0)
        exported = dict(line.split("=", 1) for line in self.export.read_text().splitlines())
        self.assertEqual(exported, {"EXISTING": "value"} | values)
        for value in values.values():
            if value:
                self.assertNotIn(value, result.stdout)

    def test_notarization_only_preserves_independent_group(self):
        code, _, _ = self.run_preflight({key: self.credentials[key] for key in NOTARY})
        self.assertEqual(code, 0)
        self.assertEqual(self.count_calls(), 1)

    def test_success_does_not_log_or_export_credentials(self):
        code, log, sleep = self.run_preflight()
        self.assertEqual(code, 0)
        self.assertIn("authentication passed", log)
        self.assertEqual(self.count_calls(), 1)
        sleep.assert_not_called()

    def test_each_missing_required_group_member_fails_before_authentication(self):
        for missing in ("APPLE_CERTIFICATE", "APPLE_SIGNING_IDENTITY") + NOTARY:
            with self.subTest(missing=missing):
                values = {key: value for key, value in self.credentials.items() if key != missing}
                code, log, _ = self.run_preflight(values)
                self.assertEqual(code, 1)
                self.assertIn(missing, log)
                self.assertEqual(self.count_calls(), 0)

    def test_each_lone_group_member_fails_before_authentication(self):
        for present in SIGNING + NOTARY:
            with self.subTest(present=present):
                code, _, _ = self.run_preflight({present: self.credentials[present]})
                self.assertEqual(code, 1)
                self.assertEqual(self.count_calls(), 0)

    def test_blank_or_multiline_credentials_fail_before_authentication(self):
        for key in SIGNING + NOTARY:
            for value in (" ", "\t", self.credentials[key] + "\n", self.credentials[key] + "\r"):
                with self.subTest(key=key, kind=repr(value[-1])):
                    code, log, _ = self.run_preflight(self.credentials | {key: value})
                    self.assertEqual(code, 1)
                    self.assertIn(key, log)
                    self.assertEqual(self.count_calls(), 0)

    def test_authentication_rejection_is_not_retried(self):
        for status in (401, 403):
            with self.subTest(status=status):
                before = self.count_calls()
                code, log, sleep = self.run_preflight(responses=(status,))
                self.assertEqual(code, 1)
                self.assertIn(str(status), log)
                self.assertEqual(self.count_calls() - before, 1)
                sleep.assert_not_called()

    def test_known_transient_failures_retry_then_succeed(self):
        for status in (429, 500, 502, 503, 504):
            with self.subTest(status=status):
                if self.calls.exists():
                    self.calls.unlink()
                code, _, sleep = self.run_preflight(responses=(status, 0))
                self.assertEqual(code, 0)
                self.assertEqual(self.count_calls(), 2)
                sleep.assert_called_once_with(5)

    def test_transient_failure_stops_after_three_attempts(self):
        code, _, sleep = self.run_preflight(responses=(503,))
        self.assertEqual(code, 1)
        self.assertEqual(self.count_calls(), 3)
        self.assertEqual(sleep.call_args_list, [mock.call(5), mock.call(10)])

    def test_unknown_failure_is_not_retried(self):
        for status, reason in ((418, "418"), ("offline", "unclassified")):
            with self.subTest(status=status):
                before = self.count_calls()
                code, log, sleep = self.run_preflight(responses=(status,))
                self.assertEqual(code, 1)
                self.assertIn(reason, log)
                self.assertEqual(self.count_calls() - before, 1)
                sleep.assert_not_called()

    def test_missing_tool_fails_without_traceback_or_retry(self):
        (self.bin / "xcrun").unlink()
        code, log, sleep = self.run_preflight()
        self.assertEqual(code, 1)
        self.assertNotIn("Traceback", log)
        self.assertIn("Xcode", log)
        sleep.assert_not_called()

    def test_timed_out_request_has_a_bounded_retry_budget(self):
        with mock.patch.object(self.helper.subprocess, "run", side_effect=subprocess.TimeoutExpired("hidden", 60)) as run:
            code, log, sleep = self.run_preflight()
        self.assertEqual(code, 1)
        self.assertIn("timed out", log)
        self.assertEqual(run.call_count, 3)
        self.assertTrue(all(call.kwargs["timeout"] == 60 for call in run.call_args_list))
        self.assertEqual(sleep.call_args_list, [mock.call(5), mock.call(10)])

    def test_workflow_stops_before_build_and_export_on_auth_failure(self):
        steps = desktop_steps()
        check = next(step for step in steps if "apple_preflight.py" in step)
        command = re.search(r"^        run: (.+)$", check, re.MULTILINE).group(1)
        build = self.bin / "build-marker"
        build.write_text(f"#!{sys.executable}\nimport pathlib\npathlib.Path({str(self.root / 'built')!r}).touch()\n")
        build.chmod(0o700)
        (self.bin / "python3").symlink_to(sys.executable)
        export_step = next(step for step in steps if "GITHUB_ENV" in step)
        export_command = re.search(r"        run: \|\n(.*)", export_step, re.DOTALL).group(1)
        result = subprocess.run(
            ["/bin/bash", "-e", "-c", command + "\nbuild-marker\n" + export_command],
            cwd=ROOT, env=self.env | self.credentials | {"PREFLIGHT_TEST_RESPONSES": "[401]"},
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=10,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("401", result.stdout)
        self.assertFalse((self.root / "built").exists())
        self.assertEqual(self.export.read_text(), "EXISTING=value\n")
        for value in self.credentials.values():
            self.assertNotIn(value, result.stdout)


if __name__ == "__main__":
    unittest.main()
