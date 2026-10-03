#!/usr/bin/env python3

"""Tests for the scripted provider used by the plugin artifact smoke."""

from __future__ import annotations

import contextlib
import http.client
import io
import json
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import plugin_smoke_provider as provider  # noqa: E402

ECHO_TOOL = {"type": "function", "function": {"name": "config-echo", "parameters": {}}}
OTHER_TOOL = {"type": "function", "function": {"name": "file_read", "parameters": {}}}
USER = {"role": "user", "content": "smoke"}
ARGUMENTS = '{"text":"hello world"}'


def asked_for(tool: str) -> dict:
    return {
        "role": "assistant",
        "tool_calls": [
            {
                "id": provider.CALL_ID,
                "type": "function",
                "function": {"name": tool, "arguments": ARGUMENTS},
            }
        ],
    }


def answered(content: object) -> dict:
    return {"role": "tool", "tool_call_id": provider.CALL_ID, "content": content}


class ProviderRefused(Exception):
    """The provider answered with a non-200 status."""

    def __init__(self, status: int, payload: object) -> None:
        super().__init__(f"provider answered {status}: {payload}")
        self.status = status
        self.payload = payload


class RunningProvider:
    """One `serve` process stand-in, run on a thread for the test's lifetime."""

    def __init__(self, directory: Path) -> None:
        self.log = directory / "requests.jsonl"
        self.script = directory / "script.json"
        self.stop_file = directory / "stop"
        self.port_file = directory / "port"
        self.thread = threading.Thread(target=self.run, daemon=True)

    def run(self) -> None:
        provider.main(
            [
                "serve",
                "--port-file",
                str(self.port_file),
                "--log",
                str(self.log),
                "--script",
                str(self.script),
                "--stop-file",
                str(self.stop_file),
            ]
        )

    def __enter__(self) -> "RunningProvider":
        self.thread.start()
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self.port_file.exists() and self.port_file.read_text().strip():
                return self
            time.sleep(0.02)
        raise AssertionError("the provider did not publish its port")

    def __exit__(self, *_: object) -> None:
        self.stop_file.touch()
        self.thread.join(timeout=10)

    def set_script(self, tool_match: str, tool_arguments: str = ARGUMENTS) -> None:
        provider.main(
            [
                "script",
                "--file",
                str(self.script),
                "--tool-match",
                tool_match,
                "--tool-arguments",
                tool_arguments,
            ]
        )

    def post(self, body: dict, path: str = "/chat/completions") -> dict:
        # A plain loopback connection with an explicit path: the test never
        # builds a URL, so nothing here can be steered at another scheme.
        port = int(self.port_file.read_text().strip())
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        try:
            connection.request(
                "POST",
                path,
                body=json.dumps(body).encode("utf-8"),
                headers={"content-type": "application/json"},
            )
            response = connection.getresponse()
            payload = json.loads(response.read())
        finally:
            connection.close()
        if response.status != 200:
            raise ProviderRefused(response.status, payload)
        return payload

    def summary(self, tool_match: str) -> dict[str, str]:
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            provider.main(
                ["summarize", "--log", str(self.log), "--tool-match", tool_match]
            )
        return dict(line.split("=", 1) for line in output.getvalue().splitlines())


