#!/usr/bin/env python3
"""Validate docs deployment promotion/rollback contract and emit traceability report."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
import sys
from pathlib import Path

POLICY_SCHEMA = "zeroclaw.docs-deploy-policy.v1"
ALLOWED_DEPLOY_TARGETS = {"preview", "production"}


def run_git(repo_root: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        cwd=str(repo_root),
        text=True,
        capture_output=True,
        check=False,
    )


def load_policy(path: Path) -> tuple[dict[str, object], list[str]]:
    violations: list[str] = []
    raw = json.loads(path.read_text(encoding="utf-8"))

    def ensure_string(name: str) -> str:
        value = raw.get(name)
        if not isinstance(value, str) or not value.strip():
            violations.append(f"Policy field `{name}` must be a non-empty string.")
            return ""
        return value.strip()

    def ensure_bool(name: str) -> bool:
        value = raw.get(name)
        if not isinstance(value, bool):
            violations.append(f"Policy field `{name}` must be a boolean.")
            return False
        return value

    def ensure_positive_int(name: str) -> int:
        value = raw.get(name)
        if not isinstance(value, int) or value <= 0:
            violations.append(f"Policy field `{name}` must be a positive integer.")
            return 0
        return value

    schema_version = ensure_string("schema_version")
    if schema_version and schema_version != POLICY_SCHEMA:
        violations.append(f"Policy schema_version must be `{POLICY_SCHEMA}`, got `{schema_version}`.")

    policy = {
        "schema_version": schema_version,
        "production_branch": ensure_string("production_branch"),
        "allow_manual_production_dispatch": ensure_bool("allow_manual_production_dispatch"),
        "require_preview_evidence_on_manual_production": ensure_bool(
            "require_preview_evidence_on_manual_production"
        ),
        "allow_manual_rollback_dispatch": ensure_bool("allow_manual_rollback_dispatch"),
        "rollback_ref_must_be_ancestor_of_production_branch": ensure_bool(
            "rollback_ref_must_be_ancestor_of_production_branch"
        ),
        "docs_preview_retention_days": ensure_positive_int("docs_preview_retention_days"),
        "docs_guard_artifact_retention_days": ensure_positive_int("docs_guard_artifact_retention_days"),
    }
    return policy, violations


def resolve_commit(repo_root: Path, ref_name: str) -> tuple[str, str | None]:
    proc = run_git(repo_root, "rev-parse", "--verify", f"{ref_name}^{{commit}}")
    if proc.returncode != 0:
        err = proc.stderr.strip() or proc.stdout.strip() or "unknown git error"
        return "", err
    return proc.stdout.strip(), None


def resolve_production_target_ref(repo_root: Path, branch_name: str) -> tuple[str, str | None]:
    remote_ref = f"refs/remotes/origin/{branch_name}"
    local_ref = f"refs/heads/{branch_name}"

    remote_exists = run_git(repo_root, "show-ref", "--verify", "--quiet", remote_ref)
    if remote_exists.returncode == 0:
        return f"origin/{branch_name}", None

    fetch_proc = run_git(repo_root, "fetch", "--quiet", "origin", branch_name)
    if fetch_proc.returncode == 0:
        remote_exists = run_git(repo_root, "show-ref", "--verify", "--quiet", remote_ref)
        if remote_exists.returncode == 0:
            return f"origin/{branch_name}", None

    local_exists = run_git(repo_root, "show-ref", "--verify", "--quiet", local_ref)
    if local_exists.returncode == 0:
        return branch_name, None

    return "", f"Could not resolve production branch ref for `{branch_name}` (checked origin and local refs)."


def is_ancestor(repo_root: Path, ancestor_sha: str, target_ref: str) -> bool:
    proc = run_git(repo_root, "merge-base", "--is-ancestor", ancestor_sha, target_ref)
    return proc.returncode == 0


def build_markdown(report: dict[str, object]) -> str:
    lines: list[str] = []
    lines.append("# Docs Deploy Guard Report")
    lines.append("")
    lines.append(f"- Generated at: `{report['generated_at']}`")
    lines.append(f"- Event: `{report['event_name']}`")
    lines.append(f"- Git ref: `{report['git_ref']}`")
    lines.append(f"- Deploy target: `{report['deploy_target']}`")
    lines.append(f"- Deploy mode: `{report['deploy_mode']}`")
    lines.append(f"- Source ref: `{report['source_ref']}`")
    lines.append(f"- Ready: `{report['ready']}`")
    lines.append("")

    if report["preview_evidence_run_url"]:
        lines.append(f"- Preview evidence: `{report['preview_evidence_run_url']}`")
    if report["rollback_ref_input"]:
        lines.append(f"- Rollback input: `{report['rollback_ref_input']}`")
    if report["rollback_ref_resolved"]:
        lines.append(f"- Rollback resolved: `{report['rollback_ref_resolved']}`")
    if report["preview_evidence_run_url"] or report["rollback_ref_input"] or report["rollback_ref_resolved"]:
        lines.append("")

    if report["warnings"]:
        lines.append("## Warnings")
        for item in report["warnings"]:
            lines.append(f"- {item}")
        lines.append("")

    if report["violations"]:
        lines.append("## Violations")
        for item in report["violations"]:
            lines.append(f"- {item}")
        lines.append("")

    return "\n".join(lines).strip() + "\n"


def write_github_outputs(path: Path, report: dict[str, object]) -> None:
    payload = {
        "ready_to_deploy": "true" if report["ready"] else "false",
        "deploy_target": str(report["deploy_target"]),
        "deploy_mode": str(report["deploy_mode"]),
        "source_ref": str(report["source_ref"]),
        "production_branch_ref": str(report["production_branch_ref"]),
        "docs_preview_retention_days": str(report["policy"]["docs_preview_retention_days"]),
        "docs_guard_artifact_retention_days": str(report["policy"]["docs_guard_artifact_retention_days"]),
    }
    with path.open("a", encoding="utf-8") as fh:
        for key, value in payload.items():
            fh.write(f"{key}={value}\n")


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate docs deploy promotion and rollback contract.")
    parser.add_argument("--repo-root", default=".")
    parser.add_argument("--event-name", required=True)
    parser.add_argument("--git-ref", required=True)
    parser.add_argument("--git-sha", required=True)
    parser.add_argument("--input-deploy-target", default="")
    parser.add_argument("--input-preview-evidence-run-url", default="")
    parser.add_argument("--input-rollback-ref", default="")
    parser.add_argument("--policy-file", required=True)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--github-output-file", default="")
    parser.add_argument("--fail-on-violation", action="store_true")
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    policy_file = Path(args.policy_file).resolve()

    if not policy_file.exists() or not policy_file.is_file():
        print(f"policy file does not exist: {policy_file}", file=sys.stderr)
        return 2

    policy, violations = load_policy(policy_file)
    warnings: list[str] = []

    production_branch = str(policy.get("production_branch", "") or "")
    production_branch_ref = f"refs/heads/{production_branch}" if production_branch else ""

    event_name = args.event_name.strip()
    git_ref = args.git_ref.strip()
    git_sha = args.git_sha.strip()
    input_target = args.input_deploy_target.strip().lower()
    preview_evidence_run_url = args.input_preview_evidence_run_url.strip()
    rollback_ref_input = args.input_rollback_ref.strip()

    deploy_target = "preview"
    if event_name == "workflow_dispatch":
        if input_target not in ALLOWED_DEPLOY_TARGETS:
            violations.append(
                f"workflow_dispatch deploy target must be one of {sorted(ALLOWED_DEPLOY_TARGETS)}, got `{input_target or '<empty>'}`."
            )
        else:
            deploy_target = input_target
    elif event_name == "pull_request":
        deploy_target = "preview"
    elif event_name == "push":
        deploy_target = "production" if git_ref == production_branch_ref else "preview"
    else:
        warnings.append(f"Unexpected event `{event_name}`; defaulting deploy target to preview.")
        deploy_target = "preview"

    deploy_mode = "preview"
    source_ref = git_sha
    rollback_ref_resolved = ""

    if deploy_target == "production":
        deploy_mode = "publish"

        if event_name == "workflow_dispatch":
            if not bool(policy.get("allow_manual_production_dispatch")):
                violations.append("Manual production docs deploy is disabled by policy.")
            if bool(policy.get("require_preview_evidence_on_manual_production")) and not preview_evidence_run_url:
                violations.append("Manual production docs deploy requires `preview_evidence_run_url`.")
            if git_ref != production_branch_ref:
                violations.append(
                    f"Manual production docs deploy must run from `{production_branch_ref}`, got `{git_ref}`."
                )
        elif event_name == "push":
            if git_ref != production_branch_ref:
                violations.append(
                    f"Production docs deploy on push is restricted to `{production_branch_ref}`, got `{git_ref}`."
                )
        else:
            violations.append(f"Production docs deploy is not allowed for event `{event_name}`.")

        if rollback_ref_input:
            deploy_mode = "rollback"
            if event_name != "workflow_dispatch":
                violations.append("`rollback_ref` is only allowed for workflow_dispatch production runs.")
            if not bool(policy.get("allow_manual_rollback_dispatch")):
                violations.append("Manual docs rollback is disabled by policy.")

            rollback_sha, rollback_err = resolve_commit(repo_root, rollback_ref_input)
            if rollback_err:
                violations.append(f"Failed to resolve rollback ref `{rollback_ref_input}`: {rollback_err}")
            else:
                rollback_ref_resolved = rollback_sha
                source_ref = rollback_sha

                if bool(policy.get("rollback_ref_must_be_ancestor_of_production_branch")):
                    target_ref, target_err = resolve_production_target_ref(repo_root, production_branch)
                    if target_err:
                        violations.append(target_err)
                    elif not is_ancestor(repo_root, rollback_sha, target_ref):
                        violations.append(
                            f"Rollback ref `{rollback_ref_input}` ({rollback_sha}) is not an ancestor of `{target_ref}`."
                        )
        else:
            source_ref = git_sha

    else:
        if rollback_ref_input:
            violations.append("`rollback_ref` can only be used when deploy target is `production`.")

    report = {
        "schema_version": "zeroclaw.docs-deploy-guard.v1",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "event_name": event_name,
        "git_ref": git_ref,
        "git_sha": git_sha,
        "deploy_target": deploy_target,
        "deploy_mode": deploy_mode,
        "source_ref": source_ref,
        "production_branch_ref": production_branch_ref,
        "preview_evidence_run_url": preview_evidence_run_url,
        "rollback_ref_input": rollback_ref_input,
        "rollback_ref_resolved": rollback_ref_resolved,
        "policy_file": str(policy_file),
        "policy_schema_version": policy.get("schema_version"),
        "policy": policy,
        "violations": violations,
        "warnings": warnings,
        "ready": not violations,
    }

    output_json = Path(args.output_json).resolve()
    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    output_md = Path(args.output_md).resolve()
    output_md.parent.mkdir(parents=True, exist_ok=True)
    output_md.write_text(build_markdown(report), encoding="utf-8")

    if args.github_output_file:
        write_github_outputs(Path(args.github_output_file), report)

    if violations and args.fail_on_violation:
        print("docs deploy guard violations found:", file=sys.stderr)
        for item in violations:
            print(f"- {item}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
