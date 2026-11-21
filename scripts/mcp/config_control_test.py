#!/usr/bin/env python3
"""Transport and gateway-boundary tests; no external services or credentials."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

import config_control as bridge


class Fixture:
    def __init__(self):
        self.calls = []
        self.token = "fixture-private-token"
        self.fail_status = False
        self.reject_patch = False
        self.patch_status = 200
        self.running = []
        self.patches = []
        self.redirect = False
        self.active = False
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_GET(self):
                self.handle_request()

            def do_PATCH(self):
                self.handle_request()

            def do_POST(self):
                self.handle_request()

            def handle_request(self):
                length = int(self.headers.get("Content-Length", 0))
                body = json.loads(self.rfile.read(length)) if length else None
                fixture.calls.append((self.command, self.path, body))
                if self.headers.get("Authorization") != "Bearer " + fixture.token:
                    self.reply(401, {"error": "unauthorized"})
                elif fixture.redirect:
                    self.send_response(302)
                    self.send_header("Location", "/redirect-destination")
                    self.end_headers()
                elif self.path == "/api/config" and self.command == "PATCH":
                    if fixture.reject_patch:
                        self.reply(422, {"code": "validation_failed", "message": fixture.token})
                    else:
                        fixture.patches.append(body)
                        self.reply(fixture.patch_status, {"value": fixture.token})
                elif self.path == "/api/config/reload-status":
                    self.reply(503 if fixture.fail_status else 200, {"pending_reload": True})
                elif self.path == "/api/sessions/running?activity_only=true":
                    data = ({"sessions": [], "busy": True} if fixture.active
                            else fixture.running.pop(0) if fixture.running else {"sessions": [], "busy": False})
                    if "message" not in data and "busy" not in data:
                        data["busy"] = bool(data["sessions"])
                    self.reply(200, data)
                elif self.path == "/admin/reload":
                    self.reply(200, {"success": True})
                elif self.path.startswith("/api/config/map-key?"):
                    self.reply(200, {"created": True, "value": fixture.token})
                elif self.path.startswith("/api/config/prop?"):
                    self.reply(200, {"path": "fixture.api_key", "populated": True})
                elif self.path.startswith("/api/config/list?"):
                    self.reply(200, {"entries": [{"path": f"fixture.{i}", "value": str(i)} for i in range(5)]})
                else:
                    self.reply(404, {"error": "not_found"})

            def reply(self, status, body):
                encoded = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                self.wfile.write(encoded)

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def url(self):
        return f"http://127.0.0.1:{self.server.server_port}"

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()


class ConfigControlTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name)
        self.fixture = Fixture()
        self.token_file = self.root / "token"
        self.token_file.write_text(self.fixture.token)
        self.token_file.chmod(0o600)
        self.gateway = bridge.Gateway(self.fixture.url, self.token_file)
        self.control = bridge.Control(self.gateway, self.root / "jobs")

    def tearDown(self):
        self.fixture.close()
        self.directory.cleanup()

    def test_atomic_patch_is_forwarded_once_without_echoing_values(self):
        operations = [{"op": "test", "path": "agent.limit", "value": 50},
                      {"op": "replace", "path": "agent.limit", "value": 200}]
        result = self.control.call("patch", {"operations": operations})
        self.assertEqual(self.fixture.patches, [operations])
        self.assertTrue(result["saved"])
        self.assertNotIn(self.fixture.token, json.dumps(result))

    def test_rejected_patch_returns_only_safe_metadata(self):
        self.fixture.reject_patch = True
        with self.assertRaises(bridge.BridgeError) as caught:
            self.control.call("set", {"path": "fixture.limit", "value": 200})
        self.assertEqual(caught.exception.details["status"], 422)
        self.assertEqual(caught.exception.details["code"], "validation_failed")
        self.assertNotIn(self.fixture.token, json.dumps(caught.exception.details))
        self.assertEqual(self.fixture.patches, [])

    def test_status_failure_cannot_hide_a_successful_write(self):
        self.fixture.fail_status = True
        result = self.control.call("set", {"path": "fixture.limit", "value": 200})
        self.assertTrue(result["saved"])
        self.assertTrue(result["application_status_unavailable"])
        self.assertEqual(len(self.fixture.patches), 1)

    def test_secrets_use_the_masked_gateway_property_surface(self):
        result = self.control.call("get", {"path": "fixture.api_key"})
        self.assertEqual(result, {"path": "fixture.api_key", "populated": True})
        self.assertNotIn(self.fixture.token, json.dumps(result))

    def test_pagination_and_invalid_batches(self):
        result = self.control.call("list", {"prefix": "fixture", "offset": 1, "limit": 2})
        self.assertEqual([entry["value"] for entry in result["entries"]], ["1", "2"])
        self.assertEqual(result["next_offset"], 3)
        before = len(self.fixture.calls)
        for arguments in ({"operations": []}, {"operations": [{"op": "copy", "path": "x"}]},
                          {"operations": [{"op": "add", "path": "x"}]},
                          {"operations": [{"op": "add", "path": "x", "value": 1, "extra": True}]}):
            with self.assertRaises(bridge.BridgeError):
                self.control.call("patch", arguments)
        self.assertEqual(len(self.fixture.calls), before)

    def test_alias_creation_uses_canonical_gateway_api(self):
        result = self.control.call("create_alias", {"path": "mcp.servers", "key": "fixture"})
        self.assertTrue(result["created"])
        self.assertEqual(self.fixture.calls[-1][1], "/api/config/map-key?path=mcp.servers&key=fixture")
        self.assertNotIn(self.fixture.token, json.dumps(result))

    def test_token_rotation_and_file_protection(self):
        self.control.call("get", {"path": "fixture.api_key"})
        self.fixture.token = "rotated-fixture-token"
        self.token_file.write_text(self.fixture.token)
        self.control.call("get", {"path": "fixture.api_key"})
        self.token_file.chmod(0o644)
        with self.assertRaises(bridge.BridgeError):
            self.control.call("get", {"path": "fixture.api_key"})
        self.token_file.chmod(0o600)
        link = self.root / "token-link"
        link.symlink_to(self.token_file)
        with self.assertRaises(OSError):
            bridge.Gateway(self.fixture.url, link).token()

    def test_redirects_and_non_loopback_targets_are_refused(self):
        for url in ("https://127.0.0.1:1234", "http://example.invalid:1234", "http://127.0.0.1:1234/?x=1",
                    "http://user:password@127.0.0.1:1234", "http://0.0.0.0:1234"):
            with self.assertRaises(bridge.BridgeError):
                bridge.Gateway(url, self.token_file)
        self.fixture.redirect = True
        with self.assertRaises(bridge.BridgeError):
            self.control.call("get", {"path": "fixture.api_key"})
        self.assertEqual(len(self.fixture.calls), 1)

    def test_uncertain_mutations_are_never_retried(self):
        with patch.object(self.gateway.http, "open", side_effect=TimeoutError) as send:
            with self.assertRaises(bridge.BridgeError) as caught:
                self.gateway.request("PATCH", "/api/config", [{"op": "add", "path": "fixture.limit", "value": 200}])
        self.assertEqual(send.call_count, 1)
        self.assertTrue(caught.exception.details["outcome_unknown"])

    def test_in_turn_writes_return_queued_without_calling_the_mutating_endpoint(self):
        self.fixture.running = [{"sessions": [{"session_id": "calling-turn"}]}]
        secret_value = "fixture-value-must-stay-out-of-job-files"
        with patch.object(bridge.subprocess, "Popen") as spawn:
            result = self.control.call("set", {"path": "fixture.setting", "value": secret_value})
        self.assertTrue(result["queued"])
        self.assertFalse(result["saved"])
        self.assertEqual(self.fixture.patches, [])
        self.assertNotIn(secret_value, json.dumps(spawn.call_args.args))
        job = next((self.root / "jobs").glob("write-*.json"))
        self.assertNotIn(secret_value, job.read_text())
        self.assertIn(secret_value.encode(), spawn.return_value.stdin.write.call_args.args[0])

    def test_all_mutation_surfaces_defer_while_a_turn_is_running(self):
        for name, arguments in [
            ("create_alias", {"path": "mcp.servers", "key": "fixture"}),
            ("patch", {"operations": [{"op": "add", "path": "fixture.limit", "value": 200}]}),
        ]:
            self.fixture.running = [{"sessions": [{"session_id": "calling-turn"}]}]
            with patch.object(bridge.subprocess, "Popen"):
                self.assertTrue(self.control.call(name, arguments)["queued"])
        self.assertTrue(all(call[0] == "GET" for call in self.fixture.calls))

    def test_write_worker_waits_for_turn_completion_then_saves(self):
        job = self.root / "write-fixture.json"
        bridge.write_job(job, {"id": "fixture", "requested_at": time.time(), "state": "waiting_for_idle"})
        self.fixture.running = [{"sessions": [{"session_id": "caller"}]}, {"sessions": []}, {"sessions": []}]
        request = {"method": "PATCH", "path": "/api/config", "body": [{"op": "add", "path": "fixture.limit", "value": 200}]}
        with patch.object(bridge.time, "sleep"):
            bridge.run_write(self.gateway, job, request, idle_seconds=0)
        methods = [call[0] for call in self.fixture.calls]
        self.assertEqual(methods, ["GET", "GET", "GET", "PATCH"])
        self.assertEqual(json.loads(job.read_text())["state"], "saved")

    def test_write_worker_preserves_queued_request_order(self):
        job = self.root / "write-fixture.json"
        bridge.write_job(job, {"id": "fixture", "requested_at": time.time(), "state": "waiting_for_idle"})
        request = {"method": "PATCH", "path": "/api/config", "body": []}
        with patch.object(bridge, "pending_writes", side_effect=[True, True, False]), patch.object(bridge.time, "sleep"):
            bridge.run_write(self.gateway, job, request, idle_seconds=0)
        self.assertEqual([call[0] for call in self.fixture.calls], ["GET", "GET", "GET", "PATCH"])

    def test_reload_cannot_overtake_queued_writes(self):
        job = self.root / "reload-fixture.json"
        bridge.write_job(job, {"requested_at": time.time(), "state": "waiting_for_idle"})
        with patch.object(bridge, "pending_writes", side_effect=[True, False, False]), patch.object(bridge.time, "sleep"):
            bridge.run_reload(self.gateway, job, idle_seconds=0)
        self.assertEqual([call[1] for call in self.fixture.calls], ["/api/sessions/running?activity_only=true"] * 3 + ["/admin/reload"])

    def test_write_worker_retries_read_probe_but_not_the_mutation(self):
        job = self.root / "write-fixture.json"
        bridge.write_job(job, {"id": "fixture", "requested_at": time.time(), "state": "waiting_for_idle"})
        request = {"method": "PATCH", "path": "/api/config", "body": []}
        with patch.object(self.gateway, "request", side_effect=[
            bridge.BridgeError("gateway_request_failed", outcome_unknown=False),
            {"sessions": [], "busy": False}, {"sessions": [], "busy": False},
            bridge.BridgeError("gateway_request_failed", outcome_unknown=True),
        ]) as send, patch.object(bridge.time, "sleep"):
            bridge.run_write(self.gateway, job, request, idle_seconds=0)
        self.assertEqual(sum(call.args[0] == "PATCH" for call in send.call_args_list), 1)
        self.assertTrue(json.loads(job.read_text())["outcome_unknown"])

    def test_reload_waits_for_idle_and_records_only_request_acceptance(self):
        job = self.root / "job.json"
        bridge.write_job(job, {"requested_at": time.time(), "state": "waiting_for_idle"})
        self.fixture.running = [{"sessions": [{"session_id": "fixture"}]}, {"sessions": []}, {"sessions": []}]
        with patch.object(bridge.time, "sleep"):
            bridge.run_reload(self.gateway, job, idle_seconds=0)
        self.assertEqual(self.fixture.calls[-1][1], "/admin/reload")
        self.assertEqual(json.loads(job.read_text())["state"], "reload_requested")

    def test_reload_fails_closed_without_native_idle_evidence(self):
        job = self.root / "job.json"
        bridge.write_job(job, {"requested_at": time.time(), "state": "waiting_for_idle"})
        self.fixture.running = [{"sessions": [], "message": "Session persistence is disabled"}]
        bridge.run_reload(self.gateway, job, idle_seconds=0)
        self.assertEqual(json.loads(job.read_text())["state"], "failed")
        self.assertFalse(any(call[1] == "/admin/reload" for call in self.fixture.calls))

    def test_reload_returns_before_worker_and_never_passes_the_token(self):
        with patch.object(bridge.subprocess, "Popen") as spawn:
            result = self.control.call("reload", {})
        self.assertEqual(result["state"], "waiting_for_idle")
        self.assertNotIn(self.fixture.token, json.dumps(spawn.call_args.args))
        self.assertEqual(list((self.root / "jobs").glob("*.json"))[0].stat().st_mode & 0o777, 0o600)

    def test_real_stdio_process_discovers_tools_and_reaches_gateway(self):
        requests = [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}},
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
            {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "set", "arguments": {"path": "fixture.limit", "value": 200}}},
        ]
        result = subprocess.run([sys.executable, str(Path(bridge.__file__)), "--gateway-url", self.fixture.url,
                                 "--token-file", str(self.token_file), "--state-dir", str(self.root / "jobs")],
                                input="".join(json.dumps(value) + "\n" for value in requests),
                                capture_output=True, text=True, timeout=10, check=True)
        replies = [json.loads(line) for line in result.stdout.splitlines()]
        self.assertEqual([reply["id"] for reply in replies], [1, 2, 3])
        self.assertEqual(replies[0]["result"]["protocolVersion"], "2025-03-26")
        self.assertEqual(len(replies[1]["result"]["tools"]), 7)
        self.assertFalse(replies[2]["result"]["isError"])
        self.assertEqual(len(self.fixture.patches), 1)
        self.assertNotIn(self.fixture.token, result.stdout + result.stderr)

    def test_real_queued_write_outlives_call_and_waits_for_the_calling_turn(self):
        self.fixture.active = True
        requests = [
            {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                "name": "set", "arguments": {"path": "channels.telegram.fixture.stream_mode", "value": "multi_message"}}},
        ]
        started = time.monotonic()
        result = subprocess.run([sys.executable, str(Path(bridge.__file__)), "--gateway-url", self.fixture.url,
                                 "--token-file", str(self.token_file), "--state-dir", str(self.root / "jobs")],
                                input="".join(json.dumps(value) + "\n" for value in requests),
                                capture_output=True, text=True, timeout=10, check=True)
        self.assertLess(time.monotonic() - started, 10)
        reply = json.loads(result.stdout.splitlines()[-1])["result"]
        data = json.loads(reply["content"][0]["text"])
        self.assertTrue(data["queued"])
        self.assertFalse(data["saved"])
        self.assertEqual(self.fixture.patches, [])
        # Finishing the tool is insufficient: the parent turn must also end.
        time.sleep(0.1)
        self.assertEqual(self.fixture.patches, [])
        self.fixture.active = False
        job = self.root / "jobs" / ("write-" + data["job"]["id"] + ".json")
        deadline = time.monotonic() + 25
        while json.loads(job.read_text())["state"] == "waiting_for_idle" and time.monotonic() < deadline:
            time.sleep(0.05)
        while json.loads(job.read_text())["state"] == "applying" and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertEqual(json.loads(job.read_text())["state"], "saved")
        self.assertEqual(self.fixture.patches, [[{"op": "add", "path": "channels.telegram.fixture.stream_mode", "value": "multi_message"}]])

    def test_http_timeout_mutation_is_uncertain_even_if_the_edit_saved(self):
        self.fixture.patch_status = 408
        with self.assertRaises(bridge.BridgeError) as caught:
            self.control.call("set", {"path": "fixture.limit", "value": 200})
        self.assertTrue(caught.exception.details["outcome_unknown"])
        self.assertEqual(len(self.fixture.patches), 1)
        job = self.root / "write-timeout.json"
        bridge.write_job(job, {"id": "timeout", "requested_at": time.time(), "state": "waiting_for_idle"})
        with patch.object(bridge.time, "sleep"):
            bridge.run_write(self.gateway, job, {"method": "PATCH", "path": "/api/config", "body": []}, idle_seconds=0)
        self.assertTrue(json.loads(job.read_text())["outcome_unknown"])
        self.assertEqual(len(self.fixture.patches), 2, "neither uncertain write is retried")

    def test_runtime_busy_signal_defers_all_writes_without_persisted_sessions(self):
        for name, arguments in [
            ("set", {"path": "fixture.limit", "value": 200}),
            ("patch", {"operations": [{"op": "add", "path": "fixture.limit", "value": 200}]}),
            ("create_alias", {"path": "mcp.servers", "key": "fixture"}),
        ]:
            self.fixture.running = [{"sessions": [], "busy": True}]
            with patch.object(bridge.subprocess, "Popen"):
                self.assertTrue(self.control.call(name, arguments)["queued"])
        self.assertEqual(self.fixture.patches, [])

    def test_workers_wait_for_runtime_activity_without_a_websocket_session(self):
        for reload in (False, True):
            job = self.root / ("reload-runtime.json" if reload else "write-runtime.json")
            bridge.write_job(job, {"id": "runtime", "requested_at": time.time(), "state": "waiting_for_idle"})
            self.fixture.running = [{"sessions": [], "busy": True},
                                    {"sessions": [], "busy": False}, {"sessions": [], "busy": False}]
            self.fixture.calls.clear()
            with patch.object(bridge.time, "sleep"):
                if reload:
                    bridge.run_reload(self.gateway, job, idle_seconds=0)
                else:
                    bridge.run_write(self.gateway, job, {"method": "PATCH", "path": "/api/config", "body": []}, idle_seconds=0)
            self.assertEqual([call[0] for call in self.fixture.calls[:3]], ["GET"] * 3)
            self.assertEqual(self.fixture.calls[-1][0], "POST" if reload else "PATCH")

    def test_missing_or_malformed_runtime_activity_fails_closed(self):
        for data in ({"sessions": []}, {"busy": None}, {"busy": 0}, [], None):
            with patch.object(self.gateway, "request", return_value=data):
                with self.assertRaises(bridge.BridgeError):
                    self.control.call("set", {"path": "fixture.limit", "value": 200})
        self.assertEqual(self.fixture.patches, [])

    def test_activity_probe_ignores_stale_persisted_running_sessions(self):
        # Old daemons may ignore the projection query and include a stale list.
        # Only the live coordinator's typed busy signal decides idle.
        self.fixture.running = [{"sessions": [{"session_id": "stale"}], "busy": False}]
        self.assertFalse(bridge.sessions_busy(self.gateway))
        self.assertEqual(self.fixture.calls[-1][1], "/api/sessions/running?activity_only=true")
        self.fixture.active = True
        self.assertTrue(bridge.sessions_busy(self.gateway))

    def test_completed_retention_is_bounded_and_preserves_all_active_jobs(self):
        directory = self.root / "jobs"
        directory.mkdir()
        now = time.time()
        for index in range(bridge.COMPLETED_JOB_LIMIT + 10):
            # Seed old installation records without invoking eager pruning.
            (directory / f"write-{index}.json").write_text(json.dumps({
                "id": str(index), "requested_at": now - index, "completed_at": now - index, "state": "saved"}))
        for state in ("waiting_for_idle", "applying"):
            bridge.write_job(directory / f"write-{state}.json", {
                "id": state, "requested_at": now - 10_000, "state": state})
        bridge.write_job(directory / "reload-old.json", {
            "requested_at": now - bridge.COMPLETED_JOB_MAX_AGE - 1, "state": "failed"})
        self.assertFalse(bridge.pending_writes(directory))
        records = bridge.job_records(directory)
        self.assertEqual(len(records), bridge.COMPLETED_JOB_LIMIT + 2)
        self.assertTrue((directory / "write-waiting_for_idle.json").exists())
        self.assertTrue((directory / "write-applying.json").exists())
        self.assertFalse((directory / "reload-old.json").exists())
        self.assertFalse((directory / "write-109.json").exists())
        status = self.control.call("status", {})
        self.assertEqual(len(status["requests"]), 20)

    def test_oversized_or_invalid_requests_cannot_execute_a_tail(self):
        output = io.StringIO()
        bridge.serve(self.control, io.BytesIO(b"x" * (bridge.MAX_MESSAGE + 1) + b"\n"), output)
        self.assertEqual(output.getvalue(), "")
        self.assertEqual(self.fixture.calls, [])


if __name__ == "__main__":
    unittest.main()
