"""Run against a signed app bundle. The optional --deliver flag requests a real banner."""
import argparse
import json
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("app", type=Path)
parser.add_argument("--deliver", action="store_true")
parser.add_argument("--origin", default="http://127.0.0.1:42617")
parser.add_argument("--agent", default="operator")
parser.add_argument("--session", help="existing dashboard conversation ID owned by this agent")
args = parser.parse_args()
binary = args.app / "Contents/MacOS/zeroclaw-notify"

def call(method, ident, **params):
    return json.dumps({"jsonrpc": "2.0", "method": method, "id": ident, "params": params})

def tool(ident, **fields):
    return call("tools/call", ident, name="system_notify",
                arguments={"title": "ZeroClaw", "body": "Open the operator conversation.", **fields})

frames = ["{", call("initialize", 1, protocolVersion="2025-06-18"),
          json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}),
          call("tools/list", 2), tool(3, session_id="../invalid")]
if args.deliver:
    import uuid
    event = uuid.uuid4().hex
    destination = {"session_id": args.session} if args.session else {}
    frames += [tool(i, event_id=event, **destination) for i in (4, 5)]
proc = subprocess.run([binary, "serve", args.origin, args.agent], input="\n".join(frames)+"\n",
                      capture_output=True, text=True, check=True, timeout=20)
rows = {row["id"]: row for row in map(json.loads, proc.stdout.splitlines())}
assert rows[None]["error"]["code"] == -32700
assert rows[1]["result"]["protocolVersion"] == "2025-06-18"
assert rows[2]["result"]["tools"][0]["name"] == "system_notify"
assert rows[3]["result"]["isError"]
print("Passed MCP initialization, discovery, malformed frame recovery, and invalid session handling.")
if args.deliver:
    for ident in (4, 5):
        print(rows[ident]["result"]["content"][0]["text"])
    if rows[4]["result"].get("isError"):
        raise SystemExit(2)
    assert rows[5]["result"]["content"][0]["text"] == "Already delivered."
