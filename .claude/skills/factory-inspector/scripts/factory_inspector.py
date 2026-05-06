#!/usr/bin/env python3
"""Factory intake inspector for ZeroClaw PRs and issues."""

from __future__ import annotations

import argparse
import base64
import datetime as dt
import json
import os
import re
import subprocess
import sys
import time
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


DEFAULT_REPO = "zeroclaw-labs/zeroclaw"
DEFAULT_CHECKS = ("pr-intake", "issue-intake")
HIGH_RISK_PREFIXES = (
    "crates/zeroclaw-runtime/",
    "crates/zeroclaw-gateway/",
    "crates/zeroclaw-tools/",
    ".github/workflows/",
)
REQUIRED_TEMPLATE_SECTIONS = (
    "Summary",
    "Validation Evidence (required)",
    "Security & Privacy Impact (required)",
    "Compatibility (required)",
    "Rollback (required for `risk: medium` and `risk: high`)",
)
BOT_LOGINS = {"github-actions", "dependabot", "dependabot[bot]"}
SANDBOX_ORIGINAL_RE = re.compile(r"<!-- factory-sandbox:original:(?P<payload>[A-Za-z0-9_=-]+) -->")
GH_MUTATION_DELAY = float(os.environ.get("FACTORY_INSPECTOR_GH_DELAY", "1.5"))
GH_RETRYABLE_ERRORS = (
    "was submitted too quickly",
    "secondary rate limit",
    "abuse detection",
    "api rate limit exceeded",
    "http 502",
    "http 503",
    "http 504",
)


@dataclass
class Finding:
    code: str
    message: str
    severity: str = "warning"


@dataclass
class Candidate:
    kind: str
    decision: str
    target_type: str
    target: int
    summary: str
    findings: list[Finding] = field(default_factory=list)
    url: str | None = None

    @property
    def marker(self) -> str:
        return f"<!-- factory-inspector:{self.kind}:{self.target_type}:{self.target} -->"


class Gh:
    def __init__(self, repo: str) -> None:
        self.repo = repo

    def run(self, *args: str) -> str:
        cmd = ["gh", *args]
        last_detail = ""
        for attempt in range(1, 7):
            try:
                result = subprocess.run(
                    cmd,
                    text=True,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    check=True,
                )
                throttle_gh_mutation(list(args))
                return result.stdout
            except FileNotFoundError:
                raise SystemExit("gh CLI is required but was not found in PATH")
            except subprocess.CalledProcessError as exc:
                detail = exc.stderr.strip() or exc.stdout.strip()
                last_detail = detail
                if not is_retryable_gh_error(detail) or attempt == 6:
                    raise RuntimeError(f"{' '.join(cmd)} failed: {detail}") from exc
                time.sleep(min(90.0, 5.0 * attempt * attempt))
        raise RuntimeError(f"{' '.join(cmd)} failed: {last_detail}")

    def json(self, *args: str) -> Any:
        out = self.run(*args)
        return json.loads(out) if out.strip() else None

    def open_prs(self, limit: int) -> list[dict[str, Any]]:
        return self.json(
            "pr",
            "list",
            "--repo",
            self.repo,
            "--state",
            "open",
            "--limit",
            str(limit),
            "--json",
            "number,title,body,labels,files,isDraft,url,comments,author",
        )

    def open_issues(self, limit: int) -> list[dict[str, Any]]:
        return self.json(
            "issue",
            "list",
            "--repo",
            self.repo,
            "--state",
            "open",
            "--limit",
            str(limit),
            "--json",
            "number,title,body,labels,url,comments,author",
        )

    def pr_comment(self, number: int, body: str) -> str:
        return self.run(
            "pr",
            "comment",
            str(number),
            "--repo",
            self.repo,
            "--body",
            body,
        ).strip()


def is_retryable_gh_error(detail: str) -> bool:
    lowered = detail.lower()
    return any(token in lowered for token in GH_RETRYABLE_ERRORS)


def throttle_gh_mutation(args: list[str]) -> None:
    if GH_MUTATION_DELAY <= 0 or len(args) < 2:
        return
    if (args[0], args[1]) == ("pr", "comment"):
        time.sleep(GH_MUTATION_DELAY)


def label_names(item: dict[str, Any]) -> set[str]:
    return {label["name"] for label in item.get("labels", [])}


