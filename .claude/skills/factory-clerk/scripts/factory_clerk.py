#!/usr/bin/env python3
"""Conservative GitHub factory cleanup for ZeroClaw.

This runner intentionally does less than a human could do. It only mutates
GitHub when the relationship is explicit and low-ambiguity.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import math
import os
import re
import subprocess
import sys
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


DEFAULT_REPO = "zeroclaw-labs/zeroclaw"
DEFAULT_CHECKS = (
    "fixed-by-merged-pr",
    "explicit-duplicates",
    "superseded-prs",
    "open-pr-links",
    "implemented-on-master-preview",
    "similarity-preview",
)

ISSUE_PROTECTED_LABELS = {
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
PROTECTED_LABEL_PREFIXES = ("security:",)

PR_PROTECTED_LABELS = {
    "security",
    "risk: high",
    "risk: manual",
    "status:blocked",
    "status:needs-maintainer-decision",
    "r:needs-maintainer-decision",
}

CLOSING_RE = re.compile(
    r"(?i)\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s+#(?P<number>\d+)\b"
)
ISSUE_REF_RE = re.compile(r"(?<![\w/.-])#(?P<number>\d+)\b")
DUPLICATE_RE = re.compile(
    r"(?i)\b(?:duplicate of|duplicates|same as)\s+#(?P<number>\d+)\b"
)
SUPERSEDED_RE = re.compile(
    r"(?i)\b(?:superseded by|replaced by)\s+#(?P<number>\d+)\b"
)
BOT_LOGINS = {"github-actions", "dependabot", "dependabot[bot]"}
MAINTAINER_ASSOCIATIONS = {"COLLABORATOR", "MEMBER", "OWNER"}
NOISY_IDENTIFIERS = {
    "as_str",
    "from_str",
    "to_string",
    "unwrap_or",
    "serde_json",
    "serde_yaml",
}


@dataclass
class Candidate:
    kind: str
    decision: str
    target_type: str
    target: int
    related: int | None
    summary: str
    evidence: list[str] = field(default_factory=list)
    action: str | None = None
    url: str | None = None

    @property
    def marker(self) -> str:
        related = self.related if self.related is not None else "none"
        return f"<!-- factory-clerk:{self.action or self.kind}:{self.target_type}:{self.target}:{related} -->"


class Gh:
    def __init__(self, repo: str, dry_run: bool = False) -> None:
        self.repo = repo
        self.dry_run = dry_run

    def run(self, *args: str, input_text: str | None = None) -> str:
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

    def json(self, *args: str) -> Any:
        out = self.run(*args)
        return json.loads(out) if out.strip() else None

    def issue_list(self, state: str = "open", limit: int = 1000) -> list[dict[str, Any]]:
        return self.json(
            "issue",
            "list",
            "--repo",
            self.repo,
            "--state",
            state,
            "--limit",
            str(limit),
            "--json",
            "number,title,body,labels,comments,updatedAt,url,author",
        )

    def issue_view(self, number: int) -> dict[str, Any]:
        return self.json(
            "issue",
            "view",
            str(number),
            "--repo",
            self.repo,
            "--json",
            "number,title,body,labels,comments,state,url,author",
        )

    def pr_list(self, state: str, limit: int = 300) -> list[dict[str, Any]]:
        return self.json(
            "pr",
            "list",
            "--repo",
            self.repo,
            "--state",
            state,
            "--limit",
            str(limit),
            "--json",
            "number,title,body,labels,comments,mergedAt,state,url,isDraft,files,baseRefName",
        )

    def pr_view(self, number: int) -> dict[str, Any]:
        return self.json(
            "pr",
            "view",
            str(number),
            "--repo",
            self.repo,
            "--json",
            "number,title,body,labels,comments,mergedAt,state,url,isDraft,files,baseRefName",
        )

    def issue_comment(self, number: int, body: str) -> str:
        return self.run(
            "issue",
            "comment",
            str(number),
            "--repo",
            self.repo,
            "--body",
            body,
        ).strip()

    def issue_close(self, number: int, body: str, reason: str) -> str:
        return self.run(
            "issue",
            "close",
            str(number),
            "--repo",
            self.repo,
            "--reason",
            reason,
            "--comment",
            body,
        ).strip()

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

    def pr_close(self, number: int, body: str) -> str:
        return self.run(
            "pr",
            "close",
            str(number),
            "--repo",
            self.repo,
            "--comment",
            body,
        ).strip()

    def issue_add_labels(self, number: int, labels: list[str]) -> str:
        if not labels:
            return "skipped: no labels"
        return self.run(
            "issue",
            "edit",
            str(number),
            "--repo",
            self.repo,
            "--add-label",
            ",".join(labels),
        ).strip()

    def pr_add_labels(self, number: int, labels: list[str]) -> str:
        if not labels:
            return "skipped: no labels"
        return self.run(
            "pr",
            "edit",
            str(number),
            "--repo",
            self.repo,
            "--add-label",
            ",".join(labels),
        ).strip()


def parse_time(value: str | None) -> dt.datetime | None:
    if not value:
        return None
    return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))


def label_names(item: dict[str, Any]) -> set[str]:
    return {label["name"] for label in item.get("labels", [])}


def normalized_label_names(item: dict[str, Any]) -> set[str]:
    return {name.strip().lower() for name in label_names(item)}


def has_protected_labels(item: dict[str, Any], protected: set[str]) -> bool:
    labels = normalized_label_names(item)
    if labels & protected:
        return True
    return any(
        label.startswith(prefix)
        for label in labels
        for prefix in PROTECTED_LABEL_PREFIXES
    )


def text_of(item: dict[str, Any]) -> str:
    return f"{item.get('title') or ''}\n{item.get('body') or ''}"


def comments_text(item: dict[str, Any]) -> str:
    return "\n".join(comment.get("body") or "" for comment in item.get("comments", []))


def has_marker(item: dict[str, Any], marker: str) -> bool:
    return marker in comments_text(item)


def issue_thread_mentions_pr(issue: dict[str, Any], pr_number: int) -> bool:
    needle = f"#{pr_number}"
    return needle in text_of(issue) or needle in comments_text(issue)


def closing_refs(text: str) -> set[int]:
    return {int(match.group("number")) for match in CLOSING_RE.finditer(text)}


def issue_refs(text: str) -> set[int]:
    return {int(match.group("number")) for match in ISSUE_REF_RE.finditer(text)}


def maintainer_comments(item: dict[str, Any]) -> list[dict[str, Any]]:
    return [
        comment
        for comment in item.get("comments", [])
        if comment.get("authorAssociation") in MAINTAINER_ASSOCIATIONS
    ]


def has_non_bot_comment_after(issue: dict[str, Any], moment: dt.datetime) -> bool:
    for comment in issue.get("comments", []):
        created = parse_time(comment.get("createdAt"))
        login = (comment.get("author") or {}).get("login")
        if created and created > moment and login not in BOT_LOGINS:
            return True
    return False


def has_later_dispute(item: dict[str, Any], moment: dt.datetime) -> bool:
    dispute_re = re.compile(r"(?i)\b(not a duplicate|not duplicate|not superseded|do not close|reopen|still valid|still needed)\b")
    for comment in item.get("comments", []):
        created = parse_time(comment.get("createdAt"))
        if not created or created <= moment:
            continue
        if comment.get("authorAssociation") not in MAINTAINER_ASSOCIATIONS:
            continue
        if dispute_re.search(comment.get("body") or ""):
            return True
    return False


def build_issue_map(issues: list[dict[str, Any]]) -> dict[int, dict[str, Any]]:
    return {int(issue["number"]): issue for issue in issues}


def build_pr_map(prs: list[dict[str, Any]]) -> dict[int, dict[str, Any]]:
    return {int(pr["number"]): pr for pr in prs}


def fixed_by_merged_pr_candidates(
    open_issues: dict[int, dict[str, Any]],
    merged_prs: list[dict[str, Any]],
) -> list[Candidate]:
    candidates: list[Candidate] = []
    for pr in merged_prs:
        merged_at = parse_time(pr.get("mergedAt"))
        if not merged_at:
            continue
        for issue_number in sorted(closing_refs(text_of(pr))):
            issue = open_issues.get(issue_number)
            if not issue:
                continue
            if pr.get("baseRefName") != "master":
                candidates.append(
                    Candidate(
                        kind="fixed-by-merged-pr",
                        decision="QUEUE_FOR_REVIEW",
                        target_type="issue",
                        target=issue_number,
                        related=pr["number"],
                        summary="Merged PR has closing keyword, but it did not target `master`.",
                        evidence=[f"PR #{pr['number']} base branch is `{pr.get('baseRefName')}`."],
                        url=issue.get("url"),
                    )
                )
                continue
            if has_protected_labels(issue, ISSUE_PROTECTED_LABELS):
                candidates.append(
                    Candidate(
                        kind="fixed-by-merged-pr",
                        decision="QUEUE_FOR_REVIEW",
                        target_type="issue",
                        target=issue_number,
                        related=pr["number"],
                        summary="Merged PR has closing keyword, but issue has protected labels.",
                        evidence=[f"PR #{pr['number']} merged at {pr.get('mergedAt')}"],
                        url=issue.get("url"),
                    )
                )
                continue
            if has_non_bot_comment_after(issue, merged_at):
                candidates.append(
                    Candidate(
                        kind="fixed-by-merged-pr",
                        decision="QUEUE_FOR_REVIEW",
                        target_type="issue",
                        target=issue_number,
                        related=pr["number"],
                        summary="Merged PR has closing keyword, but issue has later activity.",
                        evidence=[f"PR #{pr['number']} merged at {pr.get('mergedAt')}"],
                        url=issue.get("url"),
                    )
                )
                continue
            candidates.append(
                Candidate(
                    kind="fixed-by-merged-pr",
                    decision="AUTO_CLOSE",
                    target_type="issue",
                    target=issue_number,
                    related=pr["number"],
                    summary=f"Issue #{issue_number} is explicitly fixed by merged PR #{pr['number']}.",
                    evidence=[
                        "PR title/body contains a closing keyword for this issue.",
                        f"PR merged at {pr.get('mergedAt')}.",
                    ],
                    action="close-fixed",
                    url=issue.get("url"),
                )
            )
    return candidates


def explicit_duplicate_candidates(
    open_issues: dict[int, dict[str, Any]],
) -> list[Candidate]:
    candidates: list[Candidate] = []
    for issue in open_issues.values():
        if has_protected_labels(issue, ISSUE_PROTECTED_LABELS):
            continue
        for comment in maintainer_comments(issue):
            for match in DUPLICATE_RE.finditer(comment.get("body") or ""):
                canonical = int(match.group("number"))
                if canonical == issue["number"]:
                    continue
                created = parse_time(comment.get("createdAt"))
                if canonical not in open_issues:
                    candidates.append(
                        Candidate(
                            kind="explicit-duplicates",
                            decision="QUEUE_FOR_REVIEW",
                            target_type="issue",
                            target=issue["number"],
                            related=canonical,
                            summary="Maintainer duplicate marker points to an issue that is not currently open.",
                            evidence=[f"Maintainer comment {comment.get('url') or ''}".strip()],
                            url=issue.get("url"),
                        )
                    )
                    break
                if created and has_later_dispute(issue, created):
                    candidates.append(
                        Candidate(
                            kind="explicit-duplicates",
                            decision="QUEUE_FOR_REVIEW",
                            target_type="issue",
                            target=issue["number"],
                            related=canonical,
                            summary="Maintainer duplicate marker has later maintainer activity that may dispute closure.",
                            evidence=[f"Maintainer comment {comment.get('url') or ''}".strip()],
                            url=issue.get("url"),
                        )
                    )
                    break
                candidates.append(
                    Candidate(
                        kind="explicit-duplicates",
                        decision="AUTO_CLOSE",
                        target_type="issue",
                        target=issue["number"],
                        related=canonical,
                        summary=f"Issue #{issue['number']} has a maintainer duplicate marker for #{canonical}.",
                        evidence=[
                            f"Maintainer comment {comment.get('url') or ''}".strip(),
                            "Duplicate marker uses explicit duplicate wording.",
                        ],
                        action="close-duplicate",
                        url=issue.get("url"),
                    )
                )
                break
            if any(c.target == issue["number"] for c in candidates):
                break
    return candidates


def superseded_pr_candidates(
    open_prs: list[dict[str, Any]],
    merged_prs: dict[int, dict[str, Any]],
) -> list[Candidate]:
    candidates: list[Candidate] = []
    for pr in open_prs:
        if has_protected_labels(pr, PR_PROTECTED_LABELS):
            continue
        sources = [("body", {"body": pr.get("body") or ""})]
        sources.extend((("comment", comment) for comment in maintainer_comments(pr)))
        for source_name, source in sources:
            for match in SUPERSEDED_RE.finditer(source.get("body") or ""):
                superseding = int(match.group("number"))
                if superseding == pr["number"] or superseding not in merged_prs:
                    continue
                created = parse_time(source.get("createdAt"))
                if pr.get("isDraft"):
                    candidates.append(
                        Candidate(
                            kind="superseded-prs",
                            decision="QUEUE_FOR_REVIEW",
                            target_type="pr",
                            target=pr["number"],
                            related=superseding,
                            summary="PR is draft and marked superseded; draft branches may contain unique work.",
                            evidence=[f"Supersession marker found in PR {source_name}."],
                            url=pr.get("url"),
                        )
                    )
                    break
                if created and has_later_dispute(pr, created):
                    candidates.append(
                        Candidate(
                            kind="superseded-prs",
                            decision="QUEUE_FOR_REVIEW",
                            target_type="pr",
                            target=pr["number"],
                            related=superseding,
                            summary="Supersession marker has later maintainer activity that may dispute closure.",
                            evidence=[f"Supersession marker found in PR {source_name}."],
                            url=pr.get("url"),
                        )
                    )
                    break
                candidates.append(
                    Candidate(
                        kind="superseded-prs",
                        decision="AUTO_CLOSE",
                        target_type="pr",
                        target=pr["number"],
                        related=superseding,
                        summary=f"PR #{pr['number']} is explicitly superseded by merged PR #{superseding}.",
                        evidence=[
                            f"Supersession marker found in PR {source_name}.",
                            f"PR #{superseding} is merged.",
                        ],
                        action="close-superseded-pr",
                        url=pr.get("url"),
                    )
                )
                break
            if any(c.target == pr["number"] for c in candidates):
                break
    return candidates


def open_pr_link_candidates(
    open_issues: dict[int, dict[str, Any]],
    open_prs: list[dict[str, Any]],
) -> list[Candidate]:
    candidates: list[Candidate] = []
    for pr in open_prs:
        if has_protected_labels(pr, PR_PROTECTED_LABELS):
            continue
        refs = closing_refs(text_of(pr))
        for issue_number in sorted(refs):
            issue = open_issues.get(issue_number)
            if not issue:
                continue
            if has_protected_labels(issue, ISSUE_PROTECTED_LABELS):
                continue
            if issue_thread_mentions_pr(issue, pr["number"]):
                continue
            candidates.append(
                Candidate(
                    kind="open-pr-links",
                    decision="AUTO_COMMENT",
                    target_type="issue",
                    target=issue_number,
                    related=pr["number"],
                    summary=f"Open PR #{pr['number']} explicitly claims to close issue #{issue_number}.",
                    evidence=["PR title/body contains closing keyword.", "Issue thread does not mention this PR."],
                    action="comment-open-pr-link",
                    url=issue.get("url"),
                )
            )
    return candidates


STOPWORDS = {
    "the", "and", "for", "with", "that", "this", "from", "into", "when", "then",
    "have", "has", "are", "was", "were", "not", "but", "you", "your", "issue",
    "bug", "fix", "feat", "docs", "summary", "changed", "why", "scope", "linked",
    "pr", "pull", "request", "zero", "zeroclaw",
}
WORD_RE = re.compile(r"[a-z0-9_./:-]{3,}")
BACKTICK_RE = re.compile(r"`([^`\n]{3,140})`")
API_ROUTE_RE = re.compile(r"(?<!\w)(/api/[A-Za-z0-9_/{}/:?.=&.-]+)")
CLI_COMMAND_RE = re.compile(r"\b(zeroclaw\s+[a-z0-9][a-z0-9 _./:-]{1,100})")
CONFIG_SECTION_RE = re.compile(r"(\[[A-Za-z0-9_.-]+\])")
SNAKE_IDENTIFIER_RE = re.compile(r"\b([a-z][a-z0-9]+(?:_[a-z0-9]+)+)\b")
IMPLEMENTED_HINT_RE = re.compile(
    r"(?i)\b(missing|not found|no such|unknown|unrecognized|does not exist|doesn't exist|"
    r"does not know|can't find|cannot find|add|support|implement|already fixed|fixed on master)\b"
)
CONFIG_HINT_RE = re.compile(r"(?i)\b(config|setting|option|field|parameter|schema|toml)\b")


@dataclass(frozen=True)
class SymbolEvidence:
    kind: str
    symbol: str
    matches: tuple[str, ...]


def tokens(text: str) -> Counter[str]:
    out: Counter[str] = Counter()
    for raw in WORD_RE.findall(text.lower()):
        token = raw.strip("`*_.,;:()[]{}<>")
        if token in STOPWORDS or token.isdigit() or token.startswith("http"):
            continue
        out[token] += 1
    return out


def cosine(a: dict[str, float], b: dict[str, float]) -> float:
    if not a or not b:
        return 0.0
    if len(a) > len(b):
        a, b = b, a
    dot = sum(value * b.get(key, 0.0) for key, value in a.items())
    norm_a = math.sqrt(sum(value * value for value in a.values()))
    norm_b = math.sqrt(sum(value * value for value in b.values()))
    return dot / (norm_a * norm_b) if norm_a and norm_b else 0.0


def similarity_preview_candidates(
    open_issues: dict[int, dict[str, Any]],
    open_prs: list[dict[str, Any]],
    threshold: float,
    max_candidates: int,
) -> list[Candidate]:
    docs: list[tuple[str, int, Counter[str]]] = []
    for issue in open_issues.values():
        docs.append(("issue", issue["number"], tokens(text_of(issue))))
    for pr in open_prs:
        docs.append(("pr", pr["number"], tokens(text_of(pr))))

    doc_freq: Counter[str] = Counter()
    for _, _, counts in docs:
        for token in counts:
            doc_freq[token] += 1
    total = len(docs)
    idf = {token: math.log((total + 1) / (freq + 1)) + 1 for token, freq in doc_freq.items()}

    def vector(counts: Counter[str]) -> dict[str, float]:
        return {token: count * idf.get(token, 1.0) for token, count in counts.items()}

    issue_vectors = {
        number: vector(tokens(text_of(issue)))
        for number, issue in open_issues.items()
    }
    rows: list[tuple[float, dict[str, Any], dict[str, Any]]] = []
    for pr in open_prs:
        pr_text = text_of(pr)
        pr_refs = issue_refs(pr_text)
        pr_vec = vector(tokens(pr_text))
        for issue_number, issue in open_issues.items():
            if issue_number in pr_refs or issue_thread_mentions_pr(issue, pr["number"]):
                continue
            score = cosine(pr_vec, issue_vectors[issue_number])
            if score >= threshold:
                rows.append((score, pr, issue))

    rows.sort(key=lambda row: row[0], reverse=True)
    candidates: list[Candidate] = []
    for score, pr, issue in rows[:max_candidates]:
        candidates.append(
            Candidate(
                kind="similarity-preview",
                decision="QUEUE_FOR_REVIEW",
                target_type="issue",
                target=issue["number"],
                related=pr["number"],
                summary=f"PR #{pr['number']} and issue #{issue['number']} have high text similarity ({score:.3f}).",
                evidence=[
                    "Similarity-only candidates are discovery hints, never autonomous actions.",
                    f"PR: {pr.get('title')}",
                    f"Issue: {issue.get('title')}",
                ],
                url=issue.get("url"),
            )
        )
    return candidates


def extract_issue_symbols(issue: dict[str, Any], max_symbols: int) -> list[tuple[str, str]]:
    text = text_of(issue)
    symbols: list[tuple[str, str]] = []

    def add(kind: str, raw: str) -> None:
        symbol = normalize_symbol(raw)
        if not symbol:
            return
        item = (kind, symbol)
        if item not in symbols:
            symbols.append(item)

    for match in API_ROUTE_RE.finditer(text):
        add("api-route", match.group(1))
    for match in CLI_COMMAND_RE.finditer(text):
        add("cli-command", match.group(1))
    for match in CONFIG_SECTION_RE.finditer(text):
        add("config-section", match.group(1))

    for match in BACKTICK_RE.finditer(text):
        value = match.group(1).strip()
        if value.startswith("/api/"):
            add("api-route", value)
        elif value.lower().startswith("zeroclaw "):
            add("cli-command", value)
        elif value.startswith("[") and value.endswith("]"):
            add("config-section", value)
        elif CONFIG_HINT_RE.search(text) and SNAKE_IDENTIFIER_RE.fullmatch(value):
            add("config-key", value)
        elif IMPLEMENTED_HINT_RE.search(text) and SNAKE_IDENTIFIER_RE.fullmatch(value):
            add("identifier", value)

    if CONFIG_HINT_RE.search(text):
        for match in SNAKE_IDENTIFIER_RE.finditer(text):
            add("config-key", match.group(1))

    priority = {"api-route": 0, "cli-command": 1, "config-section": 2, "config-key": 3, "identifier": 4}
    symbols.sort(key=lambda item: (priority.get(item[0], 99), len(item[1])))
    return symbols[:max_symbols]


def normalize_symbol(raw: str) -> str | None:
    symbol = raw.strip().strip("`'\".,;:()<>")
    symbol = re.sub(r"\s+", " ", symbol)
    if symbol.lower().startswith("zeroclaw "):
        symbol = re.sub(r"\s+-+$", "", symbol)
    if len(symbol) < 3 or len(symbol) > 120:
        return None
    if symbol in {"[Bug]", "[Feature]", "[...]"}:
        return None
    if symbol.startswith("[") and symbol.endswith("]"):
        inner = symbol[1:-1].strip()
        if len(inner) < 2 or inner.lower() == "x":
            return None
    if symbol in NOISY_IDENTIFIERS:
        return None
    if re.fullmatch(r"\[[.\s]+\]", symbol):
        return None
    return symbol


def rg_literal(symbol: str, max_matches: int) -> tuple[str, ...]:
    cmd = [
        "rg",
        "--line-number",
        "--fixed-strings",
        "--glob",
        "!.git",
        "--glob",
        "!target",
        "--glob",
        "!artifacts",
        "--glob",
        "!docs/book/book",
        symbol,
        ".",
    ]
    try:
        result = subprocess.run(
            cmd,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )
    except FileNotFoundError:
        return ()
    if result.returncode not in {0, 1}:
        return ()
    matches: list[str] = []
    for line in result.stdout.splitlines():
        if "/.claude/skills/factory-clerk/" in line:
            continue
        if line.startswith("./.git/") or line.startswith("./artifacts/"):
            continue
        matches.append(line)
        if len(matches) >= max_matches:
            break
    return tuple(matches)


def implemented_on_master_preview_candidates(
    open_issues: dict[int, dict[str, Any]],
    max_candidates: int,
    max_symbols_per_issue: int,
    max_matches_per_symbol: int,
) -> list[Candidate]:
    candidates: list[Candidate] = []
    for issue in open_issues.values():
        if has_protected_labels(issue, ISSUE_PROTECTED_LABELS):
            continue
        text = text_of(issue)
        if not IMPLEMENTED_HINT_RE.search(text):
            continue

        evidence: list[SymbolEvidence] = []
        for kind, symbol in extract_issue_symbols(issue, max_symbols_per_issue):
            matches = rg_literal(symbol, max_matches_per_symbol)
            if matches:
                evidence.append(SymbolEvidence(kind, symbol, matches))

        if not evidence:
            continue

        strong = [item for item in evidence if item.kind in {"api-route", "cli-command", "config-section", "config-key"}]
        if not strong:
            continue

        rendered: list[str] = []
        for item in strong[:4]:
            rendered.append(f"{item.kind} `{item.symbol}` found in checkout:")
            rendered.extend(f"  {match}" for match in item.matches[:max_matches_per_symbol])

        candidates.append(
            Candidate(
                kind="implemented-on-master-preview",
                decision="QUEUE_FOR_REVIEW",
                target_type="issue",
                target=issue["number"],
                related=None,
                summary=(
                    f"Issue #{issue['number']} mentions concrete symbols that already exist in the checked-out codebase; "
                    "review whether this is already implemented on master."
                ),
                evidence=rendered,
                url=issue.get("url"),
            )
        )
        if len(candidates) >= max_candidates:
            break
    return candidates


def render_comment(candidate: Candidate) -> tuple[str, str]:
    prefix = f"{candidate.marker}\n"
    if candidate.action == "close-fixed":
        return (
            "completed",
            prefix
            + f"Closing as fixed by #{candidate.related}, merged into `master`.\n\n"
            "The PR explicitly links this issue with a closing keyword, and no later issue activity indicates the fix missed the reported scope.",
        )
    if candidate.action == "close-duplicate":
        return (
            "not planned",
            prefix
            + f"Closing as duplicate of #{candidate.related}.\n\n"
            f"A maintainer comment already identified the duplicate relationship, and #{candidate.related} is the better canonical tracker.",
        )
    if candidate.action == "close-superseded-pr":
        return (
            "",
            prefix
            + f"Closing as superseded by #{candidate.related}, which has already merged.\n\n"
            "The superseding PR carries the lifecycle path forward. Please reopen or follow up if this branch contains unique scope that still needs review.",
        )
    if candidate.action == "comment-open-pr-link":
        return (
            "",
            prefix
            + f"PR coverage link: #{candidate.related} explicitly links this issue with a closing keyword.\n\n"
            f"Leaving this open until #{candidate.related} merges; GitHub should close it automatically if the closing reference remains in the merged PR body.",
        )
    raise ValueError(f"No renderer for action {candidate.action}")


def apply_candidate(gh: Gh, candidate: Candidate, mode: str, labels: list[str]) -> str:
    if candidate.decision == "AUTO_COMMENT" and mode in {"comment-only", "apply-safe"}:
        _, body = render_comment(candidate)
        if candidate.target_type != "issue":
            return "skipped: comment target is not issue"
        comment_result = gh.issue_comment(candidate.target, body)
        if labels:
            gh.issue_add_labels(candidate.target, labels)
        return comment_result

    if candidate.decision == "AUTO_CLOSE" and mode == "apply-safe":
        reason, body = render_comment(candidate)
        if candidate.target_type == "issue":
            if labels:
                gh.issue_add_labels(candidate.target, labels)
            return gh.issue_close(candidate.target, body, reason)
        if candidate.target_type == "pr":
            if labels:
                gh.pr_add_labels(candidate.target, labels)
            return gh.pr_close(candidate.target, body)
    return "skipped"


def collect_candidates(
    gh: Gh,
    checks: set[str],
    merged_limit: int,
    open_limit: int,
    similarity_threshold: float,
    similarity_max: int,
    implemented_max: int,
    implemented_symbol_max: int,
    implemented_match_max: int,
    include_marked: bool,
) -> list[Candidate]:
    open_issues_list = gh.issue_list("open", open_limit)
    open_issues = build_issue_map(open_issues_list)
    open_prs = gh.pr_list("open", open_limit)
    merged_prs = gh.pr_list("merged", merged_limit)
    merged_pr_map = build_pr_map(merged_prs)

    candidates: list[Candidate] = []
    if "fixed-by-merged-pr" in checks:
        candidates.extend(fixed_by_merged_pr_candidates(open_issues, merged_prs))
    if "explicit-duplicates" in checks:
        candidates.extend(explicit_duplicate_candidates(open_issues))
    if "superseded-prs" in checks:
        candidates.extend(superseded_pr_candidates(open_prs, merged_pr_map))
    if "open-pr-links" in checks:
        candidates.extend(open_pr_link_candidates(open_issues, open_prs))
    if "implemented-on-master-preview" in checks:
        candidates.extend(
            implemented_on_master_preview_candidates(
                open_issues,
                implemented_max,
                implemented_symbol_max,
                implemented_match_max,
            )
        )
    if "similarity-preview" in checks:
        candidates.extend(
            similarity_preview_candidates(
                open_issues,
                open_prs,
                similarity_threshold,
                similarity_max,
            )
        )
    candidates = dedupe_candidates(candidates)
    if include_marked:
        return candidates
    return [
        candidate
        for candidate in candidates
        if not has_existing_marker(candidate, open_issues, open_prs)
    ]


def has_existing_marker(
    candidate: Candidate,
    open_issues: dict[int, dict[str, Any]],
    open_prs: list[dict[str, Any]],
) -> bool:
    if candidate.target_type == "issue":
        target = open_issues.get(candidate.target)
    elif candidate.target_type == "pr":
        target = build_pr_map(open_prs).get(candidate.target)
    else:
        target = None
    return bool(target and has_marker(target, candidate.marker))


def dedupe_candidates(candidates: list[Candidate]) -> list[Candidate]:
    seen: set[tuple[str, str, int, int | None, str | None]] = set()
    result: list[Candidate] = []
    for candidate in candidates:
        key = (
            candidate.kind,
            candidate.target_type,
            candidate.target,
            candidate.related,
            candidate.action,
        )
        if key in seen:
            continue
        seen.add(key)
        result.append(candidate)
    order = {"AUTO_CLOSE": 0, "AUTO_COMMENT": 1, "QUEUE_FOR_REVIEW": 2, "NO_ACTION": 3}
    result.sort(key=lambda c: (order.get(c.decision, 9), c.kind, c.target))
    return result


def candidate_to_dict(candidate: Candidate) -> dict[str, Any]:
    return {
        "kind": candidate.kind,
        "decision": candidate.decision,
        "target_type": candidate.target_type,
        "target": candidate.target,
        "related": candidate.related,
        "summary": candidate.summary,
        "evidence": candidate.evidence,
        "action": candidate.action,
        "url": candidate.url,
    }


def write_audit(repo: str, mode: str, candidates: list[Candidate], results: list[dict[str, Any]], output_dir: Path) -> Path:
    output_dir.mkdir(parents=True, exist_ok=True)
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    path = output_dir / f"factory-clerk-{stamp}.json"
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
        "# Factory Clerk Summary",
        "",
        f"- Candidates: {len(candidates)}",
        f"- AUTO_CLOSE: {counts['AUTO_CLOSE']}",
        f"- AUTO_COMMENT: {counts['AUTO_COMMENT']}",
        f"- QUEUE_FOR_REVIEW: {counts['QUEUE_FOR_REVIEW']}",
        f"- Mutations: {len(results)}",
        "",
        "## Candidates",
        "",
    ]
    for candidate in candidates:
        related = f" related #{candidate.related}" if candidate.related else ""
        lines.append(f"- **{candidate.decision}** `{candidate.kind}` {candidate.target_type} #{candidate.target}{related}: {candidate.summary}")
    if results:
        lines.extend(["", "## Mutations", ""])
        for result in results:
            lines.append(f"- {result['target_type']} #{result['target']} `{result['action']}`: {result['status']}")
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def print_summary(candidates: list[Candidate], results: list[dict[str, Any]]) -> None:
    counts = Counter(candidate.decision for candidate in candidates)
    kind_counts = Counter(candidate.kind for candidate in candidates)
    print("Factory Clerk summary")
    print("=======================")
    print(f"Candidates: {len(candidates)}")
    for decision in ("AUTO_CLOSE", "AUTO_COMMENT", "QUEUE_FOR_REVIEW", "NO_ACTION"):
        if counts[decision]:
            print(f"- {decision}: {counts[decision]}")
    if kind_counts:
        print("\nBy check:")
        for kind, count in sorted(kind_counts.items()):
            print(f"- {kind}: {count}")
    if results:
        print("\nMutations:")
        for result in results:
            print(f"- {result['target_type']} #{result['target']}: {result['status']}")
    print("\nCandidates:")
    for candidate in candidates:
        related = f" related=#{candidate.related}" if candidate.related else ""
        print(f"- [{candidate.decision}] {candidate.kind}: {candidate.target_type} #{candidate.target}{related}")
        print(f"  {candidate.summary}")
        for line in candidate.evidence[:6]:
            print(f"  - {line}")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY", DEFAULT_REPO))
    parser.add_argument(
        "--mode",
        choices=("preview", "comment-only", "apply-safe"),
        default="preview",
        help="preview never mutates; comment-only posts safe link comments; apply-safe also closes safe targets",
    )
    parser.add_argument(
        "--checks",
        default=",".join(DEFAULT_CHECKS),
        help=f"Comma-separated checks. Available: {', '.join(DEFAULT_CHECKS)}",
    )
    parser.add_argument("--merged-limit", type=int, default=250)
    parser.add_argument("--open-limit", type=int, default=1000)
    parser.add_argument("--similarity-threshold", type=float, default=0.42)
    parser.add_argument("--similarity-max", type=int, default=25)
    parser.add_argument(
        "--implemented-max",
        type=int,
        default=25,
        help="Maximum implemented-on-master preview candidates",
    )
    parser.add_argument(
        "--implemented-symbol-max",
        type=int,
        default=8,
        help="Maximum symbols to search per issue for implemented-on-master preview",
    )
    parser.add_argument(
        "--implemented-match-max",
        type=int,
        default=3,
        help="Maximum rg matches to retain per symbol",
    )
    parser.add_argument(
        "--include-marked",
        action="store_true",
        help="Include candidates already marked by a previous factory-clerk comment",
    )
    parser.add_argument(
        "--max-mutations",
        type=int,
        default=25,
        help="Maximum comments/closures in one non-preview run",
    )
    parser.add_argument(
        "--label",
        action="append",
        default=[],
        help="Optional label to add to mutated issues/PRs. Repeat for multiple labels.",
    )
    parser.add_argument(
        "--audit-dir",
        default="artifacts/factory-clerk",
        help="Directory for JSON audit output",
    )
    parser.add_argument(
        "--summary-file",
        help="Optional Markdown summary output path, useful for GitHub step summaries",
    )
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
    candidates = collect_candidates(
        gh,
        checks,
        args.merged_limit,
        args.open_limit,
        args.similarity_threshold,
        args.similarity_max,
        args.implemented_max,
        args.implemented_symbol_max,
        args.implemented_match_max,
        args.include_marked,
    )

    results: list[dict[str, Any]] = []
    if args.mode != "preview":
        mutation_count = 0
        for candidate in candidates:
            if candidate.decision not in {"AUTO_COMMENT", "AUTO_CLOSE"}:
                continue
            if mutation_count >= args.max_mutations:
                status = "skipped: max mutations reached"
            else:
                status = apply_candidate(gh, candidate, args.mode, args.label)
                if not status.startswith("skipped"):
                    mutation_count += 1
            results.append(
                {
                    "target_type": candidate.target_type,
                    "target": candidate.target,
                    "related": candidate.related,
                    "action": candidate.action,
                    "status": status,
                }
            )

    print_summary(candidates, results)
    if args.summary_file:
        write_markdown_summary(candidates, results, Path(args.summary_file))
    if not args.no_audit_file:
        path = write_audit(args.repo, args.mode, candidates, results, Path(args.audit_dir))
        print(f"\nAudit file: {path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
