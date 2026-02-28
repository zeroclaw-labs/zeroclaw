#!/usr/bin/env python3
"""Wrap workflow artifacts into a normalized audit event envelope."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(description="Emit normalized audit event envelope.")
    parser.add_argument("--event-type", required=True)
    parser.add_argument("--input-json", required=True)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--artifact-name", default="")
    parser.add_argument("--retention-days", type=int, default=0)
    args = parser.parse_args()

    payload = json.loads(Path(args.input_json).read_text(encoding="utf-8"))
    event = {
        "schema_version": "zeroclaw.audit.v1",
        "event_type": args.event_type,
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "run_context": {
            "repository": os.getenv("GITHUB_REPOSITORY", ""),
            "workflow": os.getenv("GITHUB_WORKFLOW", ""),
            "run_id": os.getenv("GITHUB_RUN_ID", ""),
            "run_attempt": os.getenv("GITHUB_RUN_ATTEMPT", ""),
            "sha": os.getenv("GITHUB_SHA", ""),
            "ref": os.getenv("GITHUB_REF", ""),
            "actor": os.getenv("GITHUB_ACTOR", ""),
        },
        "payload": payload,
    }
    if args.artifact_name or args.retention_days > 0:
        artifact_meta: dict[str, object] = {}
        if args.artifact_name:
            artifact_meta["name"] = args.artifact_name
        if args.retention_days > 0:
            artifact_meta["retention_days"] = args.retention_days
        event["artifact"] = artifact_meta

    out = Path(args.output_json)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(event, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