class PluginSmokeProviderTest(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        running = RunningProvider(Path(directory.name))
        running.__enter__()
        self.addCleanup(running.__exit__, None, None, None)
        self.provider = running

    def test_asks_for_the_offered_tool_then_finishes_the_turn(self) -> None:
        self.provider.set_script("config-echo")

        first = self.provider.post({"messages": [USER], "tools": [OTHER_TOOL, ECHO_TOOL]})
        choice = first["choices"][0]
        self.assertEqual(choice["finish_reason"], "tool_calls")
        call = choice["message"]["tool_calls"][0]["function"]
        self.assertEqual(call["name"], "config-echo")
        self.assertEqual(call["arguments"], ARGUMENTS)

        second = self.provider.post(
            {
                "messages": [USER, asked_for("config-echo"), answered("text=HELLO")],
                "tools": [OTHER_TOOL, ECHO_TOOL],
            }
        )
        self.assertEqual(second["choices"][0]["finish_reason"], "stop")
        self.assertEqual(
            second["choices"][0]["message"]["content"], provider.TURN_COMPLETE
        )

        self.assertEqual(
            self.provider.summary("config-echo"),
            {
                "requests": "2",
                "tools_offered": "2",
                "offered": "yes",
                "tool_name": "config-echo",
                "tool_results": "1",
                "tool_result": "text=HELLO",
            },
        )

    def test_reports_a_tool_that_was_never_offered(self) -> None:
        self.provider.set_script("config-echo")

        reply = self.provider.post({"messages": [USER], "tools": [OTHER_TOOL]})
        self.assertEqual(
            reply["choices"][0]["message"]["content"], provider.TOOL_NOT_OFFERED
        )
        self.assertNotIn("tool_calls", reply["choices"][0]["message"])

        summary = self.provider.summary("config-echo")
        self.assertEqual(summary["requests"], "1")
        self.assertEqual(summary["tools_offered"], "1")
        self.assertEqual(summary["offered"], "no")
        self.assertEqual(summary["tool_results"], "0")
        self.assertEqual(summary["tool_result"], "")

    def test_a_request_without_tools_is_told_apart_from_a_missing_tool(self) -> None:
        self.provider.set_script("config-echo")

        self.provider.post({"messages": [USER]})

        summary = self.provider.summary("config-echo")
        self.assertEqual(summary["requests"], "1")
        self.assertEqual(summary["tools_offered"], "0")
        self.assertEqual(summary["offered"], "no")

    def test_never_calls_a_tool_the_script_did_not_name(self) -> None:
        self.provider.set_script("redact")

        reply = self.provider.post({"messages": [USER], "tools": [OTHER_TOOL, ECHO_TOOL]})

        self.assertEqual(
            reply["choices"][0]["message"]["content"], provider.TOOL_NOT_OFFERED
        )

    def test_the_script_is_read_again_for_every_request(self) -> None:
        self.provider.set_script("config-echo")
        self.provider.post({"messages": [USER], "tools": [ECHO_TOOL]})
        self.provider.set_script("file_read", '{"path":"notes.txt"}')

        reply = self.provider.post({"messages": [USER], "tools": [OTHER_TOOL, ECHO_TOOL]})

        call = reply["choices"][0]["message"]["tool_calls"][0]["function"]
        self.assertEqual(call["name"], "file_read")
        self.assertEqual(call["arguments"], '{"path":"notes.txt"}')

    def test_summary_keeps_a_multiline_result_on_one_line(self) -> None:
        self.provider.set_script("config-echo")
        self.provider.post(
            {
                "messages": [
                    USER,
                    asked_for("config-echo"),
                    answered([{"type": "text", "text": "first\nsecond"}]),
                ],
                "tools": [ECHO_TOOL],
            }
        )

        self.assertEqual(
            self.provider.summary("config-echo")["tool_result"], "first\\nsecond"
        )

    def test_other_routes_are_refused_and_not_logged(self) -> None:
        self.provider.set_script("config-echo")

        with self.assertRaises(ProviderRefused) as refused:
            self.provider.post({"messages": [USER]}, path="/v1/embeddings")

        self.assertEqual(refused.exception.status, 404)
        summary = self.provider.summary("config-echo")
        self.assertEqual(summary["requests"], "0")
        self.assertEqual(summary["tools_offered"], "0")

    def test_a_versioned_base_path_is_served(self) -> None:
        self.provider.set_script("config-echo")

        reply = self.provider.post(
            {"messages": [USER], "tools": [ECHO_TOOL]}, path="/v1/chat/completions"
        )

        self.assertEqual(reply["choices"][0]["finish_reason"], "tool_calls")

    def test_script_arguments_must_be_json(self) -> None:
        with self.assertRaises(json.JSONDecodeError):
            self.provider.set_script("config-echo", "{not json")


if __name__ == "__main__":
    unittest.main()