def normalized_label_names(item: dict[str, Any]) -> set[str]:
    return {name.strip().lower() for name in label_names(item)}


def comments_text(item: dict[str, Any]) -> str:
    return "\n".join(comment.get("body") or "" for comment in item.get("comments", []))


def has_marker(item: dict[str, Any], marker: str) -> bool:
    return marker in comments_text(item)


def section_map(markdown: str) -> dict[str, str]:
    sections: dict[str, str] = {}
    current: str | None = None
    buf: list[str] = []
    for line in markdown.splitlines():
        if line.startswith("## "):
            if current is not None:
                sections[current] = "\n".join(buf).strip()
            current = line[3:].strip()
            buf = []
        elif current is not None:
            buf.append(line)
    if current is not None:
        sections[current] = "\n".join(buf).strip()
    return sections


def risk_labels(labels: set[str]) -> set[str]:
    return {label for label in labels if label.startswith("risk:")}


def sandbox_original(pr: dict[str, Any]) -> dict[str, Any] | None:
    body = pr.get("body") or ""
    match = SANDBOX_ORIGINAL_RE.search(body)
    if not match:
        return None
    try:
        raw = base64.urlsafe_b64decode(match.group("payload").encode("ascii"))
        decoded = json.loads(raw)
    except (ValueError, json.JSONDecodeError):
        return None
    return decoded if isinstance(decoded, dict) else None


def touched_paths(pr: dict[str, Any]) -> list[str]:
    original = sandbox_original(pr)
    if original and isinstance(original.get("files"), list):
        return [path for path in original["files"] if isinstance(path, str)]
    return [file["path"] for file in pr.get("files", [])]


def high_risk_paths(paths: list[str]) -> list[str]:
    return [path for path in paths if path.startswith(HIGH_RISK_PREFIXES)]


def section_is_placeholder(section: str) -> bool:
    stripped = section.strip()
    if not stripped:
        return True
    placeholders = [
        "(2-5 bullets",
        "(what this PR",
        "(scenarios, edge cases",
        "(or `None`)",
        "Closes #",
        "Related #",
        "Depends on #",
        "Supersedes #",
    ]
    return any(token in stripped for token in placeholders)


def linked_issue_incomplete(body: str, sections: dict[str, str]) -> bool:
    summary = sections.get("Summary", "")
    linked_lines = [line for line in summary.splitlines() if "Linked issue" in line]
    if not linked_lines:
        return True
    linked = "\n".join(linked_lines)
    if re.search(r"(?i)\b(?:closes|fixes|resolves|related|depends on|supersedes)\s+#\d+", linked):
        return False
    if re.search(r"(?i)\bN\.?A\.?\b|not applicable|none", linked):
        return False
    return "Closes #" in linked or "#" not in linked


def validation_incomplete(sections: dict[str, str]) -> bool:
    section = sections.get("Validation Evidence (required)", "")
    if section_is_placeholder(section):
        return True
    command_tokens = (
        "./dev/ci.sh",
        "scripts/ci/",
        "cargo ",
        "python3 ",
        "actionlint",
        "bash ",
        "markdown",
        "mdbook",
        "npm ",
    )
    return not any(token in section for token in command_tokens)


def security_incomplete(sections: dict[str, str]) -> bool:
    section = sections.get("Security & Privacy Impact (required)", "")
    if section_is_placeholder(section):
        return True
    yes_no_answers = len(re.findall(r"(?i)\b(?:yes|no)\b", section))
    return "(`Yes/No`)" in section or yes_no_answers < 3


def rollback_incomplete(sections: dict[str, str]) -> bool:
    section = sections.get("Rollback (required for `risk: medium` and `risk: high`)", "")
    if section_is_placeholder(section):
        return True
    required = ("Fast rollback", "Feature flags", "Observable failure symptoms")
    return not all(token in section for token in required)


