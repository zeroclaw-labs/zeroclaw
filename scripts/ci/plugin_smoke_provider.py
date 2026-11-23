#!/usr/bin/env python3

"""Scripted OpenAI-compatible provider for the plugin artifact smoke.

`zeroclaw` executes a plugin tool when a model asks for it during an agent
turn; no command runs one directly. This server stands in for the model, so
the smoke drives the operator's real path without credentials or network
access.

serve
    Answers POST <any>/chat/completions. While the conversation holds no tool
    result, it asks for one call to the first offered tool whose name contains
    the scripted match, passing the scripted arguments. Otherwise it answers
    with a fixed final text. The script file is read again on every request,
    so one server serves every smoke case. Each request body is appended to
    the log as one JSON line. The server stops when the stop file appears.

script
    Writes the script file `serve` reads.

summarize
    Reads a log written by `serve` and prints what the host did as
    `key=value` lines for a shell caller to assert on: how many requests
    arrived, the largest tool list any of them offered, whether a tool
    matching the script was among them, and the tool results the host sent
    back. A result is printed on one line: backslashes are doubled and line
    breaks become `\\r` and `\\n`.

one-line
    Prints a text with the encoding `summarize` applies to a result, so a
    caller can compare an expected result with the summary byte for byte.
"""

from __future__ import annotations

import argparse
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from socketserver import TCPServer
from typing import Any, Optional

TURN_COMPLETE = "SMOKE_TURN_COMPLETE"
TOOL_NOT_OFFERED = "SMOKE_TOOL_NOT_OFFERED"
CALL_ID = "call_plugin_smoke"


class _LiteralHTTPServer(ThreadingHTTPServer):
    """Use the bound identity; this fixture never consumes a reverse name."""

    def server_bind(self) -> None:
        TCPServer.server_bind(self)
        self.server_name, self.server_port = self.server_address[:2]


def offered_tool_names(body: dict[str, Any]) -> list[str]:
    names = []
    for tool in body.get("tools") or []:
        name = (tool.get("function") or {}).get("name")
        if isinstance(name, str):
            names.append(name)
    return names


def matching_tool(body: dict[str, Any], tool_match: str) -> Optional[str]:
    if not tool_match:
        return None
    return next((name for name in offered_tool_names(body) if tool_match in name), None)


def tool_results(body: dict[str, Any]) -> list[str]:
    results = []
    for message in body.get("messages") or []:
        if message.get("role") != "tool":
            continue
        content = message.get("content")
        if isinstance(content, list):
            content = "".join(
                part.get("text", "") for part in content if isinstance(part, dict)
            )
        results.append(content if isinstance(content, str) else "")
    return results


def completion(body: dict[str, Any], script: dict[str, str]) -> dict[str, Any]:
    target = matching_tool(body, script.get("tool_match", ""))
    if tool_results(body):
        message: dict[str, Any] = {"role": "assistant", "content": TURN_COMPLETE}
        finish_reason = "stop"
    elif target is None:
        message = {"role": "assistant", "content": TOOL_NOT_OFFERED}
        finish_reason = "stop"
    else:
        message = {
            "role": "assistant",
            "content": None,
            "tool_calls": [
                {
                    "id": CALL_ID,
                    "type": "function",
                    "function": {
                        "name": target,
                        "arguments": script.get("tool_arguments", "{}"),
                    },
                }
            ],
        }
        finish_reason = "tool_calls"
    return {
        "id": "chatcmpl-plugin-smoke",
        "object": "chat.completion",
        "model": body.get("model", "plugin-smoke"),
        "choices": [{"index": 0, "message": message, "finish_reason": finish_reason}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
    }


def read_script(path: Path) -> dict[str, str]:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {}


def make_handler(log: Path, script: Path) -> type[BaseHTTPRequestHandler]:
    write_lock = threading.Lock()

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, format: str, *args: Any) -> None:
            return

        def reply(self, status: int, payload: dict[str, Any]) -> None:
            encoded = json.dumps(payload).encode("utf-8")
            self.send_response(status)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

        def do_POST(self) -> None:
            length = int(self.headers.get("content-length") or 0)
            raw = self.rfile.read(length)
            if not self.path.rstrip("/").endswith("/chat/completions"):
                self.reply(404, {"error": {"message": f"no route for {self.path}"}})
                return
            try:
                body = json.loads(raw or b"{}")
            except json.JSONDecodeError as error:
                self.reply(400, {"error": {"message": f"request is not JSON: {error}"}})
                return
            with write_lock, log.open("a", encoding="utf-8") as handle:
                handle.write(json.dumps({"path": self.path, "body": body}) + "\n")
            self.reply(200, completion(body, read_script(script)))

    return Handler


