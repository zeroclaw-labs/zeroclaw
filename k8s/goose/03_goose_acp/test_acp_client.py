#!/usr/bin/env python3
"""Tests for acp-client wait command and result extraction.

Runs against the acp-client script embedded in the ConfigMap. Extracts the
Python source, imports it as a module, and tests the functions directly.

Usage:
    python3 test_acp_client.py
"""

import importlib.util
import json
import os
import sys
import tempfile
import textwrap
import time
import unittest
from io import StringIO
from unittest.mock import patch

# ── Load acp-client from ConfigMap ──────────────────────────────────────────

def _extract_script_from_configmap():
    """Extract the Python script from the ConfigMap YAML."""
    configmap_path = os.path.join(
        os.path.dirname(__file__), "04_acp_client_configmap.yaml"
    )
    with open(configmap_path) as f:
        content = f.read()

    # Find the script block (indented under acp-client.py: |)
    marker = "acp-client.py: |"
    idx = content.index(marker)
    script_lines = []
    for line in content[idx + len(marker) :].splitlines():
        # ConfigMap YAML indents the script by 4 spaces
        if line and not line.startswith("    "):
            break
        # Remove exactly 4 spaces of YAML indentation
        script_lines.append(line[4:] if line.startswith("    ") else line)
    return "\n".join(script_lines)


def _load_module(source, name="acp_client"):
    """Load Python source as a module."""
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".py", delete=False
    ) as tmp:
        tmp.write(source)
        tmp.flush()
        spec = importlib.util.spec_from_file_location(name, tmp.name)
        mod = importlib.util.module_from_spec(spec)
        # Prevent the module from running main() on import
        sys.modules[name] = mod
        spec.loader.exec_module(mod)
    os.unlink(tmp.name)
    return mod


_SOURCE = _extract_script_from_configmap()
acp = _load_module(_SOURCE)


# ── Tests ───────────────────────────────────────────────────────────────────


class TestExtractResultText(unittest.TestCase):
    """Test _extract_result_text helper."""

    def test_extracts_text_from_content_array(self):
        msg = {
            "result": {
                "content": [{"type": "text", "text": "Hello from k8s agent"}]
            }
        }
        self.assertEqual(acp._extract_result_text(msg), "Hello from k8s agent")

    def test_multiple_text_parts(self):
        msg = {
            "result": {
                "content": [
                    {"type": "text", "text": "Part 1"},
                    {"type": "text", "text": "Part 2"},
                ]
            }
        }
        self.assertEqual(acp._extract_result_text(msg), "Part 1\nPart 2")

    def test_empty_content_array(self):
        msg = {"result": {"content": []}}
        self.assertEqual(acp._extract_result_text(msg), "")

    def test_no_content_key(self):
        msg = {"result": {"sessionId": "abc"}}
        self.assertEqual(acp._extract_result_text(msg), "")

    def test_no_result_key(self):
        msg = {"jsonrpc": "2.0", "method": "notifications/update"}
        self.assertEqual(acp._extract_result_text(msg), "")

    def test_mixed_content_types(self):
        msg = {
            "result": {
                "content": [
                    {"type": "text", "text": "Analysis complete"},
                    {"type": "image", "data": "base64..."},
                    {"type": "text", "text": "See above"},
                ]
            }
        }
        self.assertEqual(
            acp._extract_result_text(msg), "Analysis complete\nSee above"
        )


class TestExtractText(unittest.TestCase):
    """Test _extract_text helper for notifications."""

    def test_returns_none_for_result_messages(self):
        msg = {"result": {"content": [{"type": "text", "text": "hi"}]}}
        self.assertIsNone(acp._extract_text(msg))

    def test_extracts_notification_text(self):
        msg = {
            "params": {
                "update": {"content": {"text": "[Agent starting...]"}}
            }
        }
        self.assertEqual(acp._extract_text(msg), "[Agent starting...]")

    def test_extracts_tool_call_notification(self):
        msg = {
            "params": {
                "update": {"type": "ToolCall", "tool_name": "shell", "status": "running"}
            }
        }
        self.assertEqual(acp._extract_text(msg), "[tool: shell — running]")


