#!/usr/bin/env python3
"""Validate pre-release stage transitions and tag integrity."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
import sys
from pathlib import Path

STABLE_TAG_RE = re.compile(r"^v(?P<version>\d+\.\d+\.\d+)$")
PRERELEASE_TAG_RE = re.compile(
    r"^v(?P<version>\d+\.\d+\.\d+)-(?P<stage>alpha|beta|rc)\.(?P<number>\d+)$"
)
STAGE_RANK = {"alpha": 1, "beta": 2, "rc": 3, "stable": 4}


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


def parse_tag(tag: str) -> tuple[str, str, int | None]:
    stable_match = STABLE_TAG_RE.fullmatch(tag)
    if stable_match:
        return (stable_match.group("version"), "stable", None)

    pre_match = PRERELEASE_TAG_RE.fullmatch(tag)
    if pre_match:
        return (
            pre_match.group("version"),
            pre_match.group("stage"),
            int(pre_match.group("number")),
        )

    raise ValueError(
        f"Tag `{tag}` must be `vX.Y.Z` or `vX.Y.Z-(alpha|beta|rc).N` (for example `v0.2.0-rc.1`)."
    )


def build_markdown(report: dict) -> str:
    lines: list[str] = []
    lines.append("# Pre-release Guard Report")
    lines.append("")
    lines.append(f"- Generated at: `{report['generated_at']}`")
    lines.append(f"- Tag: `{report['tag']}`")
    lines.append(f"- Stage: `{report['stage']}`")
    lines.append(f"- Mode: `{report['mode']}`")
    lines.append(f"- Ready to publish: `{report['ready_to_publish']}`")
    lines.append("")

    lines.append("## Required Checks")
    required_checks = report.get("required_checks", [])
    if required_checks:
        for check_name in required_checks:
            lines.append(f"- `{check_name}`")
    else:
        lines.append("- none configured")
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

    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate release tag stage gating.")
    parser.add_argument("--repo-root", default=".")
    parser.add_argument("--tag", required=True)
    parser.add_argument("--stage-config-file", required=True)
    parser.add_argument("--mode", choices=("dry-run", "publish"), default="dry-run")
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--fail-on-violation", action="store_true")
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    out_json = Path(args.output_json)
    out_md = Path(args.output_md)

    policy = json.loads(Path(args.stage_config_file).read_text(encoding="utf-8"))
    required_prev = policy.get("required_previous_stage", {})
    required_checks = policy.get("required_checks", {})

    violations: list[str] = []
    warnings: list[str] = []

    try:
        version, stage, stage_number = parse_tag(args.tag)
    except ValueError as exc:
        print(str(exc), file=sys.stderr)
        return 2

    try:
        run_git(["fetch", "--quiet", "origin", "main", "--tags"], cwd=repo_root)
    except RuntimeError as exc:
        warnings.append(f"Failed to refresh origin refs/tags before validation: {exc}")

    try:
        tag_sha = run_git(["rev-parse", f"{args.tag}^{{commit}}"], cwd=repo_root)
    except RuntimeError as exc:
        violations.append(f"Unable to resolve tag `{args.tag}`: {exc}")
        tag_sha = ""

    if tag_sha:
        proc = subprocess.run(
            ["git", "merge-base", "--is-ancestor", tag_sha, "origin/main"],
            cwd=str(repo_root),
            text=True,
            capture_output=True,
            check=False,
        )
        if proc.returncode != 0:
            violations.append(
                f"Tag `{args.tag}` ({tag_sha}) is not reachable from `origin/main`; prerelease tags must originate from main."
            )

    tags_text = run_git(["tag", "--list", f"v{version}*"], cwd=repo_root)
    sibling_tags = [line.strip() for line in tags_text.splitlines() if line.strip() and line.strip() != args.tag]

    sibling_stages: list[str] = []
    for candidate in sibling_tags:
        try:
            _, sibling_stage, _ = parse_tag(candidate)
        except ValueError:
            continue
        sibling_stages.append(sibling_stage)

    prerequisite_stage = required_prev.get(stage)
    if prerequisite_stage:
        if prerequisite_stage not in sibling_stages:
            violations.append(
                f"Stage `{stage}` requires at least one `{prerequisite_stage}` tag for version `{version}` before publishing `{args.tag}`."
            )

    if sibling_stages:
        highest_sibling_rank = max(STAGE_RANK.get(s, 0) for s in sibling_stages)
        current_rank = STAGE_RANK.get(stage, 0)
        if highest_sibling_rank > current_rank:
            violations.append(
                f"Higher stage tags already exist for `{version}`. Refusing stage regression to `{stage}`."
            )

    cargo_version = ""
    if tag_sha:
        try:
            cargo_toml = run_git(["show", f"{args.tag}:Cargo.toml"], cwd=repo_root)
            for line in cargo_toml.splitlines():
                line = line.strip()
                if line.startswith("version = "):
                    cargo_version = line.split('"', 2)[1]
                    break
        except RuntimeError as exc:
            violations.append(f"Failed to inspect Cargo.toml at `{args.tag}`: {exc}")

    if cargo_version and cargo_version != version:
        violations.append(
            f"Tag `{args.tag}` version `{version}` does not match Cargo.toml version `{cargo_version}` at the same ref."
        )

    ready_to_publish = args.mode == "publish" and not violations

    report = {
        "schema_version": "zeroclaw.prerelease-guard.v1",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "policy_schema_version": policy.get("schema_version"),
        "tag": args.tag,
        "tag_sha": tag_sha or None,
        "version": version,
        "stage": stage,
        "stage_number": stage_number,
        "mode": args.mode,
        "ready_to_publish": ready_to_publish,
        "required_checks": required_checks.get(stage, []),
        "sibling_tags": sibling_tags,
        "warnings": warnings,
        "violations": violations,
    }

    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    out_md.write_text(build_markdown(report), encoding="utf-8")

    if args.fail_on_violation and violations:
        print("prerelease guard violations found:", file=sys.stderr)
        for item in violations:
            print(f"- {item}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
