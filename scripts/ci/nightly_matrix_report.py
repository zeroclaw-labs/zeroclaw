#!/usr/bin/env python3
"""Aggregate nightly matrix lane reports and emit summary artifacts."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sys
from pathlib import Path


def load_owner_map(path: str | None) -> dict[str, str]:
    if not path:
        return {}
    raw = json.loads(Path(path).read_text(encoding="utf-8"))
    owners = raw.get("owners", {})
    if not isinstance(owners, dict):
        raise ValueError("owners file must contain an object at key 'owners'")
    return {str(k): str(v) for k, v in owners.items()}


def build_markdown(report: dict) -> str:
    lines: list[str] = []
    lines.append("# Nightly Feature Matrix Summary")
    lines.append("")
    lines.append(f"- Generated at: `{report['generated_at']}`")
    lines.append(f"- Total lanes: `{report['total']}`")
    lines.append(f"- Passed: `{report['passed']}`")
    lines.append(f"- Failed: `{report['failed']}`")
    lines.append("")

    if not report["rows"]:
        lines.append("No nightly lane result files found.")
        lines.append("")
        return "\n".join(lines)

    lines.append("| Lane | Status | Exit | Duration (s) | Owner | Command |")
    lines.append("| --- | --- | ---:| ---:| --- | --- |")
    for row in report["rows"]:
        lines.append(
            "| "
            f"`{row['lane']}` | "
            f"`{row['status']}` | "
            f"{row['exit_code']} | "
            f"{row['duration_seconds']} | "
            f"`{row['owner'] or 'unassigned'}` | "
            f"`{row['command']}` |"
        )
    lines.append("")

    failed_rows = [row for row in report["rows"] if row["status"] != "success"]
    if failed_rows:
        lines.append("## Failed Lanes")
        for row in failed_rows:
            lines.append(
                f"- `{row['lane']}` failed (exit={row['exit_code']}) owner=`{row['owner'] or 'unassigned'}`"
            )
        lines.append("")

    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description="Aggregate nightly matrix lane result JSON files.")
    parser.add_argument("--input-dir", required=True)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--owners-file", default="")
    parser.add_argument("--fail-on-failure", action="store_true")
    args = parser.parse_args()

    input_dir = Path(args.input_dir).resolve()
    output_json = Path(args.output_json)
    output_md = Path(args.output_md)

    if not input_dir.exists() or not input_dir.is_dir():
        print(f"input dir does not exist: {input_dir}", file=sys.stderr)
        return 2

    owners = load_owner_map(args.owners_file or None)

    rows: list[dict[str, object]] = []
    for path in sorted(input_dir.rglob("nightly-result-*.json")):
        raw = json.loads(path.read_text(encoding="utf-8"))
        lane = str(raw.get("lane", path.stem.replace("nightly-result-", "")))
        status = str(raw.get("status", "unknown"))
        exit_code = int(raw.get("exit_code", 1))
        duration = float(raw.get("duration_seconds", 0.0))
        command = str(raw.get("command", ""))

        rows.append(
            {
                "lane": lane,
                "status": status,
                "exit_code": exit_code,
                "duration_seconds": round(duration, 3),
                "command": command,
                "owner": owners.get(lane, ""),
                "source": path.relative_to(input_dir).as_posix(),
            }
        )

    passed = sum(1 for row in rows if row["status"] == "success")
    failed = len(rows) - passed

    report = {
        "schema_version": "zeroclaw.nightly-matrix.v1",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "input_dir": str(input_dir),
        "total": len(rows),
        "passed": passed,
        "failed": failed,
        "rows": rows,
    }

    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_md.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    output_md.write_text(build_markdown(report), encoding="utf-8")

    if args.fail_on_failure and failed > 0:
        print(f"nightly matrix contains failed lanes: {failed}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
