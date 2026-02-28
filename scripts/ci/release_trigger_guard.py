#!/usr/bin/env python3
"""Validate release trigger authorization and publish-tag provenance."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

STABLE_TAG_RE = re.compile(r"^v(?P<version>\d+\.\d+\.\d+)$")
TRUE_VALUES = {"1", "true", "yes", "on"}


def parse_bool(raw: str) -> bool:
    return raw.strip().lower() in TRUE_VALUES


def parse_csv(raw: str) -> list[str]:
    return [item.strip() for item in raw.split(",") if item.strip()]


def normalize_email(raw: str) -> str:
    value = raw.strip().lower()
    if value.startswith("<") and value.endswith(">") and len(value) > 2:
        value = value[1:-1]
    return value


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


def build_markdown(report: dict) -> str:
    lines: list[str] = []
    lines.append("# Release Trigger Guard Report")
    lines.append("")
    lines.append(f"- Generated at: `{report['generated_at']}`")
    lines.append(f"- Event: `{report['event_name']}`")
    lines.append(f"- Actor: `{report['actor']}`")
    lines.append(f"- Publish release: `{report['publish_release']}`")
    lines.append(f"- Release ref: `{report['release_ref']}`")
    lines.append(f"- Release tag: `{report['release_tag']}`")
    lines.append(f"- Ready to publish: `{report['ready_to_publish']}`")
    lines.append("")

    lines.append("## Authorization")
    lines.append(f"- Actor authorized: `{report['authorization']['actor_authorized']}`")
    lines.append(f"- Tagger authorized: `{report['authorization']['tagger_authorized']}`")
    actors = report["policy"].get("authorized_actors", [])
    if actors:
        lines.append(f"- Authorized actors: {', '.join(f'`{item}`' for item in actors)}")
    else:
        lines.append("- Authorized actors: none configured")
    lines.append("")

    lines.append("## Tag Metadata")
    metadata = report.get("tag_metadata", {})
    lines.append(f"- Tag exists on origin: `{metadata.get('tag_exists')}`")
    lines.append(f"- Tag object type: `{metadata.get('tag_object_type')}`")
    lines.append(f"- Annotated tag: `{metadata.get('annotated_tag')}`")
    lines.append(f"- Tag commit: `{metadata.get('tag_commit')}`")
    lines.append(f"- Tagger: `{metadata.get('tagger_name')}` / `{metadata.get('tagger_email')}`")
    lines.append(f"- Cargo version: `{metadata.get('cargo_version')}`")
    lines.append(f"- Tag version: `{metadata.get('tag_version')}`")
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
    parser = argparse.ArgumentParser(description="Validate release trigger authorization and tag provenance.")
    parser.add_argument("--repo-root", default=".")
    parser.add_argument("--repository", required=True, help="Repository slug (owner/repo).")
    parser.add_argument(
        "--origin-url",
        default="",
        help="Optional explicit origin URL/path (used in tests/local verification).",
    )
    parser.add_argument("--event-name", required=True)
    parser.add_argument("--actor", required=True)
    parser.add_argument("--release-ref", required=True)
    parser.add_argument("--release-tag", required=True)
    parser.add_argument("--publish-release", required=True, help="Boolean value (true/false).")
    parser.add_argument("--authorized-actors", default="")
    parser.add_argument("--authorized-tagger-emails", default="")
    parser.add_argument("--require-annotated-tag", default="true")
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--fail-on-violation", action="store_true")
    args = parser.parse_args()

    repo_root = Path(args.repo_root).resolve()
    out_json = Path(args.output_json)
    out_md = Path(args.output_md)

    publish_release = parse_bool(args.publish_release)
    require_annotated_tag = parse_bool(args.require_annotated_tag)
    authorized_actors = parse_csv(args.authorized_actors)
    authorized_tagger_emails = [normalize_email(item) for item in parse_csv(args.authorized_tagger_emails)]

    violations: list[str] = []
    warnings: list[str] = []

    actor_authorized: bool | None = None
    tagger_authorized: bool | None = None
    tag_exists = False
    tag_object_type: str | None = None
    annotated_tag: bool | None = None
    tag_commit: str | None = None
    tagger_name: str | None = None
    tagger_email: str | None = None
    tagger_date: str | None = None
    cargo_version: str | None = None
    tag_version: str | None = None

    if publish_release:
        stable_match = STABLE_TAG_RE.fullmatch(args.release_tag)
        if not stable_match:
            violations.append(
                f"Release tag `{args.release_tag}` must match stable format `vX.Y.Z`; "
                "pre-release tags belong to Pub Pre-release workflow."
            )
        else:
            tag_version = stable_match.group("version")

        if args.release_ref != args.release_tag:
            violations.append(
                f"Publish mode requires release_ref to equal release_tag; got `{args.release_ref}` vs `{args.release_tag}`."
            )

        if not authorized_actors:
            violations.append(
                "No authorized publish actors configured. Set `RELEASE_AUTHORIZED_ACTORS` repository variable."
            )
            actor_authorized = False
        else:
            actor_authorized = args.actor in authorized_actors
            if not actor_authorized:
                violations.append(
                    f"Actor `{args.actor}` is not authorized to trigger release publish. "
                    f"Allowed actors: {', '.join(authorized_actors)}."
                )

        origin_url = args.origin_url.strip() or f"https://github.com/{args.repository}.git"
        ls_remote = subprocess.run(
            ["git", "ls-remote", "--tags", origin_url],
            text=True,
            capture_output=True,
            check=False,
        )
        if ls_remote.returncode != 0:
            violations.append(f"Failed to list origin tags from `{origin_url}`: {ls_remote.stderr.strip()}")
        else:
            refs = set()
            for line in ls_remote.stdout.splitlines():
                parts = line.split()
                if len(parts) >= 2:
                    refs.add(parts[1].strip())
            tag_ref = f"refs/tags/{args.release_tag}"
            peeled_ref = f"{tag_ref}^{{}}"
            tag_exists = tag_ref in refs or peeled_ref in refs
            if not tag_exists:
                origin_path = Path(origin_url)
                if origin_path.exists():
                    local_ref = subprocess.run(
                        ["git", "-C", str(origin_path), "show-ref", "--verify", tag_ref],
                        text=True,
                        capture_output=True,
                        check=False,
                    )
                    if local_ref.returncode == 0:
                        tag_exists = True
                        warnings.append(
                            f"Resolved tag `{args.release_tag}` via local origin-path fallback (`{origin_path}`)."
                        )
            if not tag_exists:
                violations.append(
                    f"Tag `{args.release_tag}` does not exist on origin `{origin_url}`. Push tag first."
                )

        if tag_exists:
            with tempfile.TemporaryDirectory(prefix="zc-release-trigger-guard-") as tmp_dir:
                tmp_repo = Path(tmp_dir)
                try:
                    run_git(["init", "-q"], cwd=tmp_repo)
                    run_git(["remote", "add", "origin", origin_url], cwd=tmp_repo)
                    run_git(
                        [
                            "fetch",
                            "--quiet",
                            "--filter=blob:none",
                            "origin",
                            "main",
                            f"refs/tags/{args.release_tag}:refs/tags/{args.release_tag}",
                        ],
                        cwd=tmp_repo,
                    )
                except RuntimeError as exc:
                    violations.append(f"Failed to fetch release refs for guard validation: {exc}")
                else:
                    try:
                        tag_object_type = run_git(
                            ["cat-file", "-t", f"refs/tags/{args.release_tag}"],
                            cwd=tmp_repo,
                        )
                        annotated_tag = tag_object_type == "tag"
                    except RuntimeError as exc:
                        violations.append(f"Failed to inspect tag object type for `{args.release_tag}`: {exc}")

                    if require_annotated_tag and annotated_tag is False:
                        violations.append(
                            f"Release tag `{args.release_tag}` must be an annotated tag (lightweight tags are not allowed)."
                        )

                    try:
                        tag_commit = run_git(
                            ["rev-list", "-n", "1", f"refs/tags/{args.release_tag}"],
                            cwd=tmp_repo,
                        )
                    except RuntimeError as exc:
                        violations.append(f"Failed to resolve commit for tag `{args.release_tag}`: {exc}")

                    ancestor_check = subprocess.run(
                        ["git", "merge-base", "--is-ancestor", f"refs/tags/{args.release_tag}", "origin/main"],
                        cwd=str(tmp_repo),
                        text=True,
                        capture_output=True,
                        check=False,
                    )
                    if ancestor_check.returncode != 0:
                        violations.append(
                            f"Tag `{args.release_tag}` is not reachable from `origin/main`; release tags must be cut from main."
                        )

                    try:
                        cargo_toml = run_git(["show", f"refs/tags/{args.release_tag}:Cargo.toml"], cwd=tmp_repo)
                        for line in cargo_toml.splitlines():
                            stripped = line.strip()
                            if stripped.startswith("version = "):
                                cargo_version = stripped.split('"', 2)[1]
                                break
                    except RuntimeError as exc:
                        violations.append(f"Failed to inspect Cargo.toml at `{args.release_tag}`: {exc}")

                    if tag_version and cargo_version and tag_version != cargo_version:
                        violations.append(
                            f"Tag `{args.release_tag}` version `{tag_version}` does not match Cargo.toml version `{cargo_version}`."
                        )

                    if annotated_tag:
                        try:
                            tagger_raw = run_git(
                                [
                                    "for-each-ref",
                                    "--format=%(taggername)|%(taggeremail)|%(taggerdate:iso8601)",
                                    f"refs/tags/{args.release_tag}",
                                ],
                                cwd=tmp_repo,
                            )
                            if tagger_raw:
                                parts = tagger_raw.split("|", 2)
                                if len(parts) == 3:
                                    tagger_name = parts[0] or None
                                    tagger_email = parts[1] or None
                                    tagger_date = parts[2] or None
                        except RuntimeError as exc:
                            warnings.append(f"Failed to inspect tagger metadata for `{args.release_tag}`: {exc}")

                    if authorized_tagger_emails:
                        normalized_tagger = normalize_email(tagger_email or "")
                        if not normalized_tagger:
                            tagger_authorized = False
                            violations.append(
                                f"Tag `{args.release_tag}` has no tagger email metadata but tagger allowlist is enforced."
                            )
                        else:
                            tagger_authorized = normalized_tagger in authorized_tagger_emails
                            if not tagger_authorized:
                                violations.append(
                                    f"Tagger email `{normalized_tagger}` is not authorized. "
                                    f"Allowed tagger emails: {', '.join(authorized_tagger_emails)}."
                                )
                    else:
                        tagger_authorized = None
                        warnings.append(
                            "No authorized tagger email list configured; tagger authorization check skipped."
                        )
    else:
        warnings.append("Verification mode detected (`publish_release=false`); publish-trigger authorization skipped.")

    ready_to_publish = publish_release and not violations

    report = {
        "schema_version": "zeroclaw.release-trigger-guard.v1",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "event_name": args.event_name,
        "actor": args.actor,
        "release_ref": args.release_ref,
        "release_tag": args.release_tag,
        "publish_release": publish_release,
        "ready_to_publish": ready_to_publish,
        "policy": {
            "stable_tag_pattern": STABLE_TAG_RE.pattern,
            "require_annotated_tag": require_annotated_tag,
            "authorized_actors": authorized_actors,
            "authorized_tagger_emails": authorized_tagger_emails,
        },
        "authorization": {
            "actor_authorized": actor_authorized,
            "tagger_authorized": tagger_authorized,
        },
        "tag_metadata": {
            "tag_exists": tag_exists,
            "tag_object_type": tag_object_type,
            "annotated_tag": annotated_tag,
            "tag_commit": tag_commit,
            "tagger_name": tagger_name,
            "tagger_email": normalize_email(tagger_email or "") if tagger_email else None,
            "tagger_date": tagger_date,
            "tag_version": tag_version,
            "cargo_version": cargo_version,
        },
        "trigger_provenance": {
            "repository": args.repository,
            "origin_url": args.origin_url.strip() or f"https://github.com/{args.repository}.git",
            "workflow": os.environ.get("GITHUB_WORKFLOW"),
            "run_id": os.environ.get("GITHUB_RUN_ID"),
            "run_attempt": os.environ.get("GITHUB_RUN_ATTEMPT"),
            "sha": os.environ.get("GITHUB_SHA"),
            "ref": os.environ.get("GITHUB_REF"),
            "ref_name": os.environ.get("GITHUB_REF_NAME"),
            "actor": os.environ.get("GITHUB_ACTOR"),
        },
        "warnings": warnings,
        "violations": violations,
    }

    out_json.parent.mkdir(parents=True, exist_ok=True)
    out_md.parent.mkdir(parents=True, exist_ok=True)
    out_json.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    out_md.write_text(build_markdown(report), encoding="utf-8")

    if args.fail_on_violation and violations:
        print("release trigger guard violations found:", file=sys.stderr)
        for item in violations:
            print(f"- {item}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
