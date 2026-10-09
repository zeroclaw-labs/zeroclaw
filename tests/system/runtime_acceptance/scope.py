#!/usr/bin/env python3
"""Conservative application-suite selection from the actual PR event and diff."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess


ROOT = Path(__file__).resolve().parents[3]
HIGH_RISK = {"risk:high", "domain:security", "priority:p0", "priority:p1"}
SENSITIVE = ("src/", "crates/zeroclaw-config/", "crates/zeroclaw-api/", "crates/zeroclaw-gateway/",
             "crates/zeroclaw-tls/", "crates/zeroclaw-runtime/src/security/", "crates/zeroclaw-runtime/src/daemon/",
             "crates/zeroclaw-runtime/src/rpc/", "crates/zeroclaw-runtime/src/live_config", "apps/zerocode/",
             ".github/", ".cargo/", "scripts/ci/", "tests/system/runtime_acceptance/", "dev/ci")


def classify(paths, labels, event):
    if event != "pull_request":
        return "full", "non-PR event"
    if not isinstance(paths, list) or not paths or not all(isinstance(p, str) and p for p in paths):
        return "full", "missing or invalid changed paths"
    if not isinstance(labels, list) or not all(isinstance(label, str) for label in labels):
        return "full", "missing or invalid labels"
    if HIGH_RISK.intersection(labels):
        return "full", "risk, security, or urgent priority label"
    mode = "skip"
    for path in paths:
        parts = Path(path).parts
        if path.startswith("/") or ".." in parts:
            return "full", "invalid changed path"
        if path.startswith(SENSITIVE) or Path(path).name in ("Cargo.toml", "Cargo.lock", "rust-toolchain", "rust-toolchain.toml"):
            return "full", "application boundary, dependencies, or CI infrastructure changed"
        if re.search(r"(?:auth|oidc|principal|transport|daemon)", path, re.I):
            return "full", "authentication or transport surface changed"
        if ((path.startswith("docs/") and path.endswith((".md", ".svg", ".png", ".jpg")))
                or path in ("README.md", "CHANGELOG.md", "LICENSE", "NOTICE")):
            continue
        # Package existence comes from the checkout, not a second crate registry.
        known_crate = len(parts) >= 3 and parts[0] == "crates" and (ROOT / parts[0] / parts[1] / "Cargo.toml").is_file()
        if (known_crate and path.endswith(".rs")) or path.startswith(("tests/", "web/")):
            mode = "core"
        else:
            return "full", "unclassified changed path"
    return mode, "documentation and metadata only" if mode == "skip" else "ordinary code changes"


def from_event(event_name, payload):
    if event_name != "pull_request":
        return classify(None, None, event_name)
    try:
        pr = payload["pull_request"]
        base = pr["base"]["sha"]
        if not isinstance(base, str) or not re.fullmatch(r"[0-9a-f]{40}", base):
            raise ValueError("invalid base SHA")
        labels = [label["name"] for label in pr["labels"]]
        changed = subprocess.check_output(["git", "diff", "--no-renames", "--name-only", "-z", base, "HEAD"], cwd=ROOT, timeout=30)
        paths = changed.decode("utf-8").rstrip("\0").split("\0")
        return classify(paths, labels, event_name)
    except (KeyError, TypeError, ValueError, OSError, subprocess.SubprocessError):
        return "full", "event or diff could not be classified"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--event-name", default=os.environ.get("GITHUB_EVENT_NAME", "workflow_dispatch"))
    parser.add_argument("--event-path", default=os.environ.get("GITHUB_EVENT_PATH"))
    args = parser.parse_args()
    try:
        payload = json.loads(Path(args.event_path).read_text()) if args.event_path else {}
    except (OSError, ValueError):
        payload = {}
    mode, reason = from_event(args.event_name, payload)
    print(json.dumps({"suite": mode, "reason": reason}))
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"suite={mode}\nreason={reason}\n")
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as output:
            output.write(f"\nApplication acceptance selection: **{mode}** ({reason}).\n")


if __name__ == "__main__":
    main()
