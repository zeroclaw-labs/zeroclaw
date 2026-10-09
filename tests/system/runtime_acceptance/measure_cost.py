#!/usr/bin/env python3
"""Opt-in paired build measurement on a disposable Linux CI runner.

The existing and proposed Cargo commands run on the same source and machine,
restoring the identical target snapshot before EACH build. ABBA order reduces
order bias. Downloads happen before measurement. Nothing writes shared caches.
"""

import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import tempfile
import time

from scope import ROOT

TARGET = "x86_64-unknown-linux-gnu"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts-dir", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get("GITHUB_ACTIONS") != "true" or platform.system() != "Linux":
        parser.error("run only in a disposable Linux Actions checkout")
    artifacts = args.artifacts_dir.resolve()
    artifacts.mkdir(parents=True, exist_ok=True)
    target = ROOT / "target"
    metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=ROOT, text=True, timeout=30))
    if target.is_symlink() or Path(metadata["target_directory"]) != target:
        parser.error("measurement requires the checkout's ordinary, non-symlink target directory")
    report = {"commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
              "platform": platform.platform(), "cpus": os.cpu_count(), "cache_hit": os.environ.get("RUST_CACHE_HIT", "unknown"),
              "target": TARGET, "builds": [], "status": "failed"}
    started = time.monotonic()

    def run(command, log, timeout):
        began = time.monotonic()
        with (artifacts / log).open("w") as output:
            subprocess.run(command, cwd=ROOT, stdout=output, stderr=subprocess.STDOUT, check=True, timeout=timeout)
        return round(time.monotonic() - began, 3)

    try:
        (ROOT / "web/dist").mkdir(parents=True, exist_ok=True)
        (ROOT / "web/dist/.gitkeep").touch()
        report["fetch_seconds"] = run(["cargo", "fetch", "--locked", "--target", TARGET], "fetch.log", 300)
        report["rustc"] = subprocess.check_output(["rustc", "--version"], text=True, timeout=10).strip()
        target.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="acceptance-cost-", dir=os.environ.get("RUNNER_TEMP")) as temporary:
            temporary = Path(temporary)
            seed = temporary / "seed"
            binaries = temporary / "bin"
            binaries.mkdir()
            target.rename(seed)
            try:
                for index, kind in enumerate(("baseline", "candidate", "candidate", "baseline"), 1):
                    if target.exists():
                        shutil.rmtree(target)
                    subprocess.run(["cp", "-a", "--reflink=auto", str(seed), str(target)], check=True, timeout=180)
                    command = ["cargo", "build", "--profile", "ci", "--locked", "--target", TARGET]
                    if kind == "candidate":
                        command += ["-p", "zeroclaw", "-p", "zerocode", "--bins"]
                    duration = run(command, f"build-{index}-{kind}.log", 900)
                    entry = {"kind": kind, "seconds": duration, "command": command}
                    report["builds"].append(entry)
                    print(json.dumps(entry), flush=True)
                    if kind == "candidate":
                        for name in ("zeroclaw", "zerocode"):
                            shutil.copy2(target / TARGET / "ci" / name, binaries / name)
                for suite in ("core", "full"):
                    run(["bash", "scripts/ci/runtime_acceptance.sh", "--suite", suite, "--bin-dir", str(binaries),
                         "--artifacts-dir", str(artifacts / suite)], suite + ".log", 660)
                    report[suite + "_seconds"] = json.loads((artifacts / suite / "results.json").read_text())["seconds"]
                report["fault_seconds"] = run(["python3", "tests/system/runtime_acceptance/test_faults.py", "--bin-dir", str(binaries),
                                               "--artifacts-dir", str(artifacts / "faults")], "faults.log", 120)
            finally:
                if target.exists():
                    shutil.rmtree(target)
                seed.rename(target)
        report["baseline_seconds"] = statistics.mean(r["seconds"] for r in report["builds"] if r["kind"] == "baseline")
        report["candidate_seconds"] = statistics.mean(r["seconds"] for r in report["builds"] if r["kind"] == "candidate")
        report["added_build_seconds"] = round(report["candidate_seconds"] - report["baseline_seconds"], 3)
        report["status"] = "passed"
    finally:
        report["total_seconds"] = round(time.monotonic() - started, 3)
        (artifacts / "measurement.json").write_text(json.dumps(report, indent=2) + "\n")
        summary = os.environ.get("GITHUB_STEP_SUMMARY")
        if summary:
            with open(summary, "a") as stream:
                stream.write("\n### Paired acceptance cost measurement\n\nSame source, runner, target path and restored cache for each build; ABBA order.\n\n")
                stream.write("| Build | Seconds |\n| --- | --- |\n")
                for entry in report["builds"]:
                    stream.write(f"| {entry['kind']} | {entry['seconds']} |\n")
                stream.write("\n```json\n" + json.dumps(report, indent=2) + "\n```\n")


if __name__ == "__main__":
    main()
