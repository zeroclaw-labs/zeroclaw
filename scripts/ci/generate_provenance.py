#!/usr/bin/env python3
"""Generate an in-toto/SLSA-style provenance statement for a built artifact."""

from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
from pathlib import Path


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate provenance statement for artifact.")
    parser.add_argument("--artifact", required=True)
    parser.add_argument("--subject-name", default="zeroclaw")
    parser.add_argument("--output", required=True)
    args = parser.parse_args()

    artifact = Path(args.artifact)
    digest = sha256_file(artifact)
    now = dt.datetime.now(dt.timezone.utc).isoformat()

    statement = {
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{"name": args.subject_name, "digest": {"sha256": digest}}],
        "predicateType": "https://slsa.dev/provenance/v1",
        "predicate": {
            "buildDefinition": {
                "buildType": "https://zeroclaw.dev/ci/release-fast",
                "externalParameters": {
                    "repository": os.getenv("GITHUB_REPOSITORY", ""),
                    "ref": os.getenv("GITHUB_REF", ""),
                    "workflow": os.getenv("GITHUB_WORKFLOW", ""),
                },
                "internalParameters": {
                    "sha": os.getenv("GITHUB_SHA", ""),
                    "run_id": os.getenv("GITHUB_RUN_ID", ""),
                    "run_attempt": os.getenv("GITHUB_RUN_ATTEMPT", ""),
                },
                "resolvedDependencies": [],
            },
            "runDetails": {
                "builder": {
                    "id": f"https://github.com/{os.getenv('GITHUB_REPOSITORY', '')}/actions/runs/{os.getenv('GITHUB_RUN_ID', '')}"
                },
                "metadata": {
                    "invocationId": os.getenv("GITHUB_RUN_ID", ""),
                    "startedOn": now,
                    "finishedOn": now,
                },
            },
        },
    }

    out = Path(args.output)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(statement, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
