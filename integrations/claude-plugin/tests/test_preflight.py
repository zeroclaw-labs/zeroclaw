"""Process-boundary tests; all credentials and CLI responses are synthetic."""

import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "preflight.py"
FIXTURES = ROOT / "tests" / "fixtures"
spec = importlib.util.spec_from_file_location("preflight", SCRIPT)
helper = importlib.util.module_from_spec(spec)
spec.loader.exec_module(helper)


class BoundaryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="zeroclaw-plugin-test-")
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name).resolve()
        self.bin = self.base / "bin with spaces"
        self.bin.mkdir()
        executable = self.bin / "claude"
        executable.write_text("#!" + sys.executable + "\n" +
                              (FIXTURES / "fake_claude.py").read_text(), encoding="utf-8")
        executable.chmod(0o700)
        self.account = self.base / "account with spaces; $(touch injected)"
        self.account.mkdir()
        self.secret_file = self.account / ".credentials.json"
        self.secret_file.write_bytes(b"synthetic-credential-sentinel")
        self.log = self.base / "argv.jsonl"
        self.env = {"PATH": str(self.bin), "HOME": str(self.base),
                    "FIXTURE_LOG": str(self.log), "FIXTURE_MODE": "valid",
                    "FIXTURE_AUTH": str(FIXTURES / "auth_subscription.json")}

    def status(self, **arguments):
        with patch.dict(os.environ, self.env, clear=True):
            return helper.status(arguments)

    def fixture(self, value):
        path = self.base / "status.json"
        path.write_text(json.dumps(value), encoding="utf-8")
        self.env["FIXTURE_AUTH"] = str(path)

    def plan_args(self, **extra):
        return {"instance_root": str(self.base / "fresh instance; $(touch injected)"),
                "provider_alias": "claude_api", "agent_alias": "assistant", **extra}

    def apply_args(self, **extra):
        return self.plan_args(claude_config_dir=str(self.account), confirm_create=True, **extra)

    def fake_zeroclaw(self):
        executable = self.bin / "zeroclaw"
        executable.write_text("#!" + sys.executable + "\n" +
                              (FIXTURES / "fake_zeroclaw.py").read_text(), encoding="utf-8")
        executable.chmod(0o700)

    def apply(self, **extra):
        with patch.dict(os.environ, self.env, clear=True):
            return helper.apply_instance(self.apply_args(**extra))

    def test_apply_calls_canonical_owner_and_reports_verified_receipt(self):
        self.fake_zeroclaw()
        result = self.apply()
        root = Path(self.apply_args()["instance_root"])
        self.assertEqual(result["status"], "ready")
        self.assertTrue(result["inference_verified"])
        self.assertEqual(result["configuration_owner"], "zeroclaw native-onboard")
        self.assertEqual(result["provider_reference"], "claude_code_native.claude_api")
        self.assertEqual(self.secret_file.read_bytes(), b"synthetic-credential-sentinel")
        self.assertTrue((root / "config.toml").is_file())
        call = json.loads((self.base / "zeroclaw-call.json").read_text())
        self.assertEqual(call["argv"][:6], ["--config-dir", str(root), "native-onboard",
                                          "--client", "claude-code", "--provider-alias"])
        self.assertEqual(call["stdin"], "")
        self.assertNotIn("sk-ant-", json.dumps(result))
        self.assertNotIn("fixture-private-diagnostic", json.dumps(result))

    def test_apply_preserves_native_default_without_creating_directory_override(self):
        self.fake_zeroclaw()
        arguments = self.apply_args()
        del arguments["claude_config_dir"]
        with patch.dict(os.environ, self.env, clear=True):
            result = helper.apply_instance(arguments)
        self.assertEqual(result["status"], "ready")
        call = json.loads((self.base / "zeroclaw-call.json").read_text())
        self.assertNotIn("--native-config-dir", call["argv"])
        receipt = json.loads((Path(arguments["instance_root"]) / "native-onboard.json").read_text())
        self.assertIsNone(receipt["request"]["native_config_dir"])

    def test_project_relative_path_entries_cannot_select_shadow_launchers(self):
        self.fake_zeroclaw()
        for name in ("claude", "zeroclaw"):
            shadow = self.base / name
            shadow.write_text("#!" + sys.executable + "\nfrom pathlib import Path\n"
                              "Path('shadow-executed').touch()\n", encoding="utf-8")
            shadow.chmod(0o700)
        self.env["PATH"] = ".:" + str(self.bin) + ":relative:"
        previous = Path.cwd()
        try:
            os.chdir(self.base)
            self.assertEqual(self.apply()["status"], "ready")
        finally:
            os.chdir(previous)
        self.assertFalse((self.base / "shadow-executed").exists())

    def test_apply_retains_the_executable_admitted_by_preflight(self):
        self.fake_zeroclaw()
        evil = self.bin / "other-zeroclaw"
        marker = self.base / "unadmitted-executable"
        evil.write_text("#!" + sys.executable + "\nfrom pathlib import Path\n"
                        "Path(" + repr(str(marker)) + ").touch()\n", encoding="utf-8")
        evil.chmod(0o700)
        with patch.object(helper.shutil, "which", side_effect=[str(self.bin / "claude"),
                         str(self.bin / "zeroclaw"), str(evil)]):
            result = self.apply()
        self.assertEqual(result["status"], "ready")
        self.assertFalse(marker.exists())

    def test_same_second_resume_requires_new_atomic_receipt_publication(self):
        self.fake_zeroclaw()
        self.env["FIXTURE_VALIDATION_TIME"] = "1000"
        with patch.object(helper.time, "time", return_value=1000.9):
            self.assertEqual(self.apply()["status"], "ready")
            receipt = Path(self.apply_args()["instance_root"]) / "native-onboard.json"
            old = receipt.read_bytes()
            self.env["FIXTURE_ZEROCLAW_MODE"] = "no_refresh"
            result = self.apply(resume=True)
            self.assertEqual(receipt.read_bytes(), old)
            self.assertNotEqual(result["status"], "ready")
            self.assertFalse(result["inference_verified"])
            self.env["FIXTURE_ZEROCLAW_MODE"] = "ready"
            # A real new publication in the same second is admitted, even if
            # timestamp and serialized bytes happen to be identical.
            self.assertEqual(self.apply(resume=True)["status"], "ready")

    def test_apply_requires_exact_creation_risk_and_billing_choices_before_spawn(self):
        self.fake_zeroclaw()
        for extra in [{"confirm_create": False}, {"confirm_create": 1},
                      {"confirm_create": "true"}, {"risk_preset": "yolo"},
                      {"expected_billing": "api"}, {"engine_backend": "anthropic_api",
                       "accept_api_billing": True}, {"argv": ["--dangerous"]}]:
            with self.subTest(extra=extra), patch.dict(os.environ, self.env, clear=True), self.assertRaises(ValueError):
                helper.apply_instance({**self.apply_args(), **extra})
        self.assertFalse((self.base / "zeroclaw-call.json").exists())
        self.assertFalse(Path(self.apply_args()["instance_root"]).exists())

    def test_apply_refuses_missing_incompatible_or_mismatched_prerequisites(self):
        self.assertEqual(self.apply()["status"], "missing_zeroclaw")
        self.fake_zeroclaw()
        self.env["FIXTURE_ZEROCLAW_MODE"] = "incompatible"
        self.assertEqual(self.apply()["status"], "incompatible_zeroclaw")
        self.env["FIXTURE_ZEROCLAW_MODE"] = "ready"
        self.env["FIXTURE_AUTH"] = str(FIXTURES / "auth_api.json")
        self.assertEqual(self.apply()["status"], "native_billing_not_verified")
        self.assertFalse((self.base / "zeroclaw-call.json").exists())

    def test_unavailable_supervision_refuses_before_any_cli_or_instance_work(self):
        self.fake_zeroclaw()
        with patch.object(helper.SUPERVISION, "check_support", return_value=False):
            result = self.apply()
        self.assertEqual(result["status"], "process_supervision_unavailable")
        self.assertFalse(result["inference_verified"])
        self.assertFalse(self.log.exists())
        self.assertFalse((self.base / "zeroclaw-call.json").exists())
        self.assertFalse(Path(self.apply_args()["instance_root"]).exists())

    def test_supervision_failure_is_reported_without_claiming_cleanup_or_ready(self):
        self.fake_zeroclaw()
        with patch.object(helper.SUPERVISION, "check_support", return_value=True), \
                patch.object(helper.SUPERVISION, "passive_exited", side_effect=helper.SUPERVISION.SupervisionError()):
            result = self.apply()
        self.assertEqual(result["status"], "cleanup_unverified")
        self.assertFalse(result["inference_verified"])
        self.assertIsNone(result["last_validation_at"])

    def test_apply_never_claims_ready_from_failed_stale_or_invalid_receipts(self):
        self.fake_zeroclaw()
        for mode in ["nonzero", "missing_receipt", "wrong_request", "no_validation", "receipt_symlink", "stale_validation"]:
            with self.subTest(mode=mode):
                self.env["FIXTURE_ZEROCLAW_MODE"] = mode
                result = self.apply(instance_root=str(self.base / mode))
                self.assertNotEqual(result["status"], "ready")
                self.assertFalse(result["inference_verified"])
                self.assertNotIn("sk-ant-", json.dumps(result))
        self.assertEqual(self.secret_file.read_bytes(), b"synthetic-credential-sentinel")

    def test_apply_resume_is_explicit_and_only_canonical_owner_admits_it(self):
        self.fake_zeroclaw()
        self.env["FIXTURE_ZEROCLAW_MODE"] = "configured"
        result = self.apply()
        self.assertEqual(result["status"], "configured")
        self.assertFalse(result["inference_verified"])
        with self.assertRaises(ValueError):
            self.apply()
        self.env["FIXTURE_ZEROCLAW_MODE"] = "ready"
        self.assertEqual(self.apply(resume=True)["status"], "ready")

    def test_apply_deadline_interrupts_process_and_retains_owned_state(self):
        self.fake_zeroclaw()
        self.env["FIXTURE_ZEROCLAW_MODE"] = "sleep"
        with patch.object(helper, "APPLY_TIMEOUT_SECONDS", 0.4):
            result = self.apply()
        self.assertEqual(result["status"], "apply_timeout")
        self.assertFalse(result["inference_verified"])
        self.assertTrue(Path(self.apply_args()["instance_root"]).exists())
        call = json.loads((self.base / "zeroclaw-call.json").read_text())
        self.assertLess(time.monotonic() - call["started_monotonic"], 3)
        pid = call["pid"]
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)

    def assert_native_heartbeat_stopped(self):
        heartbeat = self.base / "native-heartbeat"
        before = heartbeat.stat().st_size if heartbeat.exists() else 0
        time.sleep(.15)
        after = heartbeat.stat().st_size if heartbeat.exists() else 0
        self.assertEqual(before, after, "ordinary native child continued after cancellation")

    def test_apply_timeout_cleans_noncooperative_native_group(self):
        self.fake_zeroclaw()
        self.env["FIXTURE_ZEROCLAW_MODE"] = "multi_group_sleep"
        with patch.object(helper, "APPLY_TIMEOUT_SECONDS", .5):
            result = self.apply()
        self.assertEqual(result["status"], "apply_timeout")
        self.assertFalse(result["inference_verified"])
        call = json.loads((self.base / "zeroclaw-call.json").read_text())
        self.assertIn("native_child_pid", call)
        with self.assertRaises(ProcessLookupError):
            os.kill(call["pid"], 0)
        self.assert_native_heartbeat_stopped()

    def test_mcp_cancel_close_and_termination_clean_native_other_group(self):
        self.fake_zeroclaw()
        self.env["FIXTURE_ZEROCLAW_MODE"] = "multi_group_sleep"
        for action in ("cancel", "close", "terminate"):
            with self.subTest(action=action):
                log = self.base / "zeroclaw-call.json"
                if log.exists():
                    log.unlink()
                request = {"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                           "params": {"name": "bootstrap.apply", "arguments": self.apply_args(
                               instance_root=str(self.base / ("native-" + action)))}}
                process = subprocess.Popen([sys.executable, "-I", "-B", str(SCRIPT), "--stdio"],
                                           stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                           cwd=self.base, env=self.env)
                try:
                    process.stdin.write((json.dumps(request) + "\n").encode())
                    process.stdin.flush()
                    deadline = time.monotonic() + 6
                    while not log.exists() and time.monotonic() < deadline:
                        time.sleep(.02)
                    self.assertTrue(log.exists())
                    call = json.loads(log.read_text())
                    self.assertIn("native_child_pid", call)
                    if action == "cancel":
                        cancel = {"jsonrpc": "2.0", "method": "notifications/cancelled",
                                  "params": {"requestId": 1}}
                        process.stdin.write((json.dumps(cancel) + "\n").encode())
                        process.stdin.flush()
                    if action == "terminate":
                        process.terminate()
                    else:
                        process.stdin.close()
                    process.wait(timeout=5)
                    response = json.loads(process.stdout.read())
                    result = json.loads(response["result"]["content"][0]["text"])
                    self.assertEqual(result["status"], "apply_cancelled")
                    self.assertFalse(result["inference_verified"])
                    self.assertEqual(process.stderr.read(), b"")
                    self.assert_native_heartbeat_stopped()
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait()
                    if not process.stdin.closed:
                        process.stdin.close()
                    process.stdout.close()
                    process.stderr.close()

    def test_native_headless_token_presence_requires_matching_reported_method(self):
        self.env["CLAUDE_CODE_OAUTH_TOKEN"] = "synthetic-native-token"
        self.fixture({"loggedIn": True, "authMethod": "oauth_token", "apiProvider": "firstParty"})
        self.assertEqual(self.status()["native_code"]["auth"]["billing_status"], "matches")
        self.env["ANTHROPIC_API_KEY"] = "synthetic-api-key"
        self.assertEqual(self.status()["native_code"]["auth"]["billing_status"], "unknown")

    def test_mcp_connection_close_cancels_active_apply(self):
        self.fake_zeroclaw()
        self.env["FIXTURE_ZEROCLAW_MODE"] = "sleep"
        request = {"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": "bootstrap.apply", "arguments": self.apply_args()}}
        process = subprocess.Popen([sys.executable, "-I", "-B", str(SCRIPT), "--stdio"],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   cwd=self.base, env=self.env)
        try:
            process.stdin.write((json.dumps(request) + "\n").encode())
            process.stdin.flush()
            deadline = time.monotonic() + 5
            while not (self.base / "zeroclaw-call.json").exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue((self.base / "zeroclaw-call.json").exists())
            process.stdin.close()
            process.wait(timeout=4)
            response = json.loads(process.stdout.read())
            self.assertEqual(json.loads(response["result"]["content"][0]["text"])["status"], "apply_cancelled")
            self.assertEqual(process.stderr.read(), b"")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            process.stdout.close()
            process.stderr.close()

    def test_mcp_apply_is_mutating_and_cancellation_reaches_canonical_process(self):
        self.fake_zeroclaw()
        self.env["FIXTURE_ZEROCLAW_MODE"] = "sleep"
        tools = {tool["name"]: tool for tool in helper.TOOLS}
        self.assertFalse(tools["bootstrap.apply"]["annotations"]["readOnlyHint"])
        request = {"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": "bootstrap.apply", "arguments": self.apply_args()}}
        process = subprocess.Popen([sys.executable, "-I", "-B", str(SCRIPT), "--stdio"],
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   cwd=self.base, env=self.env)
        try:
            process.stdin.write((json.dumps(request) + "\n").encode())
            process.stdin.flush()
            deadline = time.monotonic() + 5
            while not (self.base / "zeroclaw-call.json").exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue((self.base / "zeroclaw-call.json").exists())
            cancel = {"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1}}
            process.stdin.write((json.dumps(cancel) + "\n").encode())
            process.stdin.flush()
            process.stdin.close()
            process.wait(timeout=4)
            response = json.loads(process.stdout.read())
            self.assertEqual(json.loads(response["result"]["content"][0]["text"])["status"], "apply_cancelled")
            self.assertEqual(process.stderr.read(), b"")
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()
            process.stdout.close()
            process.stderr.close()

    def run_server(self, requests, raw=None, **extra_env):
        payload = raw if raw is not None else b"".join(
            (json.dumps(r) + "\n").encode() for r in requests)
        result = subprocess.run([sys.executable, "-I", "-B", str(SCRIPT), "--stdio"],
                                input=payload, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                cwd=self.base, env={**self.env, **extra_env}, timeout=15)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stderr, b"")
        return [json.loads(line) for line in result.stdout.splitlines()]

    def copied_package(self):
        target = self.base / "copied plugin"
        shutil.copytree(ROOT, target, ignore=shutil.ignore_patterns("tests"))
        return target

    def test_native_windows_status_never_probes_or_spawns_cli(self):
        with patch.object(helper.os, "name", "nt"), \
                patch.object(helper.shutil, "which", side_effect=AssertionError("CLI probe")), \
                patch.object(helper.subprocess, "Popen", side_effect=AssertionError("CLI spawn")):
            result = helper.status({})
        self.assertEqual(result["native_code"]["cli_status"], "unsupported_platform")
        self.assertIsNone(result["native_code"]["auth"])
        self.assertFalse(result["zeroclaw_engine"]["inference_verified"])
        self.assertFalse(self.log.exists())

    def test_native_windows_process_boundary_rejects_cli_spawn(self):
        with patch.object(helper.os, "name", "nt"), \
                patch.object(helper.subprocess, "Popen", side_effect=AssertionError("CLI spawn")):
            outcome, output = helper.run_cli("claude", ["--version"], self.env)
        self.assertEqual(outcome, "unsupported_platform")
        self.assertIsNone(output)

    def test_manifest_version_and_minimum_drive_actual_helper_protocol(self):
        package = self.copied_package()
        manifest_path = package / ".claude-plugin" / "plugin.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["version"] = "9.8.7"
        manifest["metadata"]["minimumClaudeCodeVersion"] = "2.1.290"
        manifest_path.write_text(json.dumps(manifest))
        requests = [{"jsonrpc": "2.0", "id": 1, "method": "initialize"},
                    {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                     "params": {"name": "bootstrap.status", "arguments": {}}}]
        result = subprocess.run([sys.executable, "-I", "-B", str(package / "scripts/preflight.py"), "--stdio"],
                                input=b"".join((json.dumps(r) + "\n").encode() for r in requests),
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                cwd=self.base, env=self.env, timeout=5)
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stderr, b"")
        output = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual(output[0]["result"]["serverInfo"]["version"], "9.8.7")
        value = json.loads(output[1]["result"]["content"][0]["text"])
        self.assertEqual(value["native_code"]["cli_status"], "unsupported_cli_version")
        self.assertEqual(json.loads(self.log.read_text())["argv"], ["--version"])

    def test_invalid_package_metadata_is_rejected_without_raw_output(self):
        package = self.copied_package()
        manifest_path = package / ".claude-plugin" / "plugin.json"
        request = (json.dumps({"jsonrpc": "2.0", "id": 1, "method": "initialize"}) + "\n").encode()
        valid = json.loads(manifest_path.read_text())
        cases = [b"not-json sk-ant-fixture-secret", (json.dumps(valid) + " " * 40000).encode(),
                 json.dumps({**valid, "version": "sk-ant-fixture-secret"}).encode(),
                 json.dumps({**valid, "metadata": []}).encode(),
                 json.dumps({**valid, "metadata": {"minimumClaudeCodeVersion": "sk-ant-fixture-secret"}}).encode(),
                 b"[]", None]
        for value in cases:
            with self.subTest(value_type=type(value).__name__):
                if value is None:
                    manifest_path.unlink()
                else:
                    manifest_path.write_bytes(value)
                result = subprocess.run([sys.executable, "-I", "-B", str(package / "scripts/preflight.py"), "--stdio"],
                                        input=request, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                        cwd=self.base, env=self.env, timeout=5)
                self.assertEqual(result.returncode, 0)
                self.assertEqual(result.stderr, b"")
                self.assertEqual(json.loads(result.stdout)["error"]["message"], "invalid_package_metadata")
                self.assertNotIn(b"sk-ant-", result.stdout)
        self.assertFalse(self.log.exists())

    def test_package_metadata_capture_has_a_fixed_memory_bound(self):
        test = self

        class BoundedFile(io.BytesIO):
            def read(self, size=-1):
                test.assertTrue(0 <= size <= 32769, "package metadata read must be bounded")
                return super().read(size)

        data = json.dumps({"version": "1.2.3", "metadata": {"minimumClaudeCodeVersion": "2.1.289"}}).encode()
        with patch.object(helper.Path, "open", return_value=BoundedFile(data)):
            self.assertEqual(helper.load_package_metadata()["version"], "1.2.3")

    def test_invalid_package_metadata_blocks_direct_status(self):
        with patch.object(helper, "PACKAGE_METADATA", None), self.assertRaises(ValueError):
            self.status()
        self.assertFalse(self.log.exists())

    def test_invalid_package_metadata_blocks_direct_plan(self):
        with patch.object(helper, "PACKAGE_METADATA", None), self.assertRaises(ValueError):
            helper.plan(self.plan_args())
        self.assertFalse(Path(self.plan_args()["instance_root"]).exists())

    def test_account_directory_and_argv_are_literal_and_parent_env_is_unchanged(self):
        self.env["CLAUDE_CONFIG_DIR"] = str(self.base)
        self.env["ANTHROPIC_API_KEY"] = "synthetic-fixture-key"
        with patch.dict(os.environ, self.env, clear=True):
            result = helper.status({"claude_config_dir": str(self.account)})
            self.assertEqual(os.environ["CLAUDE_CONFIG_DIR"], str(self.base))
        log = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual([r["argv"] for r in log], [["--version"], ["auth", "status", "--json"]])
        self.assertTrue(all(r["config_dir"] == str(self.account) for r in log))
        self.assertTrue(all(r["api_key_present"] for r in log))
        self.assertEqual(result["native_code"]["auth"]["credential_mode"], "ambiguous")
        self.assertFalse((self.base / "injected").exists())
        self.assertEqual(self.secret_file.read_bytes(), b"synthetic-credential-sentinel")

    def test_subscription_is_host_auth_and_engine_still_requires_configuration(self):
        result = self.status(claude_config_dir=str(self.account))
        self.assertEqual(result["native_code"]["auth"]["credential_mode"], "claude_subscription")
        self.assertTrue(result["native_code"]["auth"]["billing_matches_expectation"])
        self.assertEqual(result["zeroclaw_engine"]["status"], "requires_configuration")
        self.assertEqual(result["zeroclaw_engine"]["native_code_backend"], "claude_code_native")
        self.assertEqual(result["zeroclaw_engine"]["bootstrap_cli_status"], "missing_zeroclaw")
        self.assertFalse(result["zeroclaw_engine"]["inference_verified"])

    def test_allowlist_excludes_identity_unknown_fields_and_hostile_strings(self):
        self.fixture({"loggedIn": True, "authMethod": "claude.ai", "apiProvider": "firstParty",
                      "email": "user@example.com", "accessToken": "sk-ant-fixture-secret",
                      "unknown": "fixture-private-diagnostic", "subscriptionType": "sk-ant-fixture-secret"})
        output = json.dumps(self.status())
        for forbidden in ["user@example.com", "accessToken", "sk-ant-", "fixture-private-diagnostic"]:
            self.assertNotIn(forbidden, output)
        self.fixture({"loggedIn": True, "authMethod": "sk-ant-fixture-secret",
                      "apiProvider": "sk-ant-fixture-secret"})
        result = self.status()
        self.assertEqual(result["native_code"]["auth"]["credential_mode"], "unknown")
        self.assertNotIn("sk-ant-", json.dumps(result))

    def test_api_and_console_never_count_as_subscription(self):
        for fixture, mode in [("auth_api.json", "anthropic_api"), ("auth_console.json", "console_api")]:
            with self.subTest(fixture=fixture):
                self.env["FIXTURE_AUTH"] = str(FIXTURES / fixture)
                result = self.status()
                self.assertEqual(result["native_code"]["auth"]["credential_mode"], mode)
                self.assertFalse(result["native_code"]["auth"]["billing_matches_expectation"])
                self.assertEqual(result["native_code"]["auth"]["billing_status"], "mismatch")
                self.assertEqual(self.status(expected_billing="api")["native_code"]["auth"]["billing_status"], "matches")

    def test_api_provider_and_environment_conflicts_have_unknown_billing(self):
        for provider, selectors in [("bedrock", {}), ("unknown", {}),
                                    ("firstParty", {"CLAUDE_CODE_USE_BEDROCK": "1"}),
                                    ("firstParty", {"ANTHROPIC_BASE_URL": "https://host.invalid"}),
                                    ("firstParty", {"ANTHROPIC_AUTH_TOKEN": "synthetic-token"})]:
            with self.subTest(provider=provider, selectors=selectors):
                self.fixture({"loggedIn": True, "authMethod": "api_key", "apiProvider": provider})
                with patch.dict(os.environ, {**self.env, **selectors}, clear=True):
                    auth = helper.status({"expected_billing": "api"})["native_code"]["auth"]
                self.assertEqual(auth["credential_mode"], "ambiguous")
                self.assertEqual(auth["billing_source"], "unknown")
                self.assertIsNone(auth["billing_matches_expectation"])
        self.env["FIXTURE_AUTH"] = str(FIXTURES / "auth_api.json")
        self.env["ANTHROPIC_API_KEY"] = "synthetic-key"
        self.assertEqual(self.status(expected_billing="api")["native_code"]["auth"]["credential_mode"], "anthropic_api")
        self.fixture({"loggedIn": True, "authMethod": "gateway", "apiProvider": "gateway"})
        self.assertEqual(self.status(expected_billing="cloud_or_gateway")["native_code"]["auth"]["credential_mode"], "cloud_gateway")

    def test_cloud_gateway_headless_and_federation_modes(self):
        for method, provider, extra, mode in [
            ("bedrock", "bedrock", {}, "cloud_provider"),
            ("gateway", "gateway", {}, "cloud_gateway"),
            ("oauth_token", "firstParty", {}, "native_code_token"),
            ("anthropic_profile", "firstParty", {"profileAuthMode": "oidc_federation"}, "federation_api"),
        ]:
            with self.subTest(method=method):
                self.fixture({"loggedIn": True, "authMethod": method, "apiProvider": provider, **extra})
                result = self.status()
                self.assertEqual(result["native_code"]["auth"]["credential_mode"], mode)
                self.assertFalse(result["zeroclaw_engine"]["inference_verified"])

    def test_environment_indicators_reveal_conflict_without_values(self):
        for name in ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY", "ANTHROPIC_PROFILE",
                     "ANTHROPIC_BASE_URL", "CLAUDE_CODE_USE_BEDROCK"]:
            with self.subTest(name=name):
                self.env[name] = "synthetic-private-value"
                result = self.status()
                self.assertEqual(result["native_code"]["auth"]["credential_mode"], "ambiguous")
                self.assertIsNone(result["native_code"]["auth"]["billing_matches_expectation"])
                self.assertNotIn("synthetic-private-value", json.dumps(result))
                del self.env[name]

    def test_missing_binary_and_wrong_binary_are_sanitized(self):
        self.env["PATH"] = ""
        self.assertEqual(self.status()["native_code"]["cli_status"], "missing_cli")
        self.env["PATH"] = str(self.bin)
        self.env["FIXTURE_MODE"] = "wrong_binary"
        self.assertEqual(self.status()["native_code"]["cli_status"], "unrecognized_cli")
        self.assertEqual(len(self.log.read_text().splitlines()), 1)

    def test_invalid_auth_payload_and_failure_do_not_relay_output(self):
        for mode, expected in [("malformed", "malformed_auth_status"), ("nonzero", "auth_status_failed")]:
            with self.subTest(mode=mode):
                self.env["FIXTURE_MODE"] = mode
                result = self.status()
                self.assertEqual(result["native_code"]["cli_status"], expected)
                self.assertNotIn("fixture-private-diagnostic", json.dumps(result))
                self.assertNotIn("sk-ant-", json.dumps(result))
        self.env["FIXTURE_MODE"] = "valid"
        for value in [[], {"loggedIn": "true", "authMethod": "claude.ai"}]:
            self.fixture(value)
            self.assertEqual(self.status()["native_code"]["cli_status"], "malformed_auth_status")

    def test_old_and_unusable_cli_are_rejected_before_auth_status(self):
        self.env["FIXTURE_VERSION"] = "2.1.100 (Claude Code)"
        self.assertEqual(self.status()["native_code"]["cli_status"], "unsupported_cli_version")
        self.assertEqual(len(self.log.read_text().splitlines()), 1)
        with patch.object(helper.shutil, "which", return_value=str(self.base / "absent-cli")):
            self.assertEqual(self.status()["native_code"]["cli_status"], "unusable_cli")

    def test_invalid_directory_guard_without_later_path_checks(self):
        for value in [None, "", "relative", "/tmp/../fresh", "/tmp/line\nfeed", "/tmp/" + "x" * 1024]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                helper.directory_reference(value)

    def test_output_frame_limit_at_stdio_boundary(self):
        request = json.dumps({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).encode() + b"\n"
        output = io.BytesIO()
        stdin = type("Input", (), {"buffer": io.BytesIO(request)})()
        stdout = type("Output", (), {"buffer": output})()
        with patch.object(helper.sys, "stdin", stdin), patch.object(helper.sys, "stdout", stdout), \
                patch.object(helper, "MAX_OUTPUT_BYTES", 64):
            helper.stdio()
        self.assertEqual(json.loads(output.getvalue())["error"]["message"], "output_limit")

    def test_mcp_params_must_be_an_object(self):
        output = self.run_server([{"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": []}])
        self.assertEqual(output[0]["error"]["code"], -32602)

    def test_custom_endpoint_unknown_profile_and_headless_selector_stay_unknown(self):
        self.fixture({"loggedIn": True, "authMethod": "claude.ai", "apiProvider": "customEndpoint"})
        self.assertEqual(self.status()["native_code"]["auth"]["credential_mode"], "ambiguous")
        self.fixture({"loggedIn": True, "authMethod": "anthropic_profile", "apiProvider": "firstParty"})
        self.assertEqual(self.status()["native_code"]["auth"]["billing_status"], "unknown")
        self.env["FIXTURE_AUTH"] = str(FIXTURES / "auth_subscription.json")
        self.env["CLAUDE_CODE_OAUTH_TOKEN"] = "synthetic-native-token"
        self.assertEqual(self.status()["native_code"]["auth"]["credential_mode"], "ambiguous")

    def test_extracted_package_manifest_launches_with_declared_runtime_without_zeroclaw(self):
        manifest = json.loads((ROOT / ".claude-plugin" / "plugin.json").read_text())
        self.assertEqual(manifest["name"], "zeroclaw")
        self.assertEqual(manifest["version"], helper.VERSION)
        metadata = manifest["metadata"]
        self.assertEqual(metadata["operations"], [t["name"] for t in helper.TOOLS])
        self.assertEqual(metadata["minimumClaudeCodeVersion"], ".".join(map(str, helper.MIN_CLAUDE_VERSION)))
        self.assertEqual(metadata["nativeZeroClawModelBackend"], "claude_code_native")
        self.assertTrue(metadata["writesInstanceConfiguration"])
        self.assertFalse((ROOT / "bin").exists())
        self.assertFalse((ROOT / "hooks").exists())
        archive = self.base / "plugin.zip"
        shipped = [".claude-plugin/plugin.json", ".mcp.json", "scripts/preflight.py",
                   "scripts/process_supervision.py", "skills/onboard/SKILL.md", "README.md", "LICENSE"]
        with zipfile.ZipFile(archive, "w") as z:
            for name in shipped:
                z.write(ROOT / name, name)
        extracted = self.base / "extracted plugin"
        with zipfile.ZipFile(archive) as z:
            z.extractall(extracted)
        server = json.loads((extracted / ".mcp.json").read_text())["mcpServers"]["preflight"]
        self.assertEqual(server["command"], "python3")
        self.assertEqual(server["args"][:2], ["-I", "-B"])
        self.assertNotIn("env", server)
        argv = [sys.executable, *[v.replace("${CLAUDE_PLUGIN_ROOT}", str(extracted)) for v in server["args"]]]
        request = {"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                   "params": {"name": "bootstrap.status", "arguments": {}}}
        process = subprocess.run(argv, input=(json.dumps(request) + "\n").encode(),
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 env={"PATH": "", "HOME": str(self.base)}, cwd=self.base, timeout=5)
        self.assertEqual(process.returncode, 0)
        self.assertEqual(process.stderr, b"")
        value = json.loads(json.loads(process.stdout)["result"]["content"][0]["text"])
        self.assertEqual(value["native_code"]["cli_status"], "missing_cli")
        self.assertEqual(value["zeroclaw_engine"]["status"], "requires_configuration")

    def test_logout_is_not_an_authenticated_mode(self):
        self.fixture({"loggedIn": False, "authMethod": "claude.ai", "apiProvider": "firstParty"})
        auth = self.status()["native_code"]["auth"]
        self.assertEqual(auth["credential_mode"], "unauthenticated")
        self.assertFalse(auth["billing_matches_expectation"])

    def test_logged_out_exit_one_is_a_sanitized_process_observation(self):
        self.fixture({"loggedIn": False, "authMethod": "none", "apiProvider": "firstParty",
                      "email": "user@example.com", "accessToken": "sk-ant-fixture-secret"})
        self.env.update(FIXTURE_AUTH_EXIT="1", FIXTURE_MODE="hostile_stderr")
        result = self.status(claude_config_dir=str(self.account))
        self.assertEqual(result["native_code"]["cli_status"], "available")
        auth = result["native_code"]["auth"]
        self.assertFalse(auth["logged_in"])
        self.assertEqual(auth["credential_mode"], "unauthenticated")
        self.assertEqual(auth["billing_source"], "none")
        self.assertFalse(result["zeroclaw_engine"]["inference_verified"])
        self.assertNotIn("sk-ant-", json.dumps(result))
        self.assertNotIn("user@example.com", json.dumps(result))
        self.assertEqual(self.secret_file.read_bytes(), b"synthetic-credential-sentinel")
        outcome, output = helper.run_cli(str(self.bin / "claude"), ["auth", "status", "--json"], self.env)
        self.assertEqual(outcome, "ok")
        self.assertEqual(json.loads(output), {"loggedIn": False})

    def test_logged_out_exit_one_crosses_stdio_without_raw_output(self):
        self.fixture({"loggedIn": False, "authMethod": "none", "apiProvider": "firstParty",
                      "unknown": "fixture-private-diagnostic", "token": "sk-ant-fixture-secret"})
        self.env.update(FIXTURE_AUTH_EXIT="1", FIXTURE_MODE="hostile_stderr")
        response = self.run_server([{"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                                    "params": {"name": "bootstrap.status", "arguments": {}}}])[0]
        self.assertFalse(response["result"]["isError"])
        value = json.loads(response["result"]["content"][0]["text"])
        self.assertEqual(value["native_code"]["cli_status"], "available")
        self.assertFalse(value["native_code"]["auth"]["logged_in"])
        self.assertEqual(value["native_code"]["auth"]["credential_mode"], "unauthenticated")
        self.assertEqual(value["native_code"]["auth"]["billing_source"], "none")
        self.assertFalse(value["zeroclaw_engine"]["inference_verified"])
        self.assertNotIn("sk-ant-", json.dumps(response))
        self.assertNotIn("fixture-private-diagnostic", json.dumps(response))

    def test_auth_exit_one_admission_rejects_bad_state_json_and_other_exit_codes(self):
        cases = [(1, {"loggedIn": True}), (1, {"loggedIn": "false"}), (1, {"loggedIn": 0}),
                 (1, []), (2, {"loggedIn": False}), (7, {"loggedIn": False})]
        for exit_code, payload in cases:
            with self.subTest(exit_code=exit_code, payload=payload):
                self.fixture(payload)
                self.env["FIXTURE_AUTH_EXIT"] = str(exit_code)
                result = self.status()
                self.assertEqual(result["native_code"]["cli_status"], "auth_status_failed")
                self.assertIsNone(result["native_code"]["auth"])
        self.env["FIXTURE_AUTH_EXIT"] = "1"
        path = Path(self.env["FIXTURE_AUTH"])
        for raw in ["not JSON sk-ant-fixture-secret", '{"loggedIn":false,"unknown":NaN}']:
            path.write_text(raw)
            result = self.status()
            self.assertEqual(result["native_code"]["cli_status"], "auth_status_failed")
            self.assertNotIn("sk-ant-", json.dumps(result))

    def test_nonzero_version_never_uses_auth_exit_one_admission(self):
        self.env["FIXTURE_VERSION_EXIT"] = "1"
        self.assertEqual(self.status()["native_code"]["cli_status"], "unusable_cli")
        self.env["FIXTURE_VERSION"] = '{"loggedIn":false}'
        outcome, output = helper.run_cli(str(self.bin / "claude"), ["--version"], self.env)
        self.assertEqual(outcome, "command_failed")
        self.assertIsNone(output)

    def test_documented_api_key_helper_method_reports_api_billing_without_secrets(self):
        self.fixture({"loggedIn": True, "authMethod": "api_key_helper", "apiProvider": "firstParty",
                      "email": "user@example.com", "token": "sk-ant-fixture-secret"})
        result = self.status(expected_billing="api")
        self.assertEqual(result["native_code"]["auth"]["reported_method"], "api_key_helper")
        self.assertEqual(result["native_code"]["auth"]["credential_mode"], "anthropic_api")
        self.assertEqual(result["native_code"]["auth"]["billing_source"], "anthropic_api")
        self.assertEqual(result["native_code"]["auth"]["billing_status"], "matches")
        self.assertEqual(self.status()["native_code"]["auth"]["billing_status"], "mismatch")
        self.assertNotIn("sk-ant-", json.dumps(result))
        self.assertNotIn("user@example.com", json.dumps(result))

    def test_documented_third_party_method_keeps_unproven_billing_unknown(self):
        self.fixture({"loggedIn": True, "authMethod": "third_party", "apiProvider": "bedrock"})
        result = self.status(expected_billing="cloud_or_gateway")
        self.assertEqual(result["native_code"]["auth"]["credential_mode"], "unknown")
        self.assertEqual(result["native_code"]["auth"]["billing_source"], "unknown")
        self.assertIsNone(result["native_code"]["auth"]["billing_matches_expectation"])

    def test_timeout_and_output_limits_cross_real_process_boundary(self):
        with patch.object(helper, "TIMEOUT_SECONDS", 0.25):
            self.env["FIXTURE_MODE"] = "timeout"
            started = time.monotonic()
            self.assertEqual(self.status()["native_code"]["cli_status"], "cli_timeout")
            self.assertLess(time.monotonic() - started, 2)
        self.env["FIXTURE_MODE"] = "overflow"
        self.assertEqual(self.status()["native_code"]["cli_status"], "cli_output_limit")

    def test_child_holding_stdout_cannot_extend_deadline(self):
        self.env["FIXTURE_MODE"] = "child_pipe"
        with patch.object(helper, "TIMEOUT_SECONDS", 0.75):
            started = time.monotonic()
            result, output = helper.run_cli(str(self.bin / "claude"), ["auth", "status", "--json"], self.env)
            self.assertEqual(result, "cli_timeout")
            self.assertIsNone(output)
            self.assertLess(time.monotonic() - started, 2)
        self.assertEqual(json.loads(self.log.read_text())["argv"], ["auth", "status", "--json"])

    def test_successful_native_stderr_is_discarded_before_capture(self):
        self.env["FIXTURE_MODE"] = "hostile_stderr"
        result = self.status()
        self.assertEqual(result["native_code"]["cli_status"], "available")
        self.assertEqual(result["native_code"]["auth"]["credential_mode"], "claude_subscription")
        self.assertNotIn("sk-ant-", json.dumps(result))

    def test_account_directory_must_exist_and_never_reads_credential_files(self):
        with self.assertRaises(ValueError):
            self.status(claude_config_dir=str(self.base / "missing"))
        with patch("builtins.open", side_effect=AssertionError("helper file read")), \
                patch.object(Path, "open", side_effect=AssertionError("helper credential read")):
            self.status(claude_config_dir=str(self.account))
        self.assertEqual(self.secret_file.read_bytes(), b"synthetic-credential-sentinel")

    def test_plan_is_read_only_references_canonical_presets_and_cannot_be_ready(self):
        args = self.plan_args()
        result = helper.plan(args)
        self.assertEqual(result["status"], "requires_configuration")
        self.assertEqual(result["risk"]["preset"], "balanced")
        self.assertEqual(result["risk"]["effective_policy_status"], "unresolved")
        self.assertEqual(result["provider"]["alias"], "claude_api")
        self.assertEqual(result["agent"]["risk_profile"], "balanced")
        self.assertEqual(result["terminal_handoff"]["owner"], "zeroclaw native-onboard")
        self.assertFalse(Path(args["instance_root"]).exists())
        self.assertFalse(self.log.exists())

    def test_native_plan_names_real_provider_agent_and_canonical_bootstrap(self):
        args = self.plan_args(claude_config_dir=str(self.account))
        result = helper.plan(args)
        self.assertEqual(result["status"], "requires_configuration")
        self.assertEqual(result["provider"]["config_reference"], "claude_code_native.claude_api")
        self.assertEqual(result["agent"]["model_provider"], "claude_code_native.claude_api")
        self.assertEqual(result["provider"]["expected_billing"], "subscription")
        self.assertEqual(result["native_account_directory"]["path"], str(self.account))
        self.assertEqual(result["terminal_handoff"]["owner"], "zeroclaw native-onboard")
        self.assertIn("--native-config-dir", result["terminal_handoff"]["argv"])
        self.assertNotIn("setup-token", json.dumps(result))
        self.assertFalse(result["writes_performed"])
        self.assertFalse(result["inference_verified"])
        self.assertFalse(Path(args["instance_root"]).exists())

    def test_native_plan_requires_explicit_api_billing_acceptance(self):
        args = self.plan_args(expected_billing="api")
        with self.assertRaises(ValueError):
            helper.plan(args)
        result = helper.plan({**args, "accept_api_billing": True})
        self.assertEqual(result["provider"]["engine_backend"], "native_claude_code")
        self.assertEqual(result["provider"]["expected_billing"], "api")
        self.assertEqual(result["provider"]["config_reference"], "claude_code_native.claude_api")

    def test_native_plan_model_billing_and_yolo_are_literal_canonical_arguments(self):
        args = self.plan_args(model="sonnet[1m]", risk_preset="yolo", accept_yolo=True,
                              expected_billing="api", accept_api_billing=True,
                              claude_config_dir=str(self.account))
        result = helper.plan(args)
        argv = result["terminal_handoff"]["argv"]
        self.assertEqual(argv, ["zeroclaw", "--config-dir", args["instance_root"],
                               "native-onboard", "--client", "claude-code",
                               "--provider-alias", "claude_api", "--agent-alias", "assistant",
                               "--model", "sonnet[1m]", "--risk-preset", "yolo",
                               "--expected-billing", "api", "--accept-yolo", "--accept-api-billing",
                               "--native-config-dir", str(self.account)])
        self.assertFalse(self.log.exists())
        self.assertFalse(Path(args["instance_root"]).exists())
        for field in [{"model": "--dangerously-skip-permissions"}, {"model": "a;touch injected"},
                      {"model": "x" * 129}, {"model": 1}, {"expected_billing": "unknown"}]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                helper.plan(self.plan_args(**field))

    def test_installed_bootstrap_command_requires_real_flags_without_writes(self):
        executable = self.bin / "zeroclaw"
        flags = ["claude-code", "--client", "--provider-alias", "--agent-alias", "--model", "--risk-preset",
                 "--expected-billing", "--accept-yolo", "--accept-api-billing", "--native-config-dir"]
        for expected, output in [("available", " ".join(flags)),
                                 ("incompatible_zeroclaw", " ".join(flags[1:])),
                                 ("incompatible_zeroclaw", " ".join(flags[:-1])),
                                 ("incompatible_zeroclaw", "synthetic-secret-unrecognized")]:
            executable.write_text("#!" + sys.executable + "\nimport sys\n"
                                  "assert sys.argv[1:] == ['native-onboard', '--help']\n"
                                  "print(" + repr(output) + ")\n", encoding="utf-8")
            executable.chmod(0o700)
            result = self.status()
            self.assertEqual(result["zeroclaw_engine"]["bootstrap_cli_status"], expected)
            self.assertFalse(result["zeroclaw_engine"]["inference_verified"])
            self.assertNotIn("synthetic-secret", json.dumps(result))
        self.assertEqual(self.secret_file.read_bytes(), b"synthetic-credential-sentinel")

    def test_api_handoff_needs_explicit_billing_choice_and_uses_only_real_flags(self):
        args = self.plan_args(engine_backend="anthropic_api")
        with self.assertRaises(ValueError):
            helper.plan(args)
        result = helper.plan({**args, "accept_api_billing": True})
        self.assertEqual(result["status"], "requires_configuration")
        self.assertEqual(result["terminal_handoff"]["argv"],
                         ["zeroclaw", "--config-dir", args["instance_root"], "quickstart",
                          "--model-provider", "anthropic", "--agent", "assistant"])
        self.assertEqual(result["terminal_handoff"]["provider_alias_selection"], "claude_api")
        self.assertFalse(Path(args["instance_root"]).exists())

    def test_yolo_requires_exact_explicit_boolean_and_still_has_unresolved_policy(self):
        for consent in [False, "true", 1]:
            with self.subTest(consent=consent), self.assertRaises(ValueError):
                helper.plan(self.plan_args(risk_preset="yolo", accept_yolo=consent))
        result = helper.plan(self.plan_args(risk_preset="yolo", accept_yolo=True))
        self.assertEqual(result["risk"]["preset"], "yolo")
        self.assertEqual(result["risk"]["effective_policy_status"], "unresolved")

    def test_existing_roots_symlinks_and_account_overlap_are_refused(self):
        existing = self.base / "existing"
        existing.mkdir()
        config = existing / "config.toml"
        config.write_bytes(b"synthetic-existing-config")
        alias = self.base / "parent-link"
        alias.symlink_to(existing, target_is_directory=True)
        cases = [str(existing), str(alias / "fresh"), str(self.account / "fresh")]
        with patch.dict(os.environ, {"CLAUDE_CONFIG_DIR": str(self.account)}, clear=True):
            for root in cases:
                with self.subTest(root=root), self.assertRaises(ValueError):
                    helper.plan(self.plan_args(instance_root=root))
        self.assertEqual(config.read_bytes(), b"synthetic-existing-config")

    def test_argument_validation_fails_closed(self):
        for function in [helper.status, helper.plan]:
            with self.assertRaises(ValueError):
                function([])
        invalid = [
            {"instance_root": "relative"}, {"instance_root": str(self.base / ".." / "fresh")},
            {"instance_root": str(self.base / "missing-parent" / "fresh")},
            {"instance_root": "/tmp/line\nfeed"}, {"instance_root": "/tmp/" + "x" * 1024},
            {"provider_alias": "a; touch injected"}, {"agent_alias": "sk-ant-fixture-secret"},
            {"risk_preset": "full"}, {"engine_backend": "claude-code"},
            {"extra": "fixture-private-diagnostic"}, {"accept_yolo": True},
            {"accept_api_billing": True},
        ]
        for extra in invalid:
            with self.subTest(extra=extra), self.assertRaises(ValueError):
                helper.plan(self.plan_args(**extra))
        for extra in [{"expected_billing": "unknown"}, {"token": "sk-ant-fixture-secret"}]:
            with self.subTest(extra=extra), self.assertRaises(ValueError):
                self.status(**extra)

    def test_stdio_frames_only_and_fixed_read_plan_capabilities(self):
        requests = [
            {"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 1, "method": "tools/list"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "bootstrap.status", "arguments": {}}},
            {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "bootstrap.plan", "arguments": self.plan_args()}},
            {"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 3}},
        ]
        output = self.run_server(requests)
        self.assertEqual([r["id"] for r in output], [0, 1, 2, 3])
        tools = {t["name"]: t for t in output[1]["result"]["tools"]}
        self.assertEqual(set(tools), {"bootstrap.status", "bootstrap.plan", "bootstrap.apply"})
        self.assertTrue(tools["bootstrap.status"]["annotations"]["readOnlyHint"])
        self.assertTrue(tools["bootstrap.plan"]["annotations"]["readOnlyHint"])
        self.assertFalse(tools["bootstrap.apply"]["annotations"]["readOnlyHint"])
        self.assertEqual(json.loads(output[-1]["result"]["content"][0]["text"])["status"], "requires_configuration")
        self.assertFalse(Path(self.plan_args()["instance_root"]).exists())

    def test_unknown_methods_install_and_wrong_inputs_never_touch_existing_config(self):
        config = self.base / "config.toml"
        config.write_bytes(b"synthetic-existing-config")
        requests = [
            {"jsonrpc": "2.0", "id": 1, "method": "bootstrap.install", "params": {"approve": "fixture-digest"}},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "bootstrap.install", "arguments": self.plan_args()}},
            {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "bootstrap.status", "arguments": {"login": True}}},
            {"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 1}},
        ]
        output = self.run_server(requests)
        self.assertEqual(output[0]["error"]["code"], -32601)
        self.assertEqual(output[1]["error"]["code"], -32602)
        self.assertTrue(output[2]["result"]["isError"])
        self.assertEqual(config.read_bytes(), b"synthetic-existing-config")
        self.assertFalse(self.log.exists())

    def test_malformed_and_oversized_mcp_input_has_bounded_sanitized_output(self):
        self.assertEqual(self.run_server([], raw=b"{malformed\n")[0]["error"]["code"], -32700)
        self.assertEqual(self.run_server([], raw=b"x" * 40000 + b"\n")[0]["error"]["code"], -32600)
        for payload in [[], {"jsonrpc": "1.0", "id": 1, "method": "ping"},
                        {"jsonrpc": "2.0", "id": {}, "method": "ping"}]:
            self.assertEqual(self.run_server([payload])[0]["error"]["code"], -32600)


if __name__ == "__main__":
    unittest.main()
