#!/usr/bin/env python3
"""Promote an existing release by updating root metadata, the llms pair and the
version-less stable/ alias.

Master's committed pointer is policy; GitHub Latest and the deployed source
receipt prove eligibility. No builds, pruning, tag writes, or publication here.
"""

import argparse
import base64
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tomllib


# Root pair agents fetch by fixed URL; promoted as one unit with the metadata.
LLMS_FILES = ("llms.txt", "llms-full.txt")
# Version-less copy of the stable release; STABLE_ALIAS in
# xtask/src/cmd/mdbook/versions.rs. Canonical and sitemap URLs name it.
STABLE_ALIAS = "stable"


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


def root_llms(pages, tag):
    """Desired root llms files for the promoted release: text per name, or None
    to withdraw that file.

    Mirrors xtask's `sync-root-llms` copy-or-remove unit: the root pair mirrors
    the release `/` redirects to, a release carrying only one of the two files
    publishes neither, and master's own pair is never promoted in its place.
    """
    source = pages / tag / "en"
    if not all((source / name).is_file() for name in LLMS_FILES):
        return dict.fromkeys(LLMS_FILES)
    return {name: read_file(source / name) for name in LLMS_FILES}


# File names gen-seo leaves out of the sitemap at any depth (SKIP_FILES in
# xtask/src/cmd/mdbook/seo.rs).
SITEMAP_SKIP = {"print.html", "404.html", "toc.html"}


def sitemap_escape(text):
    # The same escaping as gen-seo's `escape`.
    return text.replace("&", "&amp;").replace('"', "&quot;").replace("<", "&lt;").replace(">", "&gt;")


def stable_sitemap(pages, release):
    """sitemap.xml for `release` as the stable alias, as xtask's gen-seo writes
    it: the site root, then every page of every locale under stable/, locales
    and pages in gen-seo's order. Promotion replaces stable/ with the release,
    so a sitemap still listing the previous release's pages would send
    crawlers to pages the release removed, and miss the ones it added.
    """
    host = read_file(pages / "CNAME").strip()
    require(bool(host), "CNAME is empty")
    site = f"https://{host}"
    urls = [f"{site}/"]
    locale_dirs = sorted(entry.name for entry in release.iterdir()
                         if entry.is_dir() and (entry / "index.html").is_file())
    for locale in locale_dirs:
        root = release / locale
        found = [path for path in root.rglob("*.html") if path.is_file() and path.name not in SITEMAP_SKIP]
        # gen-seo sorts paths by component, so `a/b.html` comes before `a-b.html`.
        for page in sorted(found, key=lambda path: path.relative_to(root).parts):
            urls.append(f"{site}/{STABLE_ALIAS}/{locale}/{page.relative_to(root).as_posix()}")
    return ('<?xml version="1.0" encoding="UTF-8"?>\n'
            '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n'
            + "".join(f"  <url><loc>{sitemap_escape(url)}</loc></url>\n" for url in urls)
            + "</urlset>\n")


def contains_symlink(root):
    for directory, dirs, files in os.walk(root):
        if any(os.path.islink(os.path.join(directory, name)) for name in dirs + files):
            return True
    return False


def replace_alias(target, alias):
    """Make the stable alias a copy of the release dir, as xtask's
    gen-stable-alias does on every build deploy. The copied pages keep the SEO
    tags they were deployed with; pages new in this release get their
    stable-alias canonical on the next build deploy, which reruns gen-seo.
    """
    staged = alias.with_name(f"{alias.name}.tmp")
    shutil.rmtree(staged, ignore_errors=True)
    shutil.copytree(target, staged, symlinks=True)
    if alias.exists():
        shutil.rmtree(alias)
    staged.replace(alias)


def refreshed_root_404(pages, locales):
    """The root 404.html retargeted at the stable alias and the promoted
    release's locales, or None when the site has no 404 page yet (the next
    normal deploy writes one). Only the two lines xtask's gen-root-404 writes
    for these values change: `var target = "...";` and `var locales = [...];`
    (root_404_html in xtask/src/cmd/mdbook/versions.rs)."""
    path = pages / "404.html"
    if not path.exists():
        return None
    require(not path.is_symlink(), "The root 404 page must not be a symlink")
    text = read_file(path)
    target_line = f"var target = {json.dumps(STABLE_ALIAS)};"
    # Sorted and compact, as gen-root-404 lists them.
    locales_line = "var locales = " + json.dumps(sorted(locales), separators=(",", ":")) + ";"
    text, targets = re.subn(r'^var target = "[^"\n]*";$', lambda _: target_line, text, flags=re.M)
    text, lists = re.subn(r"^var locales = \[[^\n]*\];$", lambda _: locales_line, text, flags=re.M)
    require(targets == 1 and lists == 1,
            "The root 404 page has an unexpected shape; run a normal docs deploy first")
    return text


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
    alias = pages / STABLE_ALIAS
    require(not alias.is_symlink(), "The stable alias must not be a symlink")
    require(not contains_symlink(target), "Release directory must not contain symlinks")
    llms = root_llms(pages, tag)
    sitemap = stable_sitemap(pages, target)
    for entry in entries:
        if entry["tag"] == current:
            entry["label"] = current
        if entry["tag"] == tag:
            entry["label"] = "Stable (latest release)"
    metadata["stable"] = tag
    destination = f"./{STABLE_ALIAS}/en/"
    # Same root redirect shape as xtask's gen-root-index; keep the existing
    # version inventory/order instead of copying its discovery/retention logic.
    index = ('<!doctype html>\n<meta charset="utf-8">\n'
             f'<meta http-equiv="refresh" content="0; url={destination}">\n'
             f'<link rel="canonical" href="{destination}">\n<title>ZeroClaw Docs</title>\n')
    # The 404 page sends links to pruned releases to the stable alias, for the
    # locales it lists: refresh them for the release being promoted.
    root_404 = refreshed_root_404(pages, locales)
    if not check_only:
        (pages / "stable-version.txt").write_text(tag + "\n", encoding="utf-8")
        (pages / "sitemap.xml").write_text(sitemap, encoding="utf-8")
        (pages / "versions.json").write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        (pages / "index.html").write_text(index, encoding="utf-8")
        if root_404 is not None:
            (pages / "404.html").write_text(root_404, encoding="utf-8")
        for name, text in llms.items():
            published = pages / name
            if text is None:
                published.unlink(missing_ok=True)
                continue
            staged = pages / f"{name}.tmp"
            staged.write_text(text, encoding="utf-8")
            staged.replace(published)
        replace_alias(target, alias)
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
