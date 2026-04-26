#!/usr/bin/env python3
"""Offline replay and safety testbench for ZeroClaw factory automations."""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any


DEFAULT_REPO = "zeroclaw-labs/zeroclaw"
DEFAULT_OUTPUT_DIR = Path("artifacts/factory-testbench")
ROOT = Path(__file__).resolve().parents[4]
CLERK_PATH = ROOT / ".claude/skills/factory-clerk/scripts/factory_clerk.py"
INSPECTOR_PATH = ROOT / ".claude/skills/factory-inspector/scripts/factory_inspector.py"
FOREMAN_PATH = ROOT / ".claude/skills/factory-foreman/scripts/factory_foreman.py"
REF_RE = re.compile(r"(?<![\w/.-])#(?P<number>\d+)\b")


def load_module(path: Path, name: str) -> Any:
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load module from {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


clerk = load_module(CLERK_PATH, "factory_clerk_replay")
inspector = load_module(INSPECTOR_PATH, "factory_inspector_replay")


def utc_stamp() -> str:
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def run_gh_json(args: list[str]) -> Any:
    out = run_gh_text(args)
    return json.loads(out) if out.strip() else None


def run_gh_text(args: list[str], input_text: str | None = None) -> str:
    cmd = ["gh", *args]
    try:
        result = subprocess.run(
            cmd,
            input=input_text,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
    except FileNotFoundError:
        raise SystemExit("gh CLI is required but was not found in PATH")
    except subprocess.CalledProcessError as exc:
        detail = exc.stderr.strip() or exc.stdout.strip()
        raise RuntimeError(f"{' '.join(cmd)} failed: {detail}") from exc
    return result.stdout


def run_cmd(args: list[str], cwd: Path | None = None, input_text: str | None = None) -> str:
    result = subprocess.run(
        args,
        cwd=cwd,
        input=input_text,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    )
    return result.stdout.strip() or result.stderr.strip()


def run_git(args: list[str], cwd: Path | None = None) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if result.returncode != 0:
        return ""
    return result.stdout.strip()


def git_success(args: list[str], cwd: Path | None = None) -> bool:
    result = subprocess.run(
        ["git", *args],
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    return result.returncode == 0


def clone_mirror(repo: str, clone_dir: Path) -> str:
    clone_dir.parent.mkdir(parents=True, exist_ok=True)
    url = f"https://github.com/{repo}.git"
    if clone_dir.exists():
        result = subprocess.run(
            ["git", "-C", str(clone_dir), "remote", "update", "--prune"],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
        return result.stdout.strip() or result.stderr.strip()
    result = subprocess.run(
        ["git", "clone", "--mirror", url, str(clone_dir)],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=True,
    )
    return result.stdout.strip() or result.stderr.strip()


def list_issues(repo: str, state: str, limit: int) -> list[dict[str, Any]]:
    return run_gh_json([
        "issue",
        "list",
        "--repo",
        repo,
        "--state",
        state,
        "--limit",
        str(limit),
        "--json",
        "number,title,body,labels,comments,updatedAt,url,author,state",
    ])


def list_prs(repo: str, state: str, limit: int) -> list[dict[str, Any]]:
    return run_gh_json([
        "pr",
        "list",
        "--repo",
        repo,
        "--state",
        state,
        "--limit",
        str(limit),
        "--json",
        "number,title,body,labels,comments,mergedAt,state,url,isDraft,files,baseRefName,headRefName,headRepositoryOwner,author",
    ])


def dedupe_by_number(items: list[dict[str, Any]]) -> list[dict[str, Any]]:
    seen: dict[int, dict[str, Any]] = {}
    for item in items:
        seen[int(item["number"])] = item
    return [seen[number] for number in sorted(seen)]


def write_json(payload: dict[str, Any], output_dir: Path, prefix: str) -> Path:
    output_dir.mkdir(parents=True, exist_ok=True)
    path = output_dir / f"{prefix}-{utc_stamp()}.json"
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    latest = output_dir / f"{prefix}-latest.json"
    latest.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return path


def snapshot(repo: str, limit: int, output_dir: Path, clone_dir: Path | None) -> Path:
    clone_result = None
    if clone_dir is not None:
        clone_result = clone_mirror(repo, clone_dir)

    issues = dedupe_by_number(list_issues(repo, "all", limit))
    prs = dedupe_by_number(
        list_prs(repo, "open", limit)
        + list_prs(repo, "closed", limit)
        + list_prs(repo, "merged", limit)
    )
    payload = {
        "schema": "factory-testbench-snapshot-v1",
        "repo": repo,
        "generated_at": utc_stamp(),
        "source_head": run_git(["rev-parse", "HEAD"], ROOT),
        "clone_dir": str(clone_dir) if clone_dir else None,
        "clone_result": clone_result,
        "issues": issues,
        "prs": prs,
    }
    return write_json(payload, output_dir, "snapshot")


def parse_created_number(url: str) -> int:
    match = re.search(r"/(?:issues|pull)/(?P<number>\d+)\s*$", url.strip())
    if not match:
        raise RuntimeError(f"Could not parse GitHub number from: {url}")
    return int(match.group("number"))


def repo_exists(repo: str) -> bool:
    result = subprocess.run(
        ["gh", "repo", "view", repo, "--json", "nameWithOwner"],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    return result.returncode == 0


def create_private_repo(repo: str, reuse_existing: bool, dry_run: bool) -> str:
    if dry_run:
        return f"dry-run: would create private repo {repo}"
    if repo_exists(repo):
        if reuse_existing:
            return f"reusing existing repo {repo}"
        raise RuntimeError(f"Target repo {repo} already exists. Pass --reuse-existing to use it.")
    return run_gh_text([
        "repo",
        "create",
        repo,
        "--private",
        "--description",
        "Factory sandbox replay repository",
        "--disable-wiki",
    ]).strip()


def push_mirror_to_target(source_repo: str, target_repo: str, work_dir: Path, dry_run: bool) -> str:
    if dry_run:
        return f"dry-run: would push branches/tags from {source_repo} to {target_repo}"
    mirror_dir = work_dir / "source.git"
    clone_mirror(source_repo, mirror_dir)
    return run_cmd([
        "git",
        "push",
        "--prune",
        f"https://github.com/{target_repo}.git",
        "+refs/heads/*:refs/heads/*",
        "+refs/tags/*:refs/tags/*",
    ], mirror_dir)


def label_payload(data: dict[str, Any]) -> dict[str, dict[str, str]]:
    labels: dict[str, dict[str, str]] = {}
    for collection in ("issues", "prs"):
        for item in data.get(collection, []):
            for label in item.get("labels", []):
                name = label.get("name")
                if not name:
                    continue
                labels[name] = {
                    "color": (label.get("color") or "ededed").lstrip("#")[:6] or "ededed",
                    "description": label.get("description") or "",
                }
    return labels


def create_labels(repo: str, labels: dict[str, dict[str, str]], dry_run: bool) -> list[str]:
    results: list[str] = []
    for name, meta in sorted(labels.items()):
        if dry_run:
            results.append(f"dry-run: label {name}")
            continue
        args = [
            "label",
            "create",
            name,
            "--repo",
            repo,
            "--color",
            meta["color"],
            "--force",
        ]
        if meta["description"]:
            args.extend(["--description", meta["description"]])
        results.append(run_gh_text(args).strip())
    return results


def label_args(labels: list[dict[str, Any]]) -> list[str]:
    args: list[str] = []
    for label in labels:
        name = label.get("name")
        if name:
            args.extend(["--label", name])
    return args


def compact_metadata(kind: str, source_repo: str, item: dict[str, Any]) -> str:
    payload = {
        "kind": kind,
        "source_repo": source_repo,
        "number": item.get("number"),
        "url": item.get("url"),
        "state": item.get("state"),
        "merged_at": item.get("mergedAt"),
        "base_ref": item.get("baseRefName"),
        "head_ref": item.get("headRefName"),
        "head_owner": (item.get("headRepositoryOwner") or {}).get("login"),
        "files": [file.get("path") for file in item.get("files", []) if file.get("path")],
    }
    raw = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return base64.urlsafe_b64encode(raw).decode("ascii")


def rewrite_refs(text: str, number_map: dict[int, int]) -> str:
    def replace(match: re.Match[str]) -> str:
        original = int(match.group("number"))
        return f"#{number_map.get(original, original)}"

    return REF_RE.sub(replace, text or "")


def sandbox_body(kind: str, source_repo: str, item: dict[str, Any], number_map: dict[int, int]) -> str:
    marker = compact_metadata(kind, source_repo, item)
    original = rewrite_refs(item.get("body") or "", number_map)
    header = [
        f"<!-- factory-sandbox:original:{marker} -->",
        f"Factory sandbox replay of `{source_repo}#{item.get('number')}`.",
        "",
    ]
    return "\n".join(header) + original


def comment_body(source_repo: str, comment: dict[str, Any], number_map: dict[int, int]) -> str:
    author = (comment.get("author") or {}).get("login") or "unknown"
    association = comment.get("authorAssociation") or "NONE"
    created = comment.get("createdAt") or "unknown time"
    body = rewrite_refs(comment.get("body") or "", number_map)
    return (
        f"Factory sandbox replay of a comment from @{author} "
        f"({association}) on {created} in `{source_repo}`.\n\n"
        f"{body}"
    )


def ensure_work_clone(target_repo: str, work_dir: Path) -> Path:
    clone_dir = work_dir / "target"
    if clone_dir.exists():
        run_cmd(["git", "fetch", "origin", "--prune"], clone_dir)
        return clone_dir
    run_cmd(["git", "clone", f"https://github.com/{target_repo}.git", str(clone_dir)])
    run_cmd(["git", "config", "user.name", "Factory Sandbox"], clone_dir)
    run_cmd(["git", "config", "user.email", "factory-sandbox@example.invalid"], clone_dir)
    return clone_dir


def ensure_base_branch(clone_dir: Path, base: str) -> str:
    run_cmd(["git", "fetch", "origin", "--prune"], clone_dir)
    if run_git(["show-ref", "--verify", f"refs/remotes/origin/{base}"], clone_dir):
        return base
    fallback = "master"
    run_cmd(["git", "checkout", "-B", base, f"origin/{fallback}"], clone_dir)
    run_cmd(["git", "push", "origin", base], clone_dir)
    return base


def create_sandbox_branch(clone_dir: Path, pr: dict[str, Any], base: str) -> str:
    branch = f"factory-sandbox/pr-{pr['number']}"
    run_cmd(["git", "checkout", "-B", branch, f"origin/{base}"], clone_dir)
    target = clone_dir / ".factory-sandbox" / "prs" / f"original-{pr['number']}.md"
    target.parent.mkdir(parents=True, exist_ok=True)
    files = "\n".join(f"- `{file.get('path')}`" for file in pr.get("files", []) if file.get("path"))
    target.write_text(
        "\n".join([
            f"# Original PR #{pr['number']}",
            "",
            f"Title: {pr.get('title') or ''}",
            f"Original URL: {pr.get('url') or ''}",
            f"Original base: {pr.get('baseRefName') or ''}",
            "",
            "## Original files",
            "",
            files or "- None captured",
            "",
        ]),
        encoding="utf-8",
    )
    run_cmd(["git", "add", str(target.relative_to(clone_dir))], clone_dir)
    if not git_success(["diff", "--cached", "--quiet"], clone_dir):
        run_cmd(["git", "commit", "-m", f"factory sandbox replay for PR {pr['number']}"], clone_dir)
    run_cmd(["git", "push", "-f", "origin", branch], clone_dir)
    return branch


def create_sandbox_issues(
    repo: str,
    source_repo: str,
    issues: list[dict[str, Any]],
    dry_run: bool,
) -> dict[int, int]:
    issue_map: dict[int, int] = {}
    for issue in issues:
        original = int(issue["number"])
        if dry_run:
            issue_map[original] = original
            continue
        body = sandbox_body("issue", source_repo, issue, {})
        args = [
            "issue",
            "create",
            "--repo",
            repo,
            "--title",
            issue.get("title") or f"Original issue #{original}",
            "--body-file",
            "-",
        ]
        args.extend(label_args(issue.get("labels", [])))
        url = run_gh_text(args, input_text=body)
        issue_map[original] = parse_created_number(url)
    return issue_map


def create_sandbox_prs(
    repo: str,
    source_repo: str,
    prs: list[dict[str, Any]],
    issue_map: dict[int, int],
    work_dir: Path,
    dry_run: bool,
) -> dict[int, int]:
    pr_map: dict[int, int] = {}
    if dry_run:
        return {int(pr["number"]): int(pr["number"]) for pr in prs}
    clone_dir = ensure_work_clone(repo, work_dir)
    for pr in prs:
        original = int(pr["number"])
        base = ensure_base_branch(clone_dir, pr.get("baseRefName") or "master")
        branch = create_sandbox_branch(clone_dir, pr, base)
        placeholder = sandbox_body("pr", source_repo, pr, issue_map)
        args = [
            "pr",
            "create",
            "--repo",
            repo,
            "--base",
            base,
            "--head",
            branch,
            "--title",
            pr.get("title") or f"Original PR #{original}",
            "--body-file",
            "-",
        ]
        if pr.get("isDraft"):
            args.append("--draft")
        args.extend(label_args(pr.get("labels", [])))
        url = run_gh_text(args, input_text=placeholder)
        pr_map[original] = parse_created_number(url)
    return pr_map


def edit_bodies_and_comments(
    repo: str,
    source_repo: str,
    issues: list[dict[str, Any]],
    prs: list[dict[str, Any]],
    issue_map: dict[int, int],
    pr_map: dict[int, int],
    dry_run: bool,
) -> list[str]:
    results: list[str] = []
    number_map = {**issue_map, **pr_map}
    if dry_run:
        return ["dry-run: would edit bodies and replay comments"]

    for issue in issues:
        mapped = issue_map[int(issue["number"])]
        body = sandbox_body("issue", source_repo, issue, number_map)
        run_gh_text(["issue", "edit", str(mapped), "--repo", repo, "--body-file", "-"], input_text=body)
        for comment in issue.get("comments", []):
            run_gh_text(
                ["issue", "comment", str(mapped), "--repo", repo, "--body-file", "-"],
                input_text=comment_body(source_repo, comment, number_map),
            )
        if (issue.get("state") or "OPEN").lower() == "closed":
            run_gh_text([
                "issue",
                "close",
                str(mapped),
                "--repo",
                repo,
                "--reason",
                "not planned",
                "--comment",
                "Factory sandbox replay: original issue was closed.",
            ])
        results.append(f"issue {issue['number']} -> {mapped}")

    for pr in prs:
        mapped = pr_map[int(pr["number"])]
        body = sandbox_body("pr", source_repo, pr, number_map)
        should_merge = bool(pr.get("mergedAt"))
        if should_merge:
            run_gh_text([
                "pr",
                "merge",
                str(mapped),
                "--repo",
                repo,
                "--merge",
                "--subject",
                f"Factory sandbox merge for original PR #{pr['number']}",
                "--body",
                "Merged before replay body restoration so closing keywords do not auto-close sandbox issues.",
            ])
        run_gh_text(["pr", "edit", str(mapped), "--repo", repo, "--body-file", "-"], input_text=body)
        for comment in pr.get("comments", []):
            run_gh_text(
                ["pr", "comment", str(mapped), "--repo", repo, "--body-file", "-"],
                input_text=comment_body(source_repo, comment, number_map),
            )
        if not should_merge and (pr.get("state") or "OPEN").lower() == "closed":
            run_gh_text([
                "pr",
                "close",
                str(mapped),
                "--repo",
                repo,
                "--comment",
                "Factory sandbox replay: original PR was closed without merge.",
            ])
        results.append(f"pr {pr['number']} -> {mapped}")
    return results


def run_foreman_on_sandbox(repo: str, mode: str, allow_apply_safe: bool, max_mutations: int, output_dir: Path) -> dict[str, Any]:
    if mode == "none":
        return {"mode": "none", "returncode": 0}
    cmd = [
        "python3",
        str(FOREMAN_PATH),
        "--repo",
        repo,
        "--mode",
        mode,
        "--max-mutations",
        str(max_mutations),
        "--output-dir",
        str(output_dir / "foreman"),
        "--summary-file",
        str(output_dir / "foreman" / "summary.md"),
    ]
    if allow_apply_safe:
        cmd.append("--allow-apply-safe")
    result = subprocess.run(cmd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    return {
        "mode": mode,
        "returncode": result.returncode,
        "stdout": result.stdout,
        "stderr": result.stderr,
    }


def sandbox(data: dict[str, Any], args: argparse.Namespace) -> dict[str, Any]:
    target_repo = args.target_repo
    output_dir = Path(args.output_dir)
    work_dir = Path(args.work_dir) if args.work_dir else Path(tempfile.mkdtemp(prefix="factory-sandbox-"))
    issues = data.get("issues", [])[:args.limit]
    prs = data.get("prs", [])[:args.limit]
    summary: dict[str, Any] = {
        "schema": "factory-testbench-sandbox-v1",
        "source_repo": data.get("repo"),
        "target_repo": target_repo,
        "generated_at": utc_stamp(),
        "dry_run": args.dry_run,
        "work_dir": str(work_dir),
        "issue_count": len(issues),
        "pr_count": len(prs),
        "steps": [],
        "issue_map": {},
        "pr_map": {},
    }

    summary["steps"].append(create_private_repo(target_repo, args.reuse_existing, args.dry_run))
    summary["steps"].append(push_mirror_to_target(data.get("repo") or DEFAULT_REPO, target_repo, work_dir, args.dry_run))
    labels = label_payload({"issues": issues, "prs": prs})
    summary["steps"].extend(create_labels(target_repo, labels, args.dry_run))
    issue_map = create_sandbox_issues(target_repo, data.get("repo") or DEFAULT_REPO, issues, args.dry_run)
    pr_map = create_sandbox_prs(target_repo, data.get("repo") or DEFAULT_REPO, prs, issue_map, work_dir, args.dry_run)
    summary["issue_map"] = issue_map
    summary["pr_map"] = pr_map
    summary["steps"].extend(edit_bodies_and_comments(
        target_repo,
        data.get("repo") or DEFAULT_REPO,
        issues,
        prs,
        issue_map,
        pr_map,
        args.dry_run,
    ))
    if args.run_foreman_mode != "none" and not args.dry_run:
        summary["foreman"] = run_foreman_on_sandbox(
            target_repo,
            args.run_foreman_mode,
            args.allow_apply_safe,
            args.max_mutations,
            output_dir,
        )
    return summary


@dataclass
class StaticGh:
    data: dict[str, Any]

    def issue_list(self, state: str = "open", limit: int = 1000) -> list[dict[str, Any]]:
        issues = self.data.get("issues", [])
        if state != "all":
            issues = [issue for issue in issues if (issue.get("state") or "OPEN").lower() == state]
        return issues[:limit]

    def pr_list(self, state: str, limit: int = 300) -> list[dict[str, Any]]:
        prs = self.data.get("prs", [])
        if state == "merged":
            filtered = [pr for pr in prs if pr.get("mergedAt")]
        elif state == "open":
            filtered = [pr for pr in prs if (pr.get("state") or "OPEN").lower() == "open"]
        elif state == "closed":
            filtered = [pr for pr in prs if (pr.get("state") or "").lower() == "closed" and not pr.get("mergedAt")]
        else:
            filtered = prs
        return filtered[:limit]

    def open_prs(self, limit: int) -> list[dict[str, Any]]:
        return self.pr_list("open", limit)

    def open_issues(self, limit: int) -> list[dict[str, Any]]:
        return self.issue_list("open", limit)


def replay(data: dict[str, Any], args: argparse.Namespace) -> dict[str, Any]:
    gh = StaticGh(data)
    clerk_checks = {check.strip() for check in args.clerk_checks.split(",") if check.strip()}
    inspector_checks = {check.strip() for check in args.inspector_checks.split(",") if check.strip()}
    clerk_candidates = clerk.collect_candidates(
        gh,
        clerk_checks,
        args.merged_limit,
        args.open_limit,
        args.similarity_threshold,
        args.similarity_max,
        args.implemented_max,
        args.implemented_symbol_max,
        args.implemented_match_max,
        args.include_marked,
    )
    inspector_candidates = inspector.collect_candidates(
        gh,
        inspector_checks,
        args.open_limit,
        args.include_marked,
    )
    payload = {
        "schema": "factory-testbench-replay-v1",
        "repo": data.get("repo"),
        "generated_at": utc_stamp(),
        "source_snapshot": data.get("generated_at"),
        "clerk": [clerk.candidate_to_dict(candidate) for candidate in clerk_candidates],
        "inspector": [inspector.candidate_to_dict(candidate) for candidate in inspector_candidates],
    }
    payload["invariants"] = check_invariants(payload, data)
    return payload


def labels_for(data: dict[str, Any], target_type: str, number: int) -> set[str]:
    collection = data.get("issues" if target_type == "issue" else "prs", [])
    for item in collection:
        if int(item["number"]) == number:
            return {label["name"].strip().lower() for label in item.get("labels", [])}
    return set()


def pr_for(data: dict[str, Any], number: int) -> dict[str, Any] | None:
    for pr in data.get("prs", []):
        if int(pr["number"]) == number:
            return pr
    return None


def protected(labels: set[str]) -> bool:
    protected_exact = {
        "security",
        "risk: high",
        "risk: manual",
        "type:rfc",
        "type:tracking",
        "status:blocked",
        "status:needs-maintainer-decision",
        "r:needs-maintainer-decision",
        "no-stale",
        "priority:critical",
    }
    return bool(protected_exact & labels) or any(
        label.startswith("security:") for label in labels
    )


def check_invariants(replay_payload: dict[str, Any], data: dict[str, Any]) -> dict[str, Any]:
    failures: list[str] = []
    markers: set[str] = set()

    for candidate in replay_payload["clerk"]:
        marker_key = f"clerk:{candidate['action'] or candidate['kind']}:{candidate['target_type']}:{candidate['target']}:{candidate.get('related')}"
        if marker_key in markers:
            failures.append(f"duplicate clerk marker {marker_key}")
        markers.add(marker_key)

        if candidate["decision"] != "AUTO_CLOSE":
            continue
        labels = labels_for(data, candidate["target_type"], int(candidate["target"]))
        if protected(labels):
            failures.append(f"protected target auto-close: {candidate['target_type']} #{candidate['target']}")
        if candidate["kind"] in {"similarity-preview", "implemented-on-master-preview", "open-pr-links"}:
            failures.append(f"unsafe kind auto-close: {candidate['kind']} #{candidate['target']}")
        if candidate["kind"] == "fixed-by-merged-pr":
            related = candidate.get("related")
            pr = pr_for(data, int(related)) if related else None
            if not pr or pr.get("baseRefName") != "master":
                failures.append(f"non-master fixed-by auto-close: issue #{candidate['target']} via PR #{related}")

    for candidate in replay_payload["inspector"]:
        marker_key = f"inspector:{candidate['kind']}:{candidate['target_type']}:{candidate['target']}"
        if marker_key in markers:
            failures.append(f"duplicate inspector marker {marker_key}")
        markers.add(marker_key)
        if candidate["target_type"] == "issue" and candidate["decision"] == "AUTO_COMMENT":
            failures.append(f"inspector issue mutation candidate: issue #{candidate['target']}")

    return {
        "passed": not failures,
        "failures": failures,
    }


def built_in_fixture() -> dict[str, Any]:
    maintainer = {
        "authorAssociation": "MEMBER",
        "body": "Duplicate of #1.",
        "createdAt": "2026-01-01T00:00:00Z",
        "author": {"login": "maintainer"},
    }
    return {
        "schema": "factory-testbench-snapshot-v1",
        "repo": "example/replay",
        "generated_at": "2026-01-01T00:00:00Z",
        "issues": [
            {"number": 1, "title": "Canonical", "body": "Keep this.", "labels": [], "comments": [], "state": "OPEN"},
            {"number": 2, "title": "Duplicate", "body": "Same bug.", "labels": [], "comments": [maintainer], "state": "OPEN"},
            {"number": 3, "title": "Protected", "body": "Fixes from PR.", "labels": [{"name": "risk: high"}], "comments": [], "state": "OPEN"},
            {"number": 4, "title": "Fixed", "body": "Bug.", "labels": [], "comments": [], "state": "OPEN"},
            {"number": 5, "title": "Protected mixed case", "body": "Fixes from PR.", "labels": [{"name": "Risk: High"}], "comments": [], "state": "OPEN"},
        ],
        "prs": [
            {
                "number": 10,
                "title": "fix: thing",
                "body": "Fixes #4",
                "labels": [],
                "comments": [],
                "mergedAt": "2026-01-02T00:00:00Z",
                "state": "MERGED",
                "isDraft": False,
                "files": [],
                "baseRefName": "master",
            },
            {
                "number": 11,
                "title": "fix: protected",
                "body": "Fixes #3",
                "labels": [],
                "comments": [],
                "mergedAt": "2026-01-02T00:00:00Z",
                "state": "MERGED",
                "isDraft": False,
                "files": [],
                "baseRefName": "master",
            },
            {
                "number": 12,
                "title": "fix: release",
                "body": "Fixes #1",
                "labels": [],
                "comments": [],
                "mergedAt": "2026-01-02T00:00:00Z",
                "state": "MERGED",
                "isDraft": False,
                "files": [],
                "baseRefName": "release/0.1",
            },
            {
                "number": 13,
                "title": "fix: protected mixed case",
                "body": "Fixes #5",
                "labels": [],
                "comments": [],
                "mergedAt": "2026-01-02T00:00:00Z",
                "state": "MERGED",
                "isDraft": False,
                "files": [],
                "baseRefName": "master",
            },
            {
                "number": 20,
                "title": "feat: security-sensitive branch",
                "body": "Superseded by #21",
                "labels": [{"name": "Security"}],
                "comments": [],
                "mergedAt": None,
                "state": "OPEN",
                "isDraft": False,
                "files": [],
                "baseRefName": "master",
            },
            {
                "number": 21,
                "title": "feat: replacement branch",
                "body": "",
                "labels": [],
                "comments": [],
                "mergedAt": "2026-01-02T00:00:00Z",
                "state": "MERGED",
                "isDraft": False,
                "files": [],
                "baseRefName": "master",
            },
        ],
    }


def run_fixture_test(args: argparse.Namespace) -> dict[str, Any]:
    payload = replay(built_in_fixture(), args)
    auto_closes = {
        (item["kind"], item["target"])
        for item in payload["clerk"]
        if item["decision"] == "AUTO_CLOSE"
    }
    expected = {("explicit-duplicates", 2), ("fixed-by-merged-pr", 4)}
    failures = list(payload["invariants"]["failures"])
    if auto_closes != expected:
        failures.append(f"unexpected auto-close set: {sorted(auto_closes)} expected {sorted(expected)}")
    payload["fixture"] = {
        "passed": not failures,
        "failures": failures,
    }
    payload["invariants"]["passed"] = payload["invariants"]["passed"] and payload["fixture"]["passed"]
    payload["invariants"]["failures"] = failures
    return payload


def print_replay_summary(payload: dict[str, Any]) -> None:
    clerk_count = len(payload.get("clerk", []))
    inspector_count = len(payload.get("inspector", []))
    print("Factory Testbench summary")
    print("=========================")
    print(f"Clerk candidates: {clerk_count}")
    print(f"Inspector candidates: {inspector_count}")
    print(f"Invariants: {'pass' if payload['invariants']['passed'] else 'fail'}")
    for failure in payload["invariants"]["failures"]:
        print(f"- {failure}")


def common_replay_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--clerk-checks", default="fixed-by-merged-pr,explicit-duplicates,superseded-prs,open-pr-links,implemented-on-master-preview,similarity-preview")
    parser.add_argument("--inspector-checks", default="pr-intake,issue-intake")
    parser.add_argument("--merged-limit", type=int, default=250)
    parser.add_argument("--open-limit", type=int, default=1000)
    parser.add_argument("--similarity-threshold", type=float, default=0.42)
    parser.add_argument("--similarity-max", type=int, default=25)
    parser.add_argument("--implemented-max", type=int, default=25)
    parser.add_argument("--implemented-symbol-max", type=int, default=8)
    parser.add_argument("--implemented-match-max", type=int, default=3)
    parser.add_argument("--include-marked", action="store_true")
    parser.add_argument("--output-dir", default=str(DEFAULT_OUTPUT_DIR))
    parser.add_argument("--no-audit-file", action="store_true")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    snapshot_parser = sub.add_parser("snapshot")
    snapshot_parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", DEFAULT_REPO))
    snapshot_parser.add_argument("--limit", type=int, default=1000)
    snapshot_parser.add_argument("--output-dir", default=str(DEFAULT_OUTPUT_DIR))
    snapshot_parser.add_argument("--clone-dir")

    replay_parser = sub.add_parser("replay")
    replay_parser.add_argument("--snapshot", required=True)
    common_replay_args(replay_parser)

    fixture_parser = sub.add_parser("fixture-test")
    fixture_parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", DEFAULT_REPO))
    fixture_parser.add_argument("--limit", type=int, default=1000)
    common_replay_args(fixture_parser)

    roundtrip_parser = sub.add_parser("roundtrip")
    roundtrip_parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", DEFAULT_REPO))
    roundtrip_parser.add_argument("--limit", type=int, default=1000)
    roundtrip_parser.add_argument("--clone-dir")
    common_replay_args(roundtrip_parser)

    sandbox_parser = sub.add_parser("sandbox")
    sandbox_parser.add_argument("--snapshot")
    sandbox_parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", DEFAULT_REPO))
    sandbox_parser.add_argument("--target-repo", required=True)
    sandbox_parser.add_argument("--limit", type=int, default=1000)
    sandbox_parser.add_argument("--output-dir", default=str(DEFAULT_OUTPUT_DIR))
    sandbox_parser.add_argument("--work-dir")
    sandbox_parser.add_argument("--reuse-existing", action="store_true")
    sandbox_parser.add_argument("--dry-run", action="store_true")
    sandbox_parser.add_argument("--run-foreman-mode", choices=("none", "preview", "comment-only", "apply-safe"), default="none")
    sandbox_parser.add_argument("--allow-apply-safe", action="store_true")
    sandbox_parser.add_argument("--max-mutations", type=int, default=25)

    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    output_dir = Path(getattr(args, "output_dir", DEFAULT_OUTPUT_DIR))

    if args.command == "snapshot":
        path = snapshot(args.repo, args.limit, output_dir, Path(args.clone_dir) if args.clone_dir else None)
        print(f"Snapshot file: {path}")
        return 0

    if args.command == "sandbox":
        if args.snapshot:
            data = json.loads(Path(args.snapshot).read_text(encoding="utf-8"))
        else:
            snapshot_path = snapshot(args.repo, args.limit, output_dir, None)
            data = json.loads(snapshot_path.read_text(encoding="utf-8"))
        payload = sandbox(data, args)
        print("Factory Sandbox summary")
        print("=======================")
        print(f"Source: {payload['source_repo']}")
        print(f"Target: {payload['target_repo']}")
        print(f"Issues: {payload['issue_count']}")
        print(f"PRs: {payload['pr_count']}")
        print(f"Dry run: {payload['dry_run']}")
        path = write_json(payload, output_dir, "sandbox")
        print(f"Sandbox file: {path}")
        foreman = payload.get("foreman")
        if isinstance(foreman, dict) and foreman.get("returncode"):
            return int(foreman["returncode"])
        return 0

    if args.command == "roundtrip":
        snapshot_path = snapshot(args.repo, args.limit, output_dir, Path(args.clone_dir) if args.clone_dir else None)
        data = json.loads(snapshot_path.read_text(encoding="utf-8"))
        payload = replay(data, args)
    elif args.command == "replay":
        data = json.loads(Path(args.snapshot).read_text(encoding="utf-8"))
        payload = replay(data, args)
    elif args.command == "fixture-test":
        payload = run_fixture_test(args)
    else:
        raise AssertionError(args.command)

    print_replay_summary(payload)
    if not args.no_audit_file:
        path = write_json(payload, output_dir, "replay")
        print(f"Replay file: {path}")
    return 0 if payload["invariants"]["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