def inspect_pr(pr: dict[str, Any]) -> Candidate | None:
    body = pr.get("body") or ""
    sections = section_map(body)
    labels = normalized_label_names(pr)
    findings: list[Finding] = []

    for required in REQUIRED_TEMPLATE_SECTIONS:
        if required not in sections:
            findings.append(Finding("missing-template-section", f"Missing PR template section: `{required}`."))

    if linked_issue_incomplete(body, sections):
        findings.append(Finding("linked-issue", "Fill the linked issue line with `Closes #...`, `Related #...`, or an explicit `N.A.` rationale."))

    if validation_incomplete(sections):
        findings.append(Finding("validation-evidence", "Validation evidence appears missing or placeholder-only. Paste actual command output or explain skipped commands."))

    labels_risk = risk_labels(labels)
    if not labels_risk:
        findings.append(Finding("risk-label", "Add an appropriate `risk:*` label."))

    risky_paths = high_risk_paths(touched_paths(pr))
    if risky_paths and not ({"risk: high", "risk: manual"} & labels):
        sample = ", ".join(risky_paths[:3])
        findings.append(Finding("high-risk-path", f"High-risk paths touched without `risk: high` or `risk: manual`: {sample}.", "blocking"))

    if ({"risk: medium", "risk: high"} & labels_risk) and rollback_incomplete(sections):
        findings.append(Finding("rollback", "Fill the rollback section for `risk: medium` / `risk: high`."))

    if security_incomplete(sections):
        findings.append(Finding("security-privacy", "Security & Privacy section appears incomplete or placeholder-only."))

    if not findings:
        return None

    return Candidate(
        kind="pr-intake",
        decision="AUTO_COMMENT",
        target_type="pr",
        target=pr["number"],
        summary=f"PR #{pr['number']} has intake issues before deep review.",
        findings=findings,
        url=pr.get("url"),
    )


def inspect_issue(issue: dict[str, Any]) -> Candidate | None:
    labels = normalized_label_names(issue)
    body = issue.get("body") or ""
    title = issue.get("title") or ""
    findings: list[Finding] = []

    if not any(label.startswith("type:") or label in {"bug", "enhancement"} for label in labels):
        findings.append(Finding("missing-type-label", "Issue has no obvious type label."))

    is_bug = "[Bug]" in title or "### Current behavior" in body
    if is_bug and ("### Steps to reproduce" not in body or "_No response_" in body):
        findings.append(Finding("thin-repro", "Bug report may need clearer reproduction steps."))

    if not findings:
        return None

    return Candidate(
        kind="issue-intake",
        decision="QUEUE_FOR_REVIEW",
        target_type="issue",
        target=issue["number"],
        summary=f"Issue #{issue['number']} may need intake cleanup.",
        findings=findings,
        url=issue.get("url"),
    )


def collect_candidates(gh: Gh, checks: set[str], limit: int, include_marked: bool) -> list[Candidate]:
    candidates: list[Candidate] = []
    prs: list[dict[str, Any]] = []
    issues: list[dict[str, Any]] = []

    if "pr-intake" in checks:
        prs = gh.open_prs(limit)
        for pr in prs:
            candidate = inspect_pr(pr)
            if candidate and (include_marked or not has_marker(pr, candidate.marker)):
                candidates.append(candidate)

    if "issue-intake" in checks:
        issues = gh.open_issues(limit)
        for issue in issues:
            candidate = inspect_issue(issue)
            if candidate and (include_marked or not has_marker(issue, candidate.marker)):
                candidates.append(candidate)

    order = {"AUTO_COMMENT": 0, "QUEUE_FOR_REVIEW": 1, "NO_ACTION": 2}
    candidates.sort(key=lambda candidate: (order.get(candidate.decision, 9), candidate.kind, candidate.target))
    return candidates


def render_comment(candidate: Candidate) -> str:
    lines = [
        candidate.marker,
        "Factory Inspector intake check found items to address before deep review:",
        "",
    ]
    for finding in candidate.findings:
        prefix = "[blocking]" if finding.severity == "blocking" else "[check]"
        lines.append(f"- {prefix} {finding.message}")
    lines.extend([
        "",
        "This is an intake-only comment; it is not a code review verdict.",
    ])
    return "\n".join(lines)


def apply_candidate(gh: Gh, candidate: Candidate, mode: str) -> str:
    if mode == "comment-only" and candidate.decision == "AUTO_COMMENT" and candidate.target_type == "pr":
        return gh.pr_comment(candidate.target, render_comment(candidate))
    return "skipped"


def candidate_to_dict(candidate: Candidate) -> dict[str, Any]:
    return {
        "kind": candidate.kind,
        "decision": candidate.decision,
        "target_type": candidate.target_type,
        "target": candidate.target,
        "summary": candidate.summary,
        "findings": [finding.__dict__ for finding in candidate.findings],
        "url": candidate.url,
    }