class TestWaitCommand(unittest.TestCase):
    """Test cmd_wait logic using filesystem-based session store."""

    def setUp(self):
        self.tmpdir = tempfile.mkdtemp()
        # Patch SESSION_DIR to use temp directory
        self._orig_session_dir = acp.SESSION_DIR
        acp.SESSION_DIR = self.tmpdir

    def tearDown(self):
        acp.SESSION_DIR = self._orig_session_dir
        # Cleanup
        import shutil
        shutil.rmtree(self.tmpdir, ignore_errors=True)

    def _create_session(self, session_id, status, extra=None):
        """Create a session directory with status and optional response."""
        d = os.path.join(self.tmpdir, session_id)
        os.makedirs(d, exist_ok=True)
        data = {"status": status, "updated": time.time()}
        if extra:
            data.update(extra)
        with open(os.path.join(d, "status.json"), "w") as f:
            json.dump(data, f)
        return d

    def _write_response(self, session_id, text):
        d = os.path.join(self.tmpdir, session_id)
        with open(os.path.join(d, "response.txt"), "w") as f:
            f.write(text)

    @patch("time.sleep")
    def test_wait_returns_immediately_when_complete(self, mock_sleep):
        """wait should return the result immediately if already complete."""
        self._create_session("sess-1", "complete", {"finished": time.time()})
        self._write_response("sess-1", "Task done successfully")

        captured = StringIO()
        with patch("sys.stdout", captured):
            acp.cmd_wait("sess-1", timeout=60)

        self.assertIn("Task done successfully", captured.getvalue())
        mock_sleep.assert_not_called()

    @patch("time.sleep")
    def test_wait_polls_until_complete(self, mock_sleep):
        """wait should poll and return when status transitions to complete."""
        sid = "sess-2"
        self._create_session(sid, "running", {"started": time.time()})

        call_count = [0]
        def side_effect(seconds):
            call_count[0] += 1
            if call_count[0] >= 2:
                # Simulate task completing after 2 sleep cycles
                self._create_session(sid, "complete", {"finished": time.time()})
                self._write_response(sid, "Deployment created")

        mock_sleep.side_effect = side_effect

        captured_out = StringIO()
        captured_err = StringIO()
        with patch("sys.stdout", captured_out), patch("sys.stderr", captured_err):
            acp.cmd_wait(sid, timeout=600)

        self.assertIn("Deployment created", captured_out.getvalue())
        # Should have printed progress to stderr
        self.assertIn("[waiting]", captured_err.getvalue())

    @patch("time.sleep")
    def test_wait_timeout(self, mock_sleep):
        """wait should exit with error after timeout."""
        sid = "sess-3"
        self._create_session(sid, "running", {"started": time.time() - 100})
        self._write_response(sid, "Partial output...")

        # Make time.time() advance past deadline
        original_time = time.time
        start = time.time()
        call_count = [0]
        def advancing_time():
            # Each call advances by 5 seconds
            call_count[0] += 1
            return start + call_count[0] * 5

        captured_out = StringIO()
        captured_err = StringIO()
        with patch("time.time", advancing_time), \
             patch("sys.stdout", captured_out), \
             patch("sys.stderr", captured_err):
            with self.assertRaises(SystemExit) as ctx:
                acp.cmd_wait(sid, timeout=3)

        self.assertEqual(ctx.exception.code, 1)
        self.assertIn("TIMEOUT", captured_err.getvalue())

    @patch("time.sleep")
    def test_wait_error_status(self, mock_sleep):
        """wait should report error status and exit 1."""
        sid = "sess-4"
        self._create_session(sid, "error", {"error": "Connection refused"})
        self._write_response(sid, "Partial before failure")

        captured_out = StringIO()
        captured_err = StringIO()
        with patch("sys.stdout", captured_out), patch("sys.stderr", captured_err):
            with self.assertRaises(SystemExit) as ctx:
                acp.cmd_wait(sid, timeout=60)

        self.assertEqual(ctx.exception.code, 1)
        self.assertIn("Connection refused", captured_err.getvalue())

    @patch("time.sleep")
    def test_wait_missing_session_tolerates_initial_absence(self, mock_sleep):
        """wait should tolerate missing session dir for first few checks."""
        sid = "sess-5"
        # Don't create session yet — simulate collector not started

        call_count = [0]
        def side_effect(seconds):
            call_count[0] += 1
            if call_count[0] == 2:
                # Session appears on 2nd sleep
                self._create_session(sid, "complete", {"finished": time.time()})
                self._write_response(sid, "Late result")

        mock_sleep.side_effect = side_effect

        captured_out = StringIO()
        captured_err = StringIO()
        with patch("sys.stdout", captured_out), patch("sys.stderr", captured_err):
            acp.cmd_wait(sid, timeout=600)

        self.assertIn("Late result", captured_out.getvalue())

    @patch("time.sleep")
    def test_wait_empty_response(self, mock_sleep):
        """wait should handle complete status with no response text."""
        self._create_session("sess-6", "complete", {"finished": time.time()})

        captured = StringIO()
        with patch("sys.stdout", captured):
            acp.cmd_wait("sess-6", timeout=60)

        self.assertIn("(empty response)", captured.getvalue())

    @patch("time.sleep")
    def test_wait_progress_with_last_chunk(self, mock_sleep):
        """wait should show last_chunk age in progress messages."""
        sid = "sess-7"
        self._create_session(sid, "running", {
            "started": time.time() - 30,
            "last_chunk": time.time() - 5,
        })

        call_count = [0]
        def side_effect(seconds):
            call_count[0] += 1
            if call_count[0] >= 1:
                self._create_session(sid, "complete", {"finished": time.time()})
                self._write_response(sid, "Done")

        mock_sleep.side_effect = side_effect

        captured_err = StringIO()
        captured_out = StringIO()
        with patch("sys.stdout", captured_out), patch("sys.stderr", captured_err):
            acp.cmd_wait(sid, timeout=600)

        self.assertIn("last activity", captured_err.getvalue())
        self.assertIn("Done", captured_out.getvalue())


if __name__ == "__main__":
    unittest.main()
