#!/usr/bin/env python3
"""Validate deny.toml policy hygiene for advisory ignore exceptions."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import sys
from pathlib import Path

try:
    import tomllib  # Python 3.11+
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib  # type: ignore

TICKET_RE = re.compile(r"^[A-Z]+-\d+$")


def parse_iso_date(raw: str) -> dt.date | None:
    try:
        return dt.date.fromisoformat(raw)
    except ValueError:
        return None


def build_markdown(report: dict) -> str:
    lines: list[str] = []
    lines.append("# deny.toml Policy Guard")
    lines.append("")
    lines.append(f"- Generated at: `{report['generated_at']}`")
    lines.append(f"- Ignore entries: `{report['ignore_count']}`")
    lines.append(f"- Governance entries: `{report['governance_entries']}`")
    lines.append(f"- Violations: `{len(report['violations'])}`")
    lines.append(f"- Warnings: `{len(report['warnings'])}`")
    lines.append("")
    if report["violations"]:
        lines.append("## Violations")
        for item in report["violations"]:
            lines.append(f"- {item}")
        lines.append("")
    if report["warnings"]:
        lines.append("## Warnings")
        for item in report["warnings"]:
            lines.append(f"- {item}")
        lines.append("")
    if not report["violations"]:
        lines.append("No policy violations found.")
    lines.append("")
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate deny.toml advisory ignore policy.")
    parser.add_argument("--deny-file", default="deny.toml")
    parser.add_argument("--governance-file", default=".github/security/deny-ignore-governance.json")
    parser.add_argument("--warn-days", type=int, default=30)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--fail-on-violation", action="store_true")
    args = parser.parse_args()

    deny_path = Path(args.deny_file)
    content = tomllib.loads(deny_path.read_text(encoding="utf-8"))
    advisories = content.get("advisories", {})
    ignore = advisories.get("ignore", [])

    violations: list[str] = []
    warnings: list[str] = []
    normalized: list[dict] = []
    today = dt.date.today()

    if not isinstance(ignore, list):
        violations.append("`advisories.ignore` must be a list.")
        ignore = []

    governance_path = Path(args.governance_file)
    governance_map: dict[str, dict] = {}
    if governance_path.exists():
        governance_raw = json.loads(governance_path.read_text(encoding="utf-8"))
        governance_entries = governance_raw.get("advisories", [])
        if not isinstance(governance_entries, list):
            violations.append("deny governance file: `advisories` must be a list.")
            governance_entries = []
        for idx, entry in enumerate(governance_entries):
            if not isinstance(entry, dict):
                violations.append(f"deny governance advisory[{idx}] must be an object.")
                continue
            adv_id = str(entry.get("id", "")).strip()
            owner = str(entry.get("owner", "")).strip()
            reason = str(entry.get("reason", "")).strip()
            ticket = str(entry.get("ticket", "")).strip()
            expires_on = str(entry.get("expires_on", "")).strip()

            if not adv_id:
                violations.append(f"deny governance advisory[{idx}] missing required field `id`.")
                continue
            if adv_id in governance_map:
                violations.append(f"deny governance contains duplicate advisory id `{adv_id}`.")
            if not owner:
                violations.append(f"deny governance `{adv_id}` missing required field `owner`.")
            if not reason:
                violations.append(f"deny governance `{adv_id}` missing required field `reason`.")
            elif len(reason) < 12:
                violations.append(
                    f"deny governance `{adv_id}` reason is too short; provide actionable context."
                )
            if not expires_on:
                violations.append(f"deny governance `{adv_id}` missing required field `expires_on`.")
                parsed_expires = None
            else:
                parsed_expires = parse_iso_date(expires_on)
                if parsed_expires is None:
                    violations.append(
                        f"deny governance `{adv_id}` has invalid `expires_on` (`{expires_on}`); "
                        "use YYYY-MM-DD."
                    )
                elif parsed_expires < today:
                    violations.append(
                        f"deny governance `{adv_id}` expired on `{expires_on}`; renew or remove ignore."
                    )
                elif parsed_expires <= (today + dt.timedelta(days=max(0, args.warn_days))):
                    warnings.append(
                        f"deny governance `{adv_id}` expires soon on `{expires_on}`; schedule review."
                    )
            if not ticket:
                warnings.append(f"deny governance `{adv_id}` missing tracking `ticket`.")
            elif not TICKET_RE.fullmatch(ticket):
                warnings.append(
                    f"deny governance `{adv_id}` ticket `{ticket}` does not match KEY-123 format."
                )

            governance_map[adv_id] = {
                "id": adv_id,
                "owner": owner,
                "reason": reason,
                "ticket": ticket,
                "expires_on": expires_on,
            }
    else:
        violations.append(f"deny governance file not found: `{governance_path}`")

    ignore_ids: set[str] = set()
    for idx, entry in enumerate(ignore):
        if isinstance(entry, str):
            violations.append(
                f"ignore[{idx}] uses legacy string format (`{entry}`); use table form with `id` + `reason`."
            )
            normalized.append({"id": entry, "reason": "", "legacy": True})
            continue

        if not isinstance(entry, dict):
            violations.append(f"ignore[{idx}] must be a table/object.")
            continue

        adv_id = str(entry.get("id", "")).strip()
        reason = str(entry.get("reason", "")).strip()
        expires = str(entry.get("expires", "")).strip()
        if not adv_id:
            violations.append(f"ignore[{idx}] is missing required field `id`.")
        if not reason:
            violations.append(f"ignore[{idx}] (`{adv_id or 'unknown'}`) is missing required field `reason`.")
        elif len(reason) < 12:
            violations.append(
                f"ignore[{idx}] (`{adv_id or 'unknown'}`) reason is too short; provide actionable mitigation context."
            )

        normalized.append({"id": adv_id, "reason": reason, "expires": expires, "legacy": False})
        if adv_id:
            ignore_ids.add(adv_id)
            if adv_id not in governance_map:
                violations.append(
                    f"ignore[{idx}] (`{adv_id}`) has no governance metadata in `{governance_path}`."
                )

    stale_governance = sorted([adv_id for adv_id in governance_map if adv_id not in ignore_ids])
    for adv_id in stale_governance:
        warnings.append(
            f"deny governance entry `{adv_id}` exists but advisory is not currently ignored in deny.toml."
        )

    report = {
        "schema_version": "zeroclaw.audit.v1",
        "event_type": "deny_policy_guard",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "deny_file": str(deny_path),
        "governance_file": str(governance_path),
        "ignore_count": len(normalized),
        "governance_entries": len(governance_map),
        "unmanaged_ignores": sorted([item["id"] for item in normalized if item.get("id") and item["id"] not in governance_map]),
        "stale_governance": stale_governance,
        "warnings": warnings,
        "violations": violations,
        "ignores": normalized,
        "governance": [governance_map[k] for k in sorted(governance_map)],
    }

    json_path = Path(args.output_json)
    md_path = Path(args.output_md)
    json_path.parent.mkdir(parents=True, exist_ok=True)
    md_path.parent.mkdir(parents=True, exist_ok=True)
    json_path.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    md_path.write_text(build_markdown(report), encoding="utf-8")

    if args.fail_on_violation and violations:
        print("deny policy violations found:", file=sys.stderr)
        for item in violations:
            print(f"- {item}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
