#!/usr/bin/env python3
"""Validate unsafe debt policy exception governance metadata and expiry policy."""

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
KNOWN_PATTERN_IDS = {
    "unsafe_block",
    "unsafe_fn",
    "unsafe_impl",
    "unsafe_trait",
    "mem_transmute",
    "slice_from_raw_parts",
    "ffi_libc_call",
    "missing_crate_unsafe_guard",
}


def parse_iso_date(raw: str) -> dt.date | None:
    try:
        return dt.date.fromisoformat(raw)
    except ValueError:
        return None


def normalize_path(raw: str) -> str:
    return Path(raw).as_posix().strip().strip("/")


def validate_metadata_fields(
    *,
    kind: str,
    key: str,
    owner: str,
    reason: str,
    ticket: str,
    expires_on: str,
    warnings: list[str],
    violations: list[str],
    today: dt.date,
    warn_days: int,
) -> None:
    if not owner:
        violations.append(f"{kind}: `{key}` is missing required field `owner`.")
    if not reason:
        violations.append(f"{kind}: `{key}` is missing required field `reason`.")
    elif len(reason) < 12:
        violations.append(f"{kind}: `{key}` reason is too short; provide actionable context.")
    if not expires_on:
        violations.append(f"{kind}: `{key}` is missing required field `expires_on`.")
    else:
        parsed = parse_iso_date(expires_on)
        if parsed is None:
            violations.append(
                f"{kind}: `{key}` has invalid `expires_on` date (`{expires_on}`). Use YYYY-MM-DD."
            )
        elif parsed < today:
            violations.append(
                f"{kind}: `{key}` expired on `{expires_on}` and must be removed or renewed."
            )
        elif (parsed - today).days <= warn_days:
            warnings.append(
                f"{kind}: `{key}` expires soon on `{expires_on}`; review renewal/removal."
            )
    if not ticket:
        warnings.append(f"{kind}: `{key}` has no tracking `ticket`.")
    elif not TICKET_RE.fullmatch(ticket):
        warnings.append(f"{kind}: `{key}` ticket `{ticket}` does not match KEY-123 format.")


