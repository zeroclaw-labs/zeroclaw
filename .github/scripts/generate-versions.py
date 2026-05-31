#!/usr/bin/env python3
"""Generate versions.json from deployed version directories.

This script scans the current directory for version-like directories
(master, stable, v0.7.5, v0.8.0-beta-1, etc.) and generates a versions.json
file that lists all available documentation versions.

The script also determines which version should be marked as 'stable'.

Usage:
    python3 generate-versions.py > versions.json
"""

import json
import os
import re
import sys


_SEMVER_RE = re.compile(r'^v(\d+)\.(\d+)\.(\d+)(?:-(.*))?$')


def semver_key(tag: str):
    """Sort key producing reverse-chronological semver order (newest first).

    master and stable are always pinned to the front.  Among tagged versions,
    higher versions sort before lower ones (v0.10.0 before v0.9.x).  For the
    same triplet, the GA release sorts before any pre-release
    (v0.8.0 before v0.8.0-beta-1).

    Negating major/minor/patch makes the ascending sort act as newest-first
    without requiring a separate reverse pass that would disrupt the
    master/stable pin.
    """
    if tag == 'master':
        return (0, 0, 0, 0, 0, '')
    if tag == 'stable':
        return (0, 0, 0, 0, 1, '')
    m = _SEMVER_RE.match(tag)
    if m:
        major, minor, patch, pre = int(m.group(1)), int(m.group(2)), int(m.group(3)), m.group(4)
        # GA release (pre is None) sorts before pre-releases of the same triplet.
        pre_rank = 0 if pre is None else 1
        return (1, -major, -minor, -patch, pre_rank, pre or '')
    # Unrecognised tag: sort lexicographically at the end.
    return (2, 0, 0, 0, 0, tag)


def main():
    # Find all version-like directories (v0.7.5, master, stable, v0.8.0-beta-1, etc.)
    version_pattern = re.compile(
        r'^(master|stable|v\d+\.\d+\.\d+(-[a-z0-9.-]+)?)$', re.IGNORECASE
    )
    dirs = []
    for d in os.listdir('.'):
        if os.path.isdir(d) and d != '.git' and version_pattern.match(d):
            dirs.append(d)

    # master first, stable second, then tagged versions newest -> oldest.
    # Negated numeric components in semver_key() make ascending sort == newest-first.
    dirs.sort(key=semver_key)

    versions = []
    stable_tag = None

    for tag in dirs:
        # Determine label
        if tag == 'master':
            label = 'Development (master)'
        elif tag == 'stable':
            label = 'Stable'
            stable_tag = tag
        else:
            label = tag

        versions.append({'tag': tag, 'label': label, 'url': f'/{tag}/'})

    # If 'stable' exists, use it; otherwise find the latest stable version (no pre-release)
    if not stable_tag:
        for tag in reversed(dirs):
            if re.match(r'^v\d+\.\d+\.\d+$', tag):  # No pre-release suffix
                stable_tag = tag
                break

    output = {'stable': stable_tag, 'versions': versions}

    print(json.dumps(output, indent=2))


if __name__ == '__main__':
    main()