def startup_milestone(stage: str, started: Optional[float]) -> Optional[float]:
    try:
        now = time.monotonic()
        if started is None:
            started = now
        print(
            f"PROVIDER_STARTUP monotonic={now:.6f} elapsed={now - started:.6f} stage={stage}",
            file=sys.stderr,
            flush=True,
        )
    except Exception:
        # Diagnostic computation/emission must not abort normal startup.
        pass
    return started


def startup_exception(phase: str, started: Optional[float], error: Exception) -> None:
    try:
        if started is None:
            return
        now = time.monotonic()
        name = type(error).__name__[:80]
        if not (name.isascii() and name.replace("_", "").isalnum()):
            name = "Exception"
        value = error.errno if isinstance(error, OSError) else None
        # A non-OS exception has no errno; -1 records that uncertainty numerically.
        errno = value if type(value) is int else -1
        print(
            f"PROVIDER_STARTUP_ERROR monotonic={now:.6f} elapsed={now - started:.6f} "
            f"phase={phase} type={name} errno={errno}",
            file=sys.stderr,
            flush=True,
        )
    except Exception:
        # Diagnostic failure must not replace the original startup exception.
        pass


def serve(args: argparse.Namespace) -> int:
    started = None
    phase = "serve-entry"
    try:
        started = startup_milestone("serve-entry", started)
        phase = "server-construction"
        started = startup_milestone("server-construction-start", started)
        server = _LiteralHTTPServer((args.host, 0), make_handler(args.log, args.script))
        started = startup_milestone("server-construction-done", started)
        phase = "log-touch"
        args.log.touch()
        started = startup_milestone("log-touched", started)
        phase = "port-publication"
        args.port_file.write_text(str(server.server_address[1]), encoding="utf-8")
        started = startup_milestone("port-published", started)
        phase = "worker-start"
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        started = startup_milestone("worker-started", started)
    except Exception as error:
        startup_exception(phase, started, error)
        raise
    try:
        while not args.stop_file.exists():
            time.sleep(0.1)
    except KeyboardInterrupt:
        pass
    server.shutdown()
    server.server_close()
    return 0


def write_script(args: argparse.Namespace) -> int:
    json.loads(args.tool_arguments)
    args.file.write_text(
        json.dumps(
            {"tool_match": args.tool_match, "tool_arguments": args.tool_arguments}
        ),
        encoding="utf-8",
    )
    return 0


def one_line(text: str) -> str:
    return text.replace("\\", "\\\\").replace("\r", "\\r").replace("\n", "\\n")


def print_one_line(args: argparse.Namespace) -> int:
    print(one_line(args.text))
    return 0


def summarize(args: argparse.Namespace) -> int:
    requests = 0
    tools_offered = 0
    tool_name = ""
    results: list[str] = []
    for line in args.log.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        body = json.loads(line)["body"]
        requests += 1
        tools_offered = max(tools_offered, len(offered_tool_names(body)))
        tool_name = tool_name or matching_tool(body, args.tool_match) or ""
        # Each request replays the conversation so far; the last one is complete.
        results = tool_results(body)
    print(f"requests={requests}")
    print(f"tools_offered={tools_offered}")
    print(f"offered={'yes' if tool_name else 'no'}")
    print(f"tool_name={one_line(tool_name)}")
    print(f"tool_results={len(results)}")
    print(f"tool_result={one_line(results[-1]) if results else ''}")
    return 0


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    commands = parser.add_subparsers(dest="command", required=True)

    serve_parser = commands.add_parser("serve")
    serve_parser.add_argument("--host", default="127.0.0.1")
    serve_parser.add_argument("--port-file", type=Path, required=True)
    serve_parser.add_argument("--log", type=Path, required=True)
    serve_parser.add_argument("--script", type=Path, required=True)
    serve_parser.add_argument("--stop-file", type=Path, required=True)
    serve_parser.set_defaults(run=serve)

    script_parser = commands.add_parser("script")
    script_parser.add_argument("--file", type=Path, required=True)
    script_parser.add_argument("--tool-match", required=True)
    script_parser.add_argument("--tool-arguments", required=True)
    script_parser.set_defaults(run=write_script)

    summarize_parser = commands.add_parser("summarize")
    summarize_parser.add_argument("--log", type=Path, required=True)
    summarize_parser.add_argument("--tool-match", required=True)
    summarize_parser.set_defaults(run=summarize)

    one_line_parser = commands.add_parser("one-line")
    one_line_parser.add_argument("--text", required=True)
    one_line_parser.set_defaults(run=print_one_line)

    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    return args.run(args)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