def write_audit(repo: str, mode: str, candidates: list[Candidate], results: list[dict[str, Any]], output_dir: Path) -> Path:
    output_dir.mkdir(parents=True, exist_ok=True)
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    path = output_dir / f"factory-inspector-{stamp}.json"
    payload = {
        "repo": repo,
        "mode": mode,
        "generated_at": stamp,
        "candidates": [candidate_to_dict(candidate) for candidate in candidates],
        "results": results,
    }
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    return path


def write_markdown_summary(candidates: list[Candidate], results: list[dict[str, Any]], path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    counts = Counter(candidate.decision for candidate in candidates)
    lines = [
        "# Factory Inspector Summary",
        "",
        f"- Candidates: {len(candidates)}",
        f"- AUTO_COMMENT: {counts['AUTO_COMMENT']}",
        f"- QUEUE_FOR_REVIEW: {counts['QUEUE_FOR_REVIEW']}",
        f"- Mutations: {len(results)}",
        "",
        "## Candidates",
        "",
    ]
    for candidate in candidates:
        lines.append(f"- **{candidate.decision}** `{candidate.kind}` {candidate.target_type} #{candidate.target}: {candidate.summary}")
        for finding in candidate.findings:
            lines.append(f"  - `{finding.code}` {finding.message}")
    if results:
        lines.extend(["", "## Mutations", ""])
        for result in results:
            lines.append(f"- {result['target_type']} #{result['target']}: {result['status']}")
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def print_summary(candidates: list[Candidate], results: list[dict[str, Any]]) -> None:
    counts = Counter(candidate.decision for candidate in candidates)
    print("Factory Inspector summary")
    print("=========================")
    print(f"Candidates: {len(candidates)}")
    for decision in ("AUTO_COMMENT", "QUEUE_FOR_REVIEW", "NO_ACTION"):
        if counts[decision]:
            print(f"- {decision}: {counts[decision]}")
    if results:
        print("\nMutations:")
        for result in results:
            print(f"- {result['target_type']} #{result['target']}: {result['status']}")
    print("\nCandidates:")
    for candidate in candidates:
        print(f"- [{candidate.decision}] {candidate.kind}: {candidate.target_type} #{candidate.target}")
        print(f"  {candidate.summary}")
        for finding in candidate.findings:
            print(f"  - {finding.code}: {finding.message}")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", DEFAULT_REPO))
    parser.add_argument("--mode", choices=("preview", "comment-only"), default="preview")
    parser.add_argument("--checks", default=",".join(DEFAULT_CHECKS), help=f"Comma-separated checks. Available: {', '.join(DEFAULT_CHECKS)}")
    parser.add_argument("--limit", type=int, default=1000)
    parser.add_argument("--include-marked", action="store_true")
    parser.add_argument("--max-mutations", type=int, default=25)
    parser.add_argument("--audit-dir", default="artifacts/factory-inspector")
    parser.add_argument("--summary-file")
    parser.add_argument("--no-audit-file", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    checks = {check.strip() for check in args.checks.split(",") if check.strip()}
    unknown = checks - set(DEFAULT_CHECKS)
    if unknown:
        print(f"Unknown checks: {', '.join(sorted(unknown))}", file=sys.stderr)
        return 2

    gh = Gh(args.repo)
    candidates = collect_candidates(gh, checks, args.limit, args.include_marked)
    results: list[dict[str, Any]] = []
    if args.mode != "preview":
        mutation_count = 0
        for candidate in candidates:
            if candidate.decision != "AUTO_COMMENT":
                continue
            if mutation_count >= args.max_mutations:
                status = "skipped: max mutations reached"
            else:
                status = apply_candidate(gh, candidate, args.mode)
                if not status.startswith("skipped"):
                    mutation_count += 1
            results.append({
                "target_type": candidate.target_type,
                "target": candidate.target,
                "status": status,
            })

    print_summary(candidates, results)
    if args.summary_file:
        write_markdown_summary(candidates, results, Path(args.summary_file))
    if not args.no_audit_file:
        path = write_audit(args.repo, args.mode, candidates, results, Path(args.audit_dir))
        print(f"\nAudit file: {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
