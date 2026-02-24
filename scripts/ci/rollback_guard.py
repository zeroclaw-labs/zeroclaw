#!/usr/bin/env python3
"""Build and validate a rollback execution plan for CI/CD incidents."""

from __future__ import annotations

import argparse
import datetime as dt
import fnmatch
import json
import subprocess
import sys
from pathlib import Path


def run_git(args: list[str], *, cwd: Path) -> str:
    proc = subprocess.run(
        ["git", *args],
        cwd=str(cwd),
        text=True,
        capture_output=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} failed ({proc.returncode}): {proc.stderr.strip()}")
    return proc.stdout.strip()


def resolve_target_ref(
    *,
    repo_root: Path,
    target_ref: str,
    tag_pattern: str,
) -> tuple[str | None, str | None]:
    if target_ref:
        sha = run_git(["rev-parse", f"{target_ref}^{{commit}}"], cwd=repo_root)
        return (target_ref, sha)

    # Prefer semantic version ordering for deterministic rollback target selection.
    refs = run_git(["tag", "--list", tag_pattern, "--sort=-version:refname"], cwd=repo_root)
    for ref in refs.splitlines():
        if not ref:
            continue
        try:
            sha = run_git(["rev-parse", f"{ref}^{{commit}}"], cwd=repo_root)
        except RuntimeError:
            continue
        return (ref, sha)

    # Fallback for non-semver tag names.
    fallback_refs = run_git(
        [
            "for-each-ref",
            "--sort=-creatordate",
            "--format=%(refname:short)",
            "refs/tags",
        ],
        cwd=repo_root,
    )
    for ref in fallback_refs.splitlines():
        if not ref or not fnmatch.fnmatch(ref, tag_pattern):
            continue
        try:
            sha = run_git(["rev-parse", f"{ref}^{{commit}}"], cwd=repo_root)
        except RuntimeError:
            continue
        return (ref, sha)
    return (None, None)


def build_markdown(report: dict) -> str:
    lines: list[str] = []
    lines.append("# Rollback Guard Plan")
    lines.append("")
    lines.append(f"- Generated at: `{report['generated_at']}`")
    lines.append(f"- Branch: `{report['branch']}`")
    lines.append(f"- Mode: `{report['mode']}`")
    lines.append(f"- Current head: `{report['current_head_sha']}`")
    lines.append(f"- Target ref: `{report['target_ref'] or 'n/a'}`")
    lines.append(f"- Target sha: `{report['target_sha'] or 'n/a'}`")
    lines.append(f"- Ancestor check: `{report['ancestor_check']}`")
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

    lines.append("## Plan")
    lines.append(f"- Rollback strategy: `{report['strategy']}`")
    lines.append(f"- Allow non-ancestor target: `{report['allow_non_ancestor']}`")
    lines.append(f"- Ready to execute: `{report['ready_to_execute']}`")
    lines.append("")
    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate rollback target and emit rollback execution plan.")
    parser.add_argument("--repo-root", default=".")
    parser.add_argument("--branch", default="dev")
    parser.add_argument("--mode", choices=("dry-run", "execute"), default="dry-run")
    parser.add_argument("--strategy", default="latest-release-tag")
    parser.add_argument("--target-ref", default="")
    parser.add_argument("--tag-pattern", default="v*")
    parser.add_argument("--allow-non-ancestor", action="store_true")
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--fail-on-violation", action="store_true")
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    out_json = Path(args.output_json)
    out_md = Path(args.output_md)

    warnings: list[str] = []
    violations: list[str] = []

    try:
        current_head_sha = run_git(["rev-parse", "HEAD"], cwd=repo_root)
    except RuntimeError as exc:
        print(str(exc), file=sys.stderr)
        return 2

    try:
        target_ref, target_sha = resolve_target_ref(
            repo_root=repo_root,
            target_ref=args.target_ref.strip(),
            tag_pattern=args.tag_pattern,
        )
    except RuntimeError as exc:
        target_ref, target_sha = (args.target_ref.strip() or None, None)
        violations.append(f"Failed to resolve rollback target: {exc}")
    if not target_sha:
        violations.append(
            "Rollback target could not be resolved; provide `--target-ref` or ensure matching tags exist."
        )

    ancestor_check = "unknown"
    if target_sha:
        proc = subprocess.run(
            ["git", "merge-base", "--is-ancestor", target_sha, current_head_sha],
            cwd=str(repo_root),
            text=True,
            capture_output=True,
            check=False,
        )
        if proc.returncode == 0:
            ancestor_check = "pass"
        elif proc.returncode == 1:
            ancestor_check = "fail"
            msg = (
                f"Target `{target_ref}` ({target_sha}) is not an ancestor of current head "
                f"`{current_head_sha}`."
            )
            if args.allow_non_ancestor:
                warnings.append(msg)
            else:
                violations.append(msg)
        else:
            ancestor_check = "error"
            violations.append(f"Unable to evaluate ancestor relation: {proc.stderr.strip()}")

        if target_sha == current_head_sha:
            warnings.append("Target SHA matches current head; rollback is a no-op.")

    ready_to_execute = args.mode == "execute" and not violations

    report = {
        "schema_version": "zeroclaw.audit.v1",
        "event_type": "rollback_guard",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repo_root": str(repo_root),
        "branch": args.branch,
        "mode": args.mode,
        "strategy": args.strategy,
        "tag_pattern": args.tag_pattern,
        "target_ref": target_ref,
        "target_sha": target_sha,
        "current_head_sha": current_head_sha,
        "ancestor_check": ancestor_check,
        "allow_non_ancestor": args.allow_non_ancestor,
        "ready_to_execute": ready_to_execute,
        "warnings": warnings,
        "violations": violations,
    }

    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    out_md.write_text(build_markdown(report), encoding="utf-8")

    if args.fail_on_violation and violations:
        print("rollback guard violations found:", file=sys.stderr)
        for item in violations:
            print(f"- {item}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
