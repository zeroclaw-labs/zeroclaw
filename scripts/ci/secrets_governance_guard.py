#!/usr/bin/env python3
"""Validate gitleaks allowlist governance metadata and expiry policy."""

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


TICKET_RE = re.compile(r"^[A-Z][A-Z0-9]+-\d+$")


def parse_iso_date(raw: str) -> dt.date | None:
    try:
        return dt.date.fromisoformat(raw)
    except ValueError:
        return None


def likely_overbroad_pattern(pattern: str) -> bool:
    compact = pattern.strip()
    if compact in {".*", ".+"}:
        return True
    if compact.startswith(".*") and "/" not in compact:
        return True
    if compact.count(".*") >= 3:
        return True
    return False


def build_markdown(report: dict) -> str:
    lines: list[str] = []
    lines.append("# Secrets Governance Guard")
    lines.append("")
    lines.append(f"- Generated at: `{report['generated_at']}`")
    lines.append(f"- Gitleaks allowlist paths: `{report['allowlist_paths']}`")
    lines.append(f"- Gitleaks allowlist regexes: `{report['allowlist_regexes']}`")
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
    if not report["violations"] and not report["warnings"]:
        lines.append("No governance issues found.")
        lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def validate_metadata_entry(
    *,
    kind: str,
    entry: dict,
    warnings: list[str],
    violations: list[str],
    today: dt.date,
    warn_days: int,
) -> str:
    pattern = str(entry.get("pattern", "")).strip()
    owner = str(entry.get("owner", "")).strip()
    reason = str(entry.get("reason", "")).strip()
    expires_on = str(entry.get("expires_on", "")).strip()
    ticket = str(entry.get("ticket", "")).strip()

    if not pattern:
        violations.append(f"{kind}: metadata entry is missing required field `pattern`.")
        return ""
    if not owner:
        violations.append(f"{kind}: `{pattern}` is missing required field `owner`.")
    if not reason:
        violations.append(f"{kind}: `{pattern}` is missing required field `reason`.")
    elif len(reason) < 12:
        violations.append(
            f"{kind}: `{pattern}` reason is too short; provide actionable context."
        )
    if not expires_on:
        violations.append(f"{kind}: `{pattern}` is missing required field `expires_on`.")
    else:
        parsed = parse_iso_date(expires_on)
        if parsed is None:
            violations.append(
                f"{kind}: `{pattern}` has invalid `expires_on` date (`{expires_on}`). Use YYYY-MM-DD."
            )
        else:
            if parsed < today:
                violations.append(
                    f"{kind}: `{pattern}` expired on `{expires_on}` and must be removed or renewed."
                )
            elif (parsed - today).days <= warn_days:
                warnings.append(
                    f"{kind}: `{pattern}` expires soon on `{expires_on}`; review renewal/removal."
                )
    if not ticket:
        warnings.append(f"{kind}: `{pattern}` has no tracking `ticket`.")
    elif not TICKET_RE.fullmatch(ticket):
        warnings.append(f"{kind}: `{pattern}` ticket `{ticket}` does not match KEY-123 format.")
    if likely_overbroad_pattern(pattern):
        violations.append(f"{kind}: `{pattern}` appears over-broad and must be narrowed.")

    return pattern


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate gitleaks allowlist governance policy.")
    parser.add_argument("--gitleaks-file", default=".gitleaks.toml")
    parser.add_argument(
        "--governance-file",
        default=".github/security/gitleaks-allowlist-governance.json",
    )
    parser.add_argument("--warn-days", type=int, default=21)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--fail-on-violation", action="store_true")
    args = parser.parse_args()

    gitleaks_path = Path(args.gitleaks_file)
    governance_path = Path(args.governance_file)

    gitleaks = tomllib.loads(gitleaks_path.read_text(encoding="utf-8"))
    governance = json.loads(governance_path.read_text(encoding="utf-8"))

    allowlist = gitleaks.get("allowlist", {})
    configured_paths = [str(v) for v in allowlist.get("paths", []) if str(v).strip()]
    configured_regexes = [str(v) for v in allowlist.get("regexes", []) if str(v).strip()]

    governance_paths = governance.get("paths", [])
    governance_regexes = governance.get("regexes", [])

    warnings: list[str] = []
    violations: list[str] = []
    today = dt.datetime.now(dt.timezone.utc).date()

    governed_path_patterns: set[str] = set()
    if not isinstance(governance_paths, list):
        violations.append("governance.paths must be an array.")
        governance_paths = []
    for entry in governance_paths:
        if not isinstance(entry, dict):
            violations.append("governance.paths entries must be objects.")
            continue
        pattern = validate_metadata_entry(
            kind="path",
            entry=entry,
            warnings=warnings,
            violations=violations,
            today=today,
            warn_days=args.warn_days,
        )
        if pattern:
            governed_path_patterns.add(pattern)

    governed_regex_patterns: set[str] = set()
    if not isinstance(governance_regexes, list):
        violations.append("governance.regexes must be an array.")
        governance_regexes = []
    for entry in governance_regexes:
        if not isinstance(entry, dict):
            violations.append("governance.regexes entries must be objects.")
            continue
        pattern = validate_metadata_entry(
            kind="regex",
            entry=entry,
            warnings=warnings,
            violations=violations,
            today=today,
            warn_days=args.warn_days,
        )
        if pattern:
            governed_regex_patterns.add(pattern)

    unmanaged_paths = sorted(set(configured_paths) - governed_path_patterns)
    unmanaged_regexes = sorted(set(configured_regexes) - governed_regex_patterns)
    stale_path_governance = sorted(governed_path_patterns - set(configured_paths))
    stale_regex_governance = sorted(governed_regex_patterns - set(configured_regexes))

    for pattern in unmanaged_paths:
        violations.append(
            f"path: `{pattern}` exists in .gitleaks.toml allowlist but has no governance metadata."
        )
    for pattern in unmanaged_regexes:
        violations.append(
            f"regex: `{pattern}` exists in .gitleaks.toml allowlist but has no governance metadata."
        )
    for pattern in stale_path_governance:
        warnings.append(
            f"path: `{pattern}` exists in governance metadata but not in current .gitleaks.toml allowlist."
        )
    for pattern in stale_regex_governance:
        warnings.append(
            f"regex: `{pattern}` exists in governance metadata but not in current .gitleaks.toml allowlist."
        )

    report = {
        "schema_version": "zeroclaw.audit.v1",
        "event_type": "secrets_governance_guard",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "gitleaks_file": str(gitleaks_path),
        "governance_file": str(governance_path),
        "allowlist_paths": len(configured_paths),
        "allowlist_regexes": len(configured_regexes),
        "governance_entries": len(governed_path_patterns) + len(governed_regex_patterns),
        "unmanaged_paths": unmanaged_paths,
        "unmanaged_regexes": unmanaged_regexes,
        "stale_path_governance": stale_path_governance,
        "stale_regex_governance": stale_regex_governance,
        "warnings": warnings,
        "violations": violations,
    }

    output_json = Path(args.output_json)
    output_md = Path(args.output_md)
    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_md.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    output_md.write_text(build_markdown(report), encoding="utf-8")

    if args.fail_on_violation and violations:
        print("secrets governance violations found:", file=sys.stderr)
        for item in violations:
            print(f"- {item}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
