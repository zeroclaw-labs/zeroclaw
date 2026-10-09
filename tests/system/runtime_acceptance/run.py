#!/usr/bin/env python3
"""Run bounded application scenarios and publish only sanitized evidence."""

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import shutil
import signal
import subprocess
import sys
import time
import xml.etree.ElementTree as ET

from scenarios import SCENARIOS


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", choices=("core", "full"), required=True)
    parser.add_argument("--bin-dir", type=Path, required=True)
    parser.add_argument("--artifacts-dir", type=Path, required=True)
    parser.add_argument("--fault", choices=("initialize", "response", "sop"), help="negative-control run; must fail")
    parser.add_argument("--timeout", type=float, default=60, help="per-operation deadline in seconds")
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("--timeout must be a positive finite number")
    args.bin_dir = args.bin_dir.resolve()
    args.artifacts_dir = args.artifacts_dir.resolve()
    args.artifacts_dir.mkdir(parents=True, exist_ok=True)
    selected = [(tier, test) for tier, test in SCENARIOS if args.suite == "full" or tier == "core"]
    if args.fault:
        target = "sop_persistence" if args.fault == "sop" else "warm_startup"
        selected = [(tier, test) for tier, test in selected if test.__name__ == target]
    report = {"suite": args.suite, "fault": args.fault, "platform": platform.platform(),
              "expected": [test.__name__ for _, test in selected], "scenarios": [], "binaries": {}}
    started = time.monotonic()
    failed = False
    try:
        if not selected:
            raise RuntimeError("no scenarios selected")
        if platform.system() != "Linux":
            raise RuntimeError("application acceptance requires Linux")
        if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
            raise RuntimeError("application cleanup requires Python 3.9+ and Linux pidfd support")
        for tool in ("tmux", "openssl"):
            if not shutil.which(tool):
                raise RuntimeError("required test dependency is missing: " + tool)
        for name in ("zeroclaw", "zerocode"):
            binary = args.bin_dir / name
            if not os.access(binary, os.X_OK):
                raise RuntimeError("missing executable: " + str(binary))
            digest = hashlib.sha256()
            with binary.open("rb") as stream:
                for block in iter(lambda: stream.read(1024 * 1024), b""):
                    digest.update(block)
            report["binaries"][name] = digest.hexdigest()
        report["commit"] = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True, timeout=10).strip()

        def expired(_signum, _frame):
            raise TimeoutError("suite execution budget exceeded")

        signal.signal(signal.SIGALRM, expired)
        signal.alarm(300 if args.suite == "core" else 600)
        for tier, test in selected:
            entry = {"name": test.__name__, "tier": tier, "status": "failed"}
            case_started = time.monotonic()
            try:
                test(args, args.artifacts_dir / test.__name__)
                entry["status"] = "passed"
            except Exception as error:
                # Scenario helpers redact credential-bearing application errors.
                entry["error"] = f"{type(error).__name__}: {error}"
                failed = True
            entry["seconds"] = round(time.monotonic() - case_started, 3)
            report["scenarios"].append(entry)
            print(json.dumps(entry), flush=True)
            if failed:
                break
        if len(report["scenarios"]) != len(selected):
            failed = True
    except Exception as error:
        report["error"] = str(error)
        failed = True
    finally:
        signal.alarm(0)
        report["seconds"] = round(time.monotonic() - started, 3)
        report["status"] = "failed" if failed else "passed"
        (args.artifacts_dir / "results.json").write_text(json.dumps(report, indent=2) + "\n")
        suite = ET.Element("testsuite", name="application-" + args.suite, tests=str(len(selected)),
                           failures=str(len(selected) - sum(entry["status"] == "passed" for entry in report["scenarios"])),
                           errors=str(int("error" in report)), time=str(report["seconds"]))
        seen = {entry["name"]: entry for entry in report["scenarios"]}
        for _, test in selected:
            entry = seen.get(test.__name__)
            case = ET.SubElement(suite, "testcase", name=test.__name__, time=str((entry or {}).get("seconds", 0)))
            if not entry or entry["status"] != "passed":
                ET.SubElement(case, "failure").text = (entry or {}).get("error", "selected scenario did not execute")
        ET.ElementTree(suite).write(args.artifacts_dir / "junit.xml", encoding="utf-8", xml_declaration=True)
        summary = os.environ.get("GITHUB_STEP_SUMMARY")
        if summary and not args.fault:
            with open(summary, "a") as stream:
                stream.write(f"\n### Application acceptance: {args.suite}\n\nCommit: `{report.get('commit', 'unavailable')}`\n\n")
                stream.write("| Scenario | Result | Seconds |\n| --- | --- | --- |\n")
                for entry in report["scenarios"]:
                    stream.write(f"| {entry['name']} | {entry['status']} | {entry['seconds']} |\n")
                stream.write(f"\nTotal: {report['seconds']} seconds; {report['status']}.\n")
        if "error" in report:
            print(report["error"], file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
