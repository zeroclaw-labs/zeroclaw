#!/usr/bin/env python3
"""Promote an existing release by changing exactly three gh-pages root files.

Master's committed pointer is policy; GitHub Latest and the deployed source
receipt prove eligibility. No builds, pruning, tag writes, or publication here.
"""

import argparse
import base64
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib


def require(condition, message):
    if not condition:
        raise ValueError(message)


def final_version(tag):
    require(re.fullmatch(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", tag),
            f"Expected a final vX.Y.Z tag, got {tag!r}")
    return tuple(int(part) for part in tag[1:].split("."))


def gh_json(repo, endpoint):
    result = subprocess.run(["gh", "api", "--method", "GET", f"repos/{repo}/{endpoint}"],
                            check=True, capture_output=True, text=True, timeout=60)
    return json.loads(result.stdout)


def github_file(repo, name, sha):
    data = gh_json(repo, f"contents/{name}?ref={sha}")
    require(data["encoding"] == "base64", "GitHub content was not base64 encoded")
    return base64.b64decode(data["content"].replace("\n", ""), validate=True).decode("utf-8")


def read_file(path):
    require(path.is_file() and not path.is_symlink(), f"Expected a regular file: {path}")
    return path.read_text(encoding="utf-8")


def promote(*, pages, repo, master_sha, tag, workflow_ref, minimum, check_only=False):
    version = final_version(tag)
    require(workflow_ref == "refs/heads/master", "Promotion must run on master")
    require(version >= final_version(minimum), "Release is below the docs minimum version")
    master = gh_json(repo, "branches/master")
    require(master["protected"] is True, "Promotion requires protected master")
    require(master["commit"]["sha"] == master_sha, "master advanced; dispatch a fresh promotion")
    pointer = github_file(repo, "docs/book/stable-version.txt", master_sha).strip()
    require(pointer == tag, "Requested tag does not match master's committed pointer")
    release = gh_json(repo, "releases/latest")
    require(release["tag_name"] == tag, "Requested tag is not GitHub Latest")
    require(release.get("draft") is False and release.get("prerelease") is False
            and bool(release.get("published_at")), "GitHub Latest must be a public final release")

    # Resolve annotated as well as lightweight tags; target_commitish can be a
    # mutable branch name and cannot establish release source identity.
    obj = gh_json(repo, f"git/ref/tags/{tag}")["object"]
    for _ in range(8):
        if obj["type"] != "tag":
            break
        obj = gh_json(repo, f"git/tags/{obj['sha']}")["object"]
    require(obj["type"] == "commit" and re.fullmatch(r"[0-9a-f]{40}", obj["sha"]),
            "Could not resolve the release tag commit")
    tag_sha = obj["sha"]
    target = pages / tag
    require(not target.is_symlink(), "Release directory must not be a symlink")
    require(read_file(target / ".docs-source-commit").strip() == tag_sha,
            "Release tag does not match deployed source; deploy that release first")

    # The release's own locale registry defines its payload, even if master has
    # added a language since that release. English is the root redirect target.
    registry = tomllib.loads(github_file(repo, "locales.toml", tag_sha))
    locales = [entry["code"] for entry in registry["locale"]]
    require("en" in locales and len(set(locales)) == len(locales)
            and all(re.fullmatch(r"[A-Za-z0-9]+(?:-[A-Za-z0-9]+)*", code) for code in locales),
            "Invalid release locale registry")
    for locale in locales:
        require(not (target / locale).is_symlink(), "Locale directory must not be a symlink")
        require(bool(read_file(target / locale / "index.html").strip()),
                f"Release locale {locale} has an empty landing page")

    current = read_file(pages / "stable-version.txt").strip()
    require(version >= final_version(current),
            "Refusing a stable downgrade; use a separately reviewed manual rollback")
    metadata = json.loads(read_file(pages / "versions.json"))
    require(metadata["stable"] == current, "Version metadata disagrees with the live pointer")
    entries = metadata["versions"]
    tags = [entry["tag"] for entry in entries]
    require(tag in tags and current in tags and len(set(tags)) == len(tags),
            "Missing or duplicate version entries; run a normal docs deploy first")
    read_file(pages / "index.html")  # Check every output before writing any.
    for entry in entries:
        if entry["tag"] == current:
            entry["label"] = current
        if entry["tag"] == tag:
            entry["label"] = "Stable (latest release)"
    metadata["stable"] = tag
    destination = f"./{tag}/en/"
    # Same root redirect shape as xtask's gen-root-index; keep the existing
    # version inventory/order instead of copying its discovery/retention logic.
    index = ('<!doctype html>\n<meta charset="utf-8">\n'
             f'<meta http-equiv="refresh" content="0; url={destination}">\n'
             f'<link rel="canonical" href="{destination}">\n<title>ZeroClaw Docs</title>\n')
    if not check_only:
        (pages / "stable-version.txt").write_text(tag + "\n", encoding="utf-8")
        (pages / "versions.json").write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        (pages / "index.html").write_text(index, encoding="utf-8")
    print(f"{'Validated' if check_only else 'Promoted'} {tag} ({tag_sha}) from master {master_sha}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pages", type=Path, required=True)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--master-sha", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--minimum", default="v0.7.5")
    parser.add_argument("--check-only", action="store_true")
    args = vars(parser.parse_args())
    try:
        promote(**args, workflow_ref=os.environ.get("GITHUB_REF", ""))
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f"::error::Stable promotion failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
