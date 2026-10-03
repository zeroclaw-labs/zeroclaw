#!/usr/bin/env python3
"""Prove that real application failures cannot turn into successful smoke runs."""

import argparse
import json
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin-dir", required=True)
    parser.add_argument("--artifacts-dir", type=Path, required=True)
    args = parser.parse_args()
    for fault, expected in (("initialize", "ZeroCode"), ("response", "REPLY_WARM"), ("sop", "SOP completion")):
        artifacts = args.artifacts_dir / fault
        result = subprocess.run([sys.executable, str(Path(__file__).with_name("run.py")), "--suite", "core",
                                 "--bin-dir", args.bin_dir, "--artifacts-dir", str(artifacts),
                                 "--fault", fault, "--timeout", "8"], timeout=90)
        report = json.loads((artifacts / "results.json").read_text())
        errors = " ".join(row.get("error", "") for row in report["scenarios"])
        if result.returncode != 1 or expected not in errors:
            raise AssertionError(f"negative control {fault} did not fail at its intended boundary: {errors}")
        print(f"Negative control {fault}: detected", flush=True)


if __name__ == "__main__":
    main()