def build_markdown(report: dict) -> str:
    lines: list[str] = []
    lines.append("# Unsafe Policy Governance Guard")
    lines.append("")
    lines.append(f"- Generated at: `{report['generated_at']}`")
    lines.append(f"- Policy ignore_paths: `{report['ignore_paths']}`")
    lines.append(f"- Policy ignore_pattern_ids: `{report['ignore_pattern_ids']}`")
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


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate unsafe debt policy governance.")
    parser.add_argument("--policy-file", default="scripts/ci/config/unsafe_debt_policy.toml")
    parser.add_argument(
        "--governance-file",
        default=".github/security/unsafe-audit-governance.json",
    )
    parser.add_argument("--warn-days", type=int, default=30)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--fail-on-violation", action="store_true")
    args = parser.parse_args()

    policy_path = Path(args.policy_file)
    governance_path = Path(args.governance_file)

    violations: list[str] = []
    warnings: list[str] = []
    today = dt.datetime.now(dt.timezone.utc).date()

    policy = tomllib.loads(policy_path.read_text(encoding="utf-8"))
    audit = policy.get("audit", {})
    if not isinstance(audit, dict):
        violations.append("unsafe debt policy file must contain [audit] table.")
        audit = {}

    configured_paths_raw = audit.get("ignore_paths", [])
    configured_pattern_ids_raw = audit.get("ignore_pattern_ids", [])
    if not isinstance(configured_paths_raw, list) or not all(
        isinstance(i, str) for i in configured_paths_raw
    ):
        violations.append("[audit].ignore_paths must be an array of strings.")
        configured_paths_raw = []
    if not isinstance(configured_pattern_ids_raw, list) or not all(
        isinstance(i, str) for i in configured_pattern_ids_raw
    ):
        violations.append("[audit].ignore_pattern_ids must be an array of strings.")
        configured_pattern_ids_raw = []

    configured_paths = sorted({normalize_path(v) for v in configured_paths_raw if normalize_path(v)})
    configured_pattern_ids = sorted({str(v).strip() for v in configured_pattern_ids_raw if str(v).strip()})
    for pattern_id in configured_pattern_ids:
        if pattern_id not in KNOWN_PATTERN_IDS:
            violations.append(
                f"pattern_id: `{pattern_id}` in policy ignore_pattern_ids is unknown."
            )

    governance_paths_raw: list[dict] = []
    governance_pattern_ids_raw: list[dict] = []
    if governance_path.exists():
        governance = json.loads(governance_path.read_text(encoding="utf-8"))
        governance_paths_raw = governance.get("ignore_paths", [])
        governance_pattern_ids_raw = governance.get("ignore_pattern_ids", [])
        if not isinstance(governance_paths_raw, list):
            violations.append("governance.ignore_paths must be an array.")
            governance_paths_raw = []
        if not isinstance(governance_pattern_ids_raw, list):
            violations.append("governance.ignore_pattern_ids must be an array.")
            governance_pattern_ids_raw = []
    else:
        violations.append(f"unsafe governance file not found: `{governance_path}`")

    governed_paths: set[str] = set()
    for entry in governance_paths_raw:
        if not isinstance(entry, dict):
            violations.append("governance.ignore_paths entries must be objects.")
            continue
        path_value = normalize_path(str(entry.get("path", "")).strip())
        if not path_value:
            violations.append("path: governance entry is missing required field `path`.")
            continue
        if path_value in governed_paths:
            violations.append(f"path: duplicate governance entry for `{path_value}`.")
        owner = str(entry.get("owner", "")).strip()
        reason = str(entry.get("reason", "")).strip()
        ticket = str(entry.get("ticket", "")).strip()
        expires_on = str(entry.get("expires_on", "")).strip()
        validate_metadata_fields(
            kind="path",
            key=path_value,
            owner=owner,
            reason=reason,
            ticket=ticket,
            expires_on=expires_on,
            warnings=warnings,
            violations=violations,
            today=today,
            warn_days=args.warn_days,
        )
        governed_paths.add(path_value)

    governed_pattern_ids: set[str] = set()
    for entry in governance_pattern_ids_raw:
        if not isinstance(entry, dict):
            violations.append("governance.ignore_pattern_ids entries must be objects.")
            continue
        pattern_id = str(entry.get("pattern_id", "")).strip()
        if not pattern_id:
            violations.append(
                "pattern_id: governance entry is missing required field `pattern_id`."
            )
            continue
        if pattern_id not in KNOWN_PATTERN_IDS:
            violations.append(f"pattern_id: governance entry `{pattern_id}` is unknown.")
        if pattern_id in governed_pattern_ids:
            violations.append(f"pattern_id: duplicate governance entry for `{pattern_id}`.")
        owner = str(entry.get("owner", "")).strip()
        reason = str(entry.get("reason", "")).strip()
        ticket = str(entry.get("ticket", "")).strip()
        expires_on = str(entry.get("expires_on", "")).strip()
        validate_metadata_fields(
            kind="pattern_id",
            key=pattern_id,
            owner=owner,
            reason=reason,
            ticket=ticket,
            expires_on=expires_on,
            warnings=warnings,
            violations=violations,
            today=today,
            warn_days=args.warn_days,
        )
        governed_pattern_ids.add(pattern_id)

    unmanaged_paths = sorted(set(configured_paths) - governed_paths)
    unmanaged_pattern_ids = sorted(set(configured_pattern_ids) - governed_pattern_ids)
    stale_path_governance = sorted(governed_paths - set(configured_paths))
    stale_pattern_governance = sorted(governed_pattern_ids - set(configured_pattern_ids))

    for item in unmanaged_paths:
        violations.append(
            f"path: `{item}` exists in unsafe_debt_policy ignore_paths but has no governance metadata."
        )
    for item in unmanaged_pattern_ids:
        violations.append(
            f"pattern_id: `{item}` exists in unsafe_debt_policy ignore_pattern_ids but has no governance metadata."
        )
    for item in stale_path_governance:
        warnings.append(
            f"path: `{item}` exists in governance metadata but not in unsafe_debt_policy ignore_paths."
        )
    for item in stale_pattern_governance:
        warnings.append(
            f"pattern_id: `{item}` exists in governance metadata but not in unsafe_debt_policy ignore_pattern_ids."
        )

    report = {
        "schema_version": "zeroclaw.audit.v1",
        "event_type": "unsafe_policy_guard",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "policy_file": str(policy_path),
        "governance_file": str(governance_path),
        "ignore_paths": len(configured_paths),
        "ignore_pattern_ids": len(configured_pattern_ids),
        "governance_entries": len(governed_paths) + len(governed_pattern_ids),
        "known_pattern_ids": sorted(KNOWN_PATTERN_IDS),
        "unmanaged_paths": unmanaged_paths,
        "unmanaged_pattern_ids": unmanaged_pattern_ids,
        "stale_path_governance": stale_path_governance,
        "stale_pattern_governance": stale_pattern_governance,
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
        print("unsafe policy governance violations found:", file=sys.stderr)
        for item in violations:
            print(f"- {item}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
