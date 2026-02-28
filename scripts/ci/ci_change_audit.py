#!/usr/bin/env python3
"""Generate a CI/CD change audit report for GitHub workflows and CI scripts.

The report is designed for change-control traceability and light policy checks:
- enumerate changed CI-related files
- summarize line churn
- capture newly introduced action references
- detect unpinned `uses:` action references
- detect risky pipe-to-shell commands (e.g. `curl ... | sh`)
- detect newly introduced `pull_request_target` triggers in supported YAML forms
- detect broad `permissions: write-all` grants
- detect newly introduced `${{ secrets.* }}` references
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable


AUDIT_PREFIXES = (
    ".github/workflows/",
    ".github/release/",
    ".github/actions/",
    ".github/codeql/",
    "scripts/ci/",
    ".githooks/",
)
AUDIT_FILES = {
    ".github/dependabot.yml",
    "deny.toml",
    ".gitleaks.toml",
}
WORKFLOW_PATH_PREFIXES = (
    ".github/workflows/",
    ".github/release/",
    ".github/actions/",
    ".github/codeql/",
)
WORKFLOW_EXTENSIONS = (".yml", ".yaml")
SHELL_EXTENSIONS = (".sh", ".bash")
USES_RE = re.compile(r"^\+\s*(?:-\s*)?uses:\s*([^\s#]+)")
SECRETS_RE = re.compile(r"\$\{\{\s*secrets\.([A-Za-z0-9_]+)\s*}}")
SHA_PIN_RE = re.compile(r"^[0-9a-f]{40}$")
PIPE_TO_SHELL_RE = re.compile(r"\b(?:curl|wget)\b.*\|\s*(?:sh|bash)\b")
PERMISSION_WRITE_RE = re.compile(r"^\+\s*([a-z-]+):\s*write\s*$")
PERMISSIONS_WRITE_ALL_RE = re.compile(r"^\+\s*permissions\s*:\s*write-all\s*$", re.IGNORECASE)


def line_adds_pull_request_target(added_text: str) -> bool:
    # Support the three common YAML forms:
    # 1) pull_request_target:
    # 2) - pull_request_target
    # 3) on: [push, pull_request_target]
    normalized = added_text.split("#", 1)[0].strip().lower()
    if not normalized:
        return False
    if normalized.startswith("pull_request_target:"):
        return True
    if normalized == "- pull_request_target":
        return True
    if normalized.startswith("on:") and "[" in normalized and "]" in normalized:
        return "pull_request_target" in normalized
    return False


def run(cmd: list[str]) -> str:
    proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        raise RuntimeError(f"Command failed ({proc.returncode}): {' '.join(cmd)}\n{proc.stderr}")
    return proc.stdout


def is_ci_path(path: str) -> bool:
    return path in AUDIT_FILES or path.startswith(AUDIT_PREFIXES)


def is_workflow_yaml_path(path: str) -> bool:
    return path.startswith(WORKFLOW_PATH_PREFIXES) and path.endswith(WORKFLOW_EXTENSIONS)


def is_shell_path(path: str) -> bool:
    return path.endswith(SHELL_EXTENSIONS) or path.startswith(".githooks/")


@dataclass
class FileAudit:
    path: str
    status: str
    added: int = 0
    deleted: int = 0
    added_actions: list[str] = field(default_factory=list)
    unpinned_actions: list[str] = field(default_factory=list)
    added_secret_refs: list[str] = field(default_factory=list)
    added_pipe_to_shell: list[str] = field(default_factory=list)
    added_write_permissions: list[str] = field(default_factory=list)
    added_pull_request_target: int = 0

    @property
    def risk_level(self) -> str:
        if (
            self.unpinned_actions
            or self.added_pipe_to_shell
            or self.added_pull_request_target
            or "write-all" in self.added_write_permissions
        ):
            return "high"
        if self.added_secret_refs or self.added_actions or self.added_write_permissions:
            return "medium"
        return "low"


def parse_changed_files(base_sha: str, head_sha: str) -> list[tuple[str, str]]:
    out = run(["git", "diff", "--name-status", "--find-renames", base_sha, head_sha])
    changed: list[tuple[str, str]] = []
    for line in out.splitlines():
        if not line.strip():
            continue
        parts = line.split("\t")
        status = parts[0]
        path = parts[-1]
        changed.append((status, path))
    return changed


def parse_numstat(base_sha: str, head_sha: str, path: str) -> tuple[int, int]:
    out = run(["git", "diff", "--numstat", base_sha, head_sha, "--", path]).strip()
    if not out:
        return (0, 0)
    parts = out.split("\t")
    if len(parts) < 3:
        return (0, 0)
    add_raw, del_raw = parts[0], parts[1]
    try:
        added = 0 if add_raw == "-" else int(add_raw)
        deleted = 0 if del_raw == "-" else int(del_raw)
    except ValueError:
        return (0, 0)
    return (added, deleted)


def parse_patch_added_lines(base_sha: str, head_sha: str, path: str) -> Iterable[str]:
    out = run(["git", "diff", "-U0", base_sha, head_sha, "--", path])
    for line in out.splitlines():
        if not line.startswith("+") or line.startswith("+++"):
            continue
        yield line


def action_is_pinned(action_ref: str) -> bool:
    if action_ref.startswith("./"):
        return True
    if "@" not in action_ref:
        return False
    version = action_ref.rsplit("@", 1)[1]
    return bool(SHA_PIN_RE.fullmatch(version))


def build_markdown(
    audits: list[FileAudit],
    *,
    base_sha: str,
    head_sha: str,
    violations: list[str],
) -> str:
    lines: list[str] = []
    lines.append("# CI/CD Change Audit")
    lines.append("")
    lines.append(f"- Base SHA: `{base_sha}`")
    lines.append(f"- Head SHA: `{head_sha}`")
    lines.append(f"- Audited files: `{len(audits)}`")
    lines.append(
        f"- Policy violations: `{len(violations)}` "
        "(currently: unpinned `uses:`, pipe-to-shell commands, broad "
        "`permissions: write-all`, and new `pull_request_target` triggers)"
    )
    lines.append("")

    if violations:
        lines.append("## Violations")
        for entry in violations:
            lines.append(f"- {entry}")
        lines.append("")

    if not audits:
        lines.append("No CI/CD files changed in this diff.")
        return "\n".join(lines) + "\n"

    lines.append("## File Summary")
    lines.append("")
    lines.append(
        "| Path | Status | +Lines | -Lines | New Actions | New Secret Refs | "
        "Pipe-to-Shell | New `*: write` | New `pull_request_target` | Risk |"
    )
    lines.append("| --- | --- | ---:| ---:| ---:| ---:| ---:| ---:| ---:| --- |")
    for audit in sorted(audits, key=lambda x: x.path):
        lines.append(
            f"| `{audit.path}` | `{audit.status}` | {audit.added} | {audit.deleted} | "
            f"{len(audit.added_actions)} | {len(audit.added_secret_refs)} | "
            f"{len(audit.added_pipe_to_shell)} | {len(set(audit.added_write_permissions))} | "
            f"{audit.added_pull_request_target} | "
            f"`{audit.risk_level}` |"
        )
    lines.append("")

    medium_or_high = [a for a in audits if a.risk_level in {"medium", "high"}]
    if medium_or_high:
        lines.append("## Detailed Review Targets")
        for audit in sorted(medium_or_high, key=lambda x: x.path):
            lines.append(f"### `{audit.path}`")
            if audit.added_actions:
                lines.append("- Added `uses:` references:")
                for action in audit.added_actions:
                    pin_state = "pinned" if action not in audit.unpinned_actions else "unpinned"
                    lines.append(f"  - `{action}` ({pin_state})")
            if audit.added_secret_refs:
                lines.append("- Added secret references:")
                for secret_key in sorted(set(audit.added_secret_refs)):
                    lines.append(f"  - `secrets.{secret_key}`")
            if audit.added_pipe_to_shell:
                lines.append("- Added pipe-to-shell commands (high risk):")
                for cmd in audit.added_pipe_to_shell:
                    lines.append(f"  - `{cmd}`")
            if audit.added_write_permissions:
                lines.append("- Added write permissions:")
                for permission_name in sorted(set(audit.added_write_permissions)):
                    if permission_name == "write-all":
                        lines.append("  - `permissions: write-all`")
                    else:
                        lines.append(f"  - `{permission_name}: write`")
            if audit.added_pull_request_target:
                lines.append("- Added `pull_request_target` trigger (high risk):")
                lines.append("  - Review event payload usage and token scope carefully.")
            lines.append("")

    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate CI/CD change audit report.")
    parser.add_argument("--base-sha", required=True, help="Base commit SHA")
    parser.add_argument("--head-sha", default="HEAD", help="Head commit SHA (default: HEAD)")
    parser.add_argument("--output-json", required=True, help="Output JSON path")
    parser.add_argument("--output-md", required=True, help="Output Markdown path")
    parser.add_argument(
        "--fail-on-violations",
        action="store_true",
        help="Return non-zero when policy violations are found",
    )
    args = parser.parse_args()

    try:
        changed = parse_changed_files(args.base_sha, args.head_sha)
    except RuntimeError as exc:
        print(str(exc), file=sys.stderr)
        return 2

    audits: list[FileAudit] = []
    violations: list[str] = []
    for status, path in changed:
        if not is_ci_path(path):
            continue

        added, deleted = parse_numstat(args.base_sha, args.head_sha, path)
        audit = FileAudit(path=path, status=status, added=added, deleted=deleted)
        workflow_yaml = is_workflow_yaml_path(path)
        shell_script = is_shell_path(path)

        for line in parse_patch_added_lines(args.base_sha, args.head_sha, path):
            added_text = line[1:].strip()

            uses_match = USES_RE.search(line)
            if uses_match and workflow_yaml:
                action_ref = uses_match.group(1).strip()
                audit.added_actions.append(action_ref)
                if not action_is_pinned(action_ref):
                    audit.unpinned_actions.append(action_ref)
                    violations.append(
                        f"{path}: unpinned action reference introduced -> `{action_ref}`"
                    )

            for secret_name in SECRETS_RE.findall(line):
                audit.added_secret_refs.append(secret_name)

            if PIPE_TO_SHELL_RE.search(added_text) and (workflow_yaml or shell_script):
                command = added_text[:220]
                audit.added_pipe_to_shell.append(command)
                violations.append(
                    f"{path}: pipe-to-shell command introduced -> `{command}`"
                )

            permission_match = PERMISSION_WRITE_RE.match(line)
            if permission_match and workflow_yaml:
                audit.added_write_permissions.append(permission_match.group(1))
            if PERMISSIONS_WRITE_ALL_RE.match(line) and workflow_yaml:
                audit.added_write_permissions.append("write-all")
                violations.append(
                    f"{path}: `permissions: write-all` introduced; scope permissions minimally."
                )

            if line_adds_pull_request_target(added_text) and workflow_yaml:
                audit.added_pull_request_target += 1
                violations.append(
                    f"{path}: `pull_request_target` trigger introduced -> `{added_text[:180]}`; "
                    "manual security review required."
                )

        audits.append(audit)

    summary = {
        "total_changed_files": len(changed),
        "audited_files": len(audits),
        "added_lines": sum(a.added for a in audits),
        "deleted_lines": sum(a.deleted for a in audits),
        "new_actions": sum(len(a.added_actions) for a in audits),
        "new_unpinned_actions": sum(len(a.unpinned_actions) for a in audits),
        "new_secret_references": sum(len(a.added_secret_refs) for a in audits),
        "new_pipe_to_shell_commands": sum(len(a.added_pipe_to_shell) for a in audits),
        "new_write_permissions": sum(len(set(a.added_write_permissions)) for a in audits),
        "new_pull_request_target_triggers": sum(a.added_pull_request_target for a in audits),
        "violations": len(violations),
    }
    payload = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "base_sha": args.base_sha,
        "head_sha": args.head_sha,
        "summary": summary,
        "files": [
            {
                "path": a.path,
                "status": a.status,
                "added": a.added,
                "deleted": a.deleted,
                "added_actions": a.added_actions,
                "unpinned_actions": a.unpinned_actions,
                "added_secret_refs": sorted(set(a.added_secret_refs)),
                "added_pipe_to_shell": a.added_pipe_to_shell,
                "added_write_permissions": sorted(set(a.added_write_permissions)),
                "added_pull_request_target": a.added_pull_request_target,
                "risk_level": a.risk_level,
            }
            for a in sorted(audits, key=lambda x: x.path)
        ],
        "violations": violations,
    }

    json_path = Path(args.output_json)
    md_path = Path(args.output_md)
    json_path.parent.mkdir(parents=True, exist_ok=True)
    md_path.parent.mkdir(parents=True, exist_ok=True)
    json_path.write_text(json.dumps(payload, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    md_path.write_text(
        build_markdown(audits, base_sha=args.base_sha, head_sha=args.head_sha, violations=violations),
        encoding="utf-8",
    )

    if args.fail_on_violations and violations:
        print("CI/CD change audit violations found:", file=sys.stderr)
        for item in violations:
            print(f"- {item}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
