#!/usr/bin/env python3
"""Validate GHCR publish tag contract and emit rollback mapping evidence."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

POLICY_SCHEMA = "zeroclaw.ghcr-tag-policy.v1"
ACCEPT_HEADER = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.v2+json"


def load_policy(path: Path) -> tuple[dict[str, object], list[str]]:
    violations: list[str] = []
    raw = json.loads(path.read_text(encoding="utf-8"))

    def ensure_string(name: str) -> str:
        value = raw.get(name)
        if not isinstance(value, str) or not value.strip():
            violations.append(f"Policy field `{name}` must be a non-empty string.")
            return ""
        return value.strip()

    def ensure_positive_int(name: str) -> int:
        value = raw.get(name)
        if not isinstance(value, int) or value <= 0:
            violations.append(f"Policy field `{name}` must be a positive integer.")
            return 0
        return value

    def ensure_bool(name: str) -> bool:
        value = raw.get(name)
        if not isinstance(value, bool):
            violations.append(f"Policy field `{name}` must be a boolean.")
            return False
        return value

    def ensure_string_list(name: str, *, allowed: set[str] | None = None) -> list[str]:
        value = raw.get(name)
        if not isinstance(value, list) or not value:
            violations.append(f"Policy field `{name}` must be a non-empty array.")
            return []
        seen: set[str] = set()
        out: list[str] = []
        for item in value:
            if not isinstance(item, str) or not item.strip():
                violations.append(f"Policy field `{name}` contains invalid entry.")
                continue
            text = item.strip()
            if text in seen:
                violations.append(f"Policy field `{name}` contains duplicate entry `{text}`.")
                continue
            if allowed is not None and text not in allowed:
                allowed_sorted = ", ".join(sorted(allowed))
                violations.append(
                    f"Policy field `{name}` contains unsupported value `{text}`. Allowed: {allowed_sorted}."
                )
                continue
            out.append(text)
            seen.add(text)
        return out

    schema_version = ensure_string("schema_version")
    if schema_version and schema_version != POLICY_SCHEMA:
        violations.append(f"Policy schema_version must be `{POLICY_SCHEMA}`, got `{schema_version}`.")

    release_tag_regex = ensure_string("release_tag_regex")
    if release_tag_regex:
        try:
            re.compile(release_tag_regex)
        except re.error as exc:
            violations.append(f"Policy field `release_tag_regex` is invalid: {exc}.")

    contract_artifact_retention_days = ensure_positive_int("contract_artifact_retention_days")
    scan_artifact_retention_days = ensure_positive_int("scan_artifact_retention_days")

    policy = {
        "schema_version": schema_version,
        "release_tag_regex": release_tag_regex,
        "sha_tag_prefix": ensure_string("sha_tag_prefix"),
        "sha_tag_length": ensure_positive_int("sha_tag_length"),
        "latest_tag": ensure_string("latest_tag"),
        "require_latest_on_release": ensure_bool("require_latest_on_release"),
        "immutable_tag_classes": ensure_string_list(
            "immutable_tag_classes",
            allowed={"release", "sha", "latest"},
        ),
        "rollback_priority": ensure_string_list(
            "rollback_priority",
            allowed={"release", "sha", "latest"},
        ),
        "contract_artifact_retention_days": contract_artifact_retention_days,
        "scan_artifact_retention_days": scan_artifact_retention_days,
    }

    # Keep this invariant explicit to avoid ambiguous rollback ordering.
    if policy["require_latest_on_release"] and "latest" not in policy["rollback_priority"]:
        # This is advisory only; latest is mutable and normally not first rollback candidate.
        pass

    return policy, violations


def resolve_tags(policy: dict[str, object], *, release_tag: str, sha: str) -> tuple[dict[str, str], list[str]]:
    violations: list[str] = []

    if not re.fullmatch(r"[0-9a-fA-F]{12,64}", sha):
        violations.append("Input `sha` must be a 12-64 length hex string.")

    release_regex = str(policy["release_tag_regex"])
    if release_regex and not re.fullmatch(release_regex, release_tag):
        violations.append(
            f"Release tag `{release_tag}` does not match policy regex `{release_regex}`."
        )

    sha_tag_prefix = str(policy["sha_tag_prefix"])
    sha_tag_length = int(policy["sha_tag_length"])
    sha_tag = f"{sha_tag_prefix}{sha[:sha_tag_length].lower()}"

    tags = {
        "release": release_tag,
        "sha": sha_tag,
        "latest": str(policy["latest_tag"]),
    }
    return tags, violations


def fetch_ghcr_token(repository: str) -> tuple[str | None, str | None]:
    qs = urllib.parse.urlencode({"scope": f"repository:{repository}:pull"})
    url = f"https://ghcr.io/token?{qs}"
    try:
        with urllib.request.urlopen(url, timeout=20) as response:
            payload = json.loads(response.read().decode("utf-8"))
    except Exception as exc:  # noqa: BLE001
        return None, f"Failed to fetch GHCR token: {exc}"

    token = payload.get("token")
    if not isinstance(token, str) or not token:
        return None, "GHCR token response did not include a usable `token` field."
    return token, None


def fetch_manifest(repository: str, tag: str, token: str) -> dict[str, object]:
    url = f"https://ghcr.io/v2/{repository}/manifests/{urllib.parse.quote(tag, safe='')}"
    request = urllib.request.Request(
        url,
        headers={
            "Authorization": f"Bearer {token}",
            "Accept": ACCEPT_HEADER,
            "User-Agent": "zeroclaw-ghcr-publish-contract-guard/1",
        },
        method="GET",
    )

    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            body = response.read().decode("utf-8", errors="replace")
            digest = response.headers.get("Docker-Content-Digest", "").strip()
            content_type = response.headers.get("Content-Type", "").strip()
            return {
                "tag": tag,
                "url": url,
                "status_code": int(response.status),
                "digest": digest,
                "content_type": content_type,
                "error": None,
                "body_preview": body[:512],
            }
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", errors="replace") if exc.fp else ""
        return {
            "tag": tag,
            "url": url,
            "status_code": int(exc.code),
            "digest": "",
            "content_type": "",
            "error": f"HTTP {exc.code}",
            "body_preview": body[:512],
        }
    except Exception as exc:  # noqa: BLE001
        return {
            "tag": tag,
            "url": url,
            "status_code": 0,
            "digest": "",
            "content_type": "",
            "error": str(exc),
            "body_preview": "",
        }


def load_snapshot(path: Path) -> dict[str, dict[str, object]]:
    raw = json.loads(path.read_text(encoding="utf-8"))
    tags = raw.get("tags", {})
    out: dict[str, dict[str, object]] = {}
    if isinstance(tags, dict):
        for tag_name, value in tags.items():
            if not isinstance(tag_name, str) or not isinstance(value, dict):
                continue
            status_code = value.get("status_code", 0)
            out[tag_name] = {
                "tag": tag_name,
                "url": value.get("url"),
                "status_code": int(status_code) if isinstance(status_code, int) else 0,
                "digest": str(value.get("digest", "") or "").strip(),
                "content_type": str(value.get("content_type", "") or "").strip(),
                "error": value.get("error"),
                "body_preview": value.get("body_preview", ""),
            }
    return out


def build_markdown(report: dict[str, object]) -> str:
    lines: list[str] = []
    lines.append("# GHCR Publish Contract Report")
    lines.append("")
    lines.append(f"- Generated at: `{report['generated_at']}`")
    lines.append(f"- Repository: `{report['repository']}`")
    lines.append(f"- Release tag: `{report['release_tag']}`")
    lines.append(f"- Ready: `{report['ready']}`")
    lines.append("")

    lines.append("## Resolved Tags")
    resolved = report["resolved_tags"]
    lines.append(f"- Release: `{resolved['release']}`")
    lines.append(f"- SHA: `{resolved['sha']}`")
    lines.append(f"- Latest: `{resolved['latest']}`")
    lines.append("")

    lines.append("## Manifest Fetch")
    manifests: dict[str, dict[str, object]] = report["manifests"]
    lines.append("| Class | Tag | HTTP | Digest |")
    lines.append("| --- | --- | ---:| --- |")
    for class_name in ("release", "sha", "latest"):
        tag = resolved[class_name]
        entry = manifests.get(tag, {})
        lines.append(
            f"| `{class_name}` | `{tag}` | {entry.get('status_code', 0)} | `{entry.get('digest', '')}` |"
        )
    lines.append("")

    lines.append("## Rollback Candidates")
    for item in report["rollback_candidates"]:
        lines.append(f"- `{item}`")
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

    return "\n".join(lines).rstrip() + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate GHCR publish tag contract and rollback mapping.")
    parser.add_argument("--repository", required=True, help="Repository path for GHCR API, e.g. zeroclaw-labs/zeroclaw")
    parser.add_argument("--release-tag", required=True)
    parser.add_argument("--sha", required=True)
    parser.add_argument("--policy-file", required=True)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--manifest-snapshot-file", default="")
    parser.add_argument("--fail-on-violation", action="store_true")
    args = parser.parse_args()

    policy_file = Path(args.policy_file).resolve()
    output_json = Path(args.output_json)
    output_md = Path(args.output_md)
    snapshot_file = Path(args.manifest_snapshot_file).resolve() if args.manifest_snapshot_file else None

    if not policy_file.exists() or not policy_file.is_file():
        print(f"policy file does not exist: {policy_file}", file=sys.stderr)
        return 2

    violations: list[str] = []
    warnings: list[str] = []

    policy, policy_violations = load_policy(policy_file)
    violations.extend(policy_violations)

    resolved_tags, tag_violations = resolve_tags(policy, release_tag=args.release_tag, sha=args.sha)
    violations.extend(tag_violations)

    manifests: dict[str, dict[str, object]] = {}
    if snapshot_file is not None:
        if not snapshot_file.exists() or not snapshot_file.is_file():
            print(f"manifest snapshot file does not exist: {snapshot_file}", file=sys.stderr)
            return 2
        manifests = load_snapshot(snapshot_file)
    else:
        token, token_error = fetch_ghcr_token(args.repository)
        if token_error:
            violations.append(token_error)
            token = None
        if token is not None:
            for class_name in ("release", "sha", "latest"):
                tag = resolved_tags[class_name]
                manifests[tag] = fetch_manifest(args.repository, tag, token)

    for class_name in ("release", "sha", "latest"):
        tag = resolved_tags[class_name]
        entry = manifests.get(tag)
        if entry is None:
            violations.append(f"Missing manifest entry for `{tag}` ({class_name}).")
            continue
        status_code = int(entry.get("status_code", 0))
        if status_code != 200:
            violations.append(
                f"Manifest fetch for `{tag}` ({class_name}) returned HTTP {status_code}."
            )
            continue
        digest = str(entry.get("digest", "") or "").strip()
        if not digest:
            violations.append(f"Manifest `{tag}` ({class_name}) did not include Docker-Content-Digest header.")

    release_digest = str(manifests.get(resolved_tags["release"], {}).get("digest", "") or "").strip()
    sha_digest = str(manifests.get(resolved_tags["sha"], {}).get("digest", "") or "").strip()
    latest_digest = str(manifests.get(resolved_tags["latest"], {}).get("digest", "") or "").strip()

    if release_digest and sha_digest and release_digest != sha_digest:
        violations.append(
            "Digest parity check failed: release tag digest does not match immutable sha tag digest."
        )

    if bool(policy.get("require_latest_on_release")):
        if release_digest and latest_digest and release_digest != latest_digest:
            violations.append(
                "Digest parity check failed: latest tag digest does not match release tag digest."
            )

    rollback_candidates: list[str] = []
    for class_name in policy.get("rollback_priority", []):
        if not isinstance(class_name, str):
            continue
        tag_name = resolved_tags.get(class_name)
        if not isinstance(tag_name, str):
            continue
        entry = manifests.get(tag_name, {})
        if int(entry.get("status_code", 0)) == 200 and str(entry.get("digest", "")).strip():
            rollback_candidates.append(tag_name)
        else:
            warnings.append(
                f"Rollback candidate `{class_name}` resolved to `{tag_name}` but manifest evidence is incomplete."
            )

    report = {
        "schema_version": "zeroclaw.ghcr-publish-contract.v1",
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(),
        "repository": args.repository,
        "release_tag": args.release_tag,
        "sha": args.sha,
        "policy_file": str(policy_file),
        "policy_schema_version": policy.get("schema_version"),
        "policy": policy,
        "resolved_tags": resolved_tags,
        "manifests": manifests,
        "rollback_candidates": rollback_candidates,
        "ready": not violations,
        "warnings": warnings,
        "violations": violations,
    }

    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_md.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    output_md.write_text(build_markdown(report), encoding="utf-8")

    if args.fail_on_violation and violations:
        print("ghcr publish contract violations found:", file=sys.stderr)
        for item in violations:
            print(f"- {item}", file=sys.stderr)
        return 3

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
