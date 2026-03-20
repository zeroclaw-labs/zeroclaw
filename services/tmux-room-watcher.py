#!/usr/bin/env python3
"""
services/tmux-room-watcher.py

Polls tmux panes for pending questions/prompts and forwards them to the
corresponding Matrix room so the user can respond via chat (which zeroclaw
then routes back to the pane via the 'tmux ' prefix).

Zero-token — no LLM involved.

Config: ~/.zeroclaw/room-bot.json
State:  ~/.zeroclaw/room-bot-state.json

Usage:
  python3 services/tmux-room-watcher.py            # run once
  python3 services/tmux-room-watcher.py --loop     # poll continuously
"""

import hashlib
import json
import os
import subprocess
import sys
import time
import uuid
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

CONFIG_PATH = Path.home() / ".zeroclaw" / "room-bot.json"
STATE_PATH = Path.home() / ".zeroclaw" / "room-bot-state.json"

# DEFAULT_CONFIG is a template only — copy to ~/.zeroclaw/room-bot.json and fill in real values.
# Run `services/setup-matrix.sh` to generate the config automatically.
DEFAULT_CONFIG = {
    "homeserver": "http://localhost:6167",
    "user_id": "@room-bot:example.com",
    "password": "",
    "poll_interval_secs": 30,
    "context_lines": 15,
    # Map tmux pane targets to Matrix room IDs.
    # Room IDs are obtained after running setup-matrix.sh.
    "pane_room_map": {},
}

# Heuristics for detecting that a tmux pane is waiting for user input.
# Checked against the last non-empty visible line.
def _is_question_line(line: str) -> bool:
    s = line.strip()
    lower = s.lower()
    return (
        s.endswith("?")
        or s.endswith("? ")
        or "(y/n)" in lower
        or "(yes/no)" in lower
        or "(y/n/e)" in lower
        or "(y/n/d)" in lower
        or lower.startswith("do you want")
        or lower.startswith("would you like")
        or lower.startswith("shall i")
        or lower.startswith("should i")
        or lower.startswith("can i")
        # Claude Code interactive confirmation prompts
        or s.endswith("[Y/n]")
        or s.endswith("[y/N]")
        or s == ">"
        or s == "?"
    )


def load_config() -> dict:
    if CONFIG_PATH.exists():
        return json.loads(CONFIG_PATH.read_text())
    return DEFAULT_CONFIG


def load_state() -> dict:
    if STATE_PATH.exists():
        try:
            return json.loads(STATE_PATH.read_text())
        except (json.JSONDecodeError, OSError):
            pass
    return {"access_token": None, "posted": {}}


def save_state(state: dict) -> None:
    STATE_PATH.write_text(json.dumps(state, indent=2))


def matrix_request(method: str, url: str, token: str = None, body: dict = None) -> dict:
    headers = {"Content-Type": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"HTTP {e.code}: {e.read().decode()[:200]}")


def login(config: dict) -> str:
    resp = matrix_request(
        "POST",
        f"{config['homeserver']}/_matrix/client/v3/login",
        body={
            "type": "m.login.password",
            "user": config["user_id"],
            "password": config["password"],
        },
    )
    return resp["access_token"]


def ensure_joined(homeserver: str, token: str, room_id: str) -> bool:
    """Try to join the room if not already in it. Returns True if joined."""
    encoded = urllib.parse.quote(room_id, safe="")
    try:
        matrix_request(
            "POST",
            f"{homeserver}/_matrix/client/v3/join/{encoded}",
            token=token,
            body={},
        )
        return True
    except RuntimeError as e:
        if "already" in str(e).lower() or "M_FORBIDDEN" in str(e):
            return False
        print(f"  Warning: could not join {room_id}: {e}")
        return False


def capture_pane(target: str) -> str | None:
    result = subprocess.run(
        ["tmux", "capture-pane", "-p", "-t", target],
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        return None
    return result.stdout


def find_pending_question(pane_output: str, context_lines: int = 15) -> str | None:
    """
    Returns a context string if the pane appears to be waiting for user input,
    otherwise None.
    """
    if not pane_output:
        return None
    lines = pane_output.rstrip().splitlines()
    non_empty = [l for l in lines if l.strip()]
    if not non_empty:
        return None

    last = non_empty[-1]
    if not _is_question_line(last):
        return None

    # Return the last `context_lines` non-empty lines as context
    context = "\n".join(non_empty[-context_lines:])
    return context


def post_to_room(homeserver: str, token: str, room_id: str, text: str) -> None:
    txn_id = uuid.uuid4().hex
    encoded = urllib.parse.quote(room_id, safe="")
    matrix_request(
        "PUT",
        f"{homeserver}/_matrix/client/v3/rooms/{encoded}/send/m.room.message/{txn_id}",
        token=token,
        body={"msgtype": "m.text", "body": text},
    )


def content_hash(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()[:20]


def poll(config: dict, state: dict) -> None:
    homeserver = config["homeserver"]

    # Login if no token cached
    if not state.get("access_token"):
        print("Logging in as room-bot...")
        try:
            state["access_token"] = login(config)
            save_state(state)
        except RuntimeError as e:
            print(f"Login failed: {e}")
            return

    token = state["access_token"]
    posted: dict = state.setdefault("posted", {})
    changed = False

    for pane, room_id in config["pane_room_map"].items():
        output = capture_pane(pane)
        if output is None:
            print(f"  {pane}: not found")
            continue

        question = find_pending_question(output, config.get("context_lines", 15))
        if not question:
            # Clear posted state for this room when pane is no longer waiting
            if room_id in posted:
                del posted[room_id]
                changed = True
            continue

        chash = content_hash(question)
        if posted.get(room_id) == chash:
            print(f"  {pane}: already posted, skipping")
            continue

        msg = f"Pending input needed in `{pane}`:\n\n```\n{question}\n```\n\nReply with `tmux <your answer>` to respond."
        try:
            ensure_joined(homeserver, token, room_id)
            post_to_room(homeserver, token, room_id, msg)
            posted[room_id] = chash
            changed = True
            print(f"  {pane} → {room_id}: posted")
        except RuntimeError as e:
            err = str(e)
            if "401" in err or "M_UNKNOWN_TOKEN" in err:
                print(f"  {pane}: token expired, will re-login next cycle")
                state["access_token"] = None
            else:
                print(f"  {pane}: post failed: {e}")

    if changed:
        save_state(state)


def main() -> None:
    loop_mode = "--loop" in sys.argv
    config = load_config()
    state = load_state()

    if loop_mode:
        interval = config.get("poll_interval_secs", 30)
        print(f"tmux-room-watcher running (interval: {interval}s, panes: {list(config['pane_room_map'])})")
        while True:
            try:
                poll(config, state)
            except Exception as e:
                print(f"Poll error: {e}")
            time.sleep(interval)
    else:
        poll(config, state)


if __name__ == "__main__":
    main()
