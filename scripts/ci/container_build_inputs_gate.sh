#!/usr/bin/env bash
# Gate: a container definition that compiles the root package from source must
# copy the root build script into that build stage. Cargo runs no build script
# when the file is absent, and `env!` consumers of its `cargo:rustc-env` output
# then fail to compile — an error only image builds can see.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

python3 - "$repo_root" "$@" <<'PY'
import pathlib
import re
import sys

repo_root = pathlib.Path(sys.argv[1])
explicit = sys.argv[2:]

if not (repo_root / "build.rs").is_file():
    print("==> container build inputs gate: no root build.rs, nothing to guard")
    raise SystemExit(0)

if explicit:
    definitions = [pathlib.Path(p) for p in explicit]
else:
    skip_dirs = {".git", "target", "node_modules"}
    definitions = sorted(
        path
        for path in repo_root.rglob("*")
        if path.is_file()
        and re.fullmatch(r"(?i)(dockerfile|containerfile)[.\w-]*", path.name)
        and not skip_dirs.intersection(path.relative_to(repo_root).parts)
    )

CARGO_BUILD = re.compile(r"cargo\s+(?:build|install|zigbuild|check|nextest)\b")
ROOT_PACKAGE = re.compile(r"(?:-p[ =]zeroclaw\b|--bin\s+zeroclaw\b)")
COPIES_RS = re.compile(
    r"^\s*(?:COPY|ADD)\s+(?:\*\.rs|build\.rs)\b", re.IGNORECASE | re.MULTILINE
)
COPIES_CONTEXT = re.compile(
    r"^\s*(?:COPY|ADD)\s+\.\s+\./?\s*$", re.IGNORECASE | re.MULTILINE
)


def stages(text):
    """Yield (stage_number, body) split on FROM, with comment lines dropped."""
    body = []
    number = 0
    yielded = False
    for raw in text.splitlines():
        if raw.lstrip().startswith("#"):
            continue
        if re.match(r"(?i)\s*FROM\b", raw):
            if yielded:
                yield number, "\n".join(body)
            number += 1
            body = [raw]
            yielded = False
            continue
        if re.match(r"(?i)\s*(?:COPY|ADD|RUN)\b", raw):
            yielded = True
        body.append(raw)
    if body:
        yield number, "\n".join(body)


def commands(body):
    """Join backslash continuations so one instruction is one string."""
    chunk = ""
    for line in body.splitlines():
        stripped = line.rstrip()
        if stripped.endswith("\\"):
            chunk += stripped[:-1] + " "
            continue
        yield (chunk + stripped).strip()
        chunk = ""


failures = []
for path in definitions:
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as error:
        failures.append(f"{path}: unreadable ({error})")
        continue
    label = path.relative_to(repo_root) if path.is_relative_to(repo_root) else path
    for number, body in stages(text):
        builds_root = any(
            CARGO_BUILD.search(command) and ROOT_PACKAGE.search(command)
            for command in commands(body)
        )
        if not builds_root:
            continue
        if COPIES_RS.search(body) or COPIES_CONTEXT.search(body):
            continue
        failures.append(
            f"{label} stage {number}: builds the root package but never copies "
            "root build.rs (add `COPY *.rs .` or `COPY . .` to this stage)"
        )

if failures:
    print("==> container build inputs gate: FAILED", file=sys.stderr)
    for failure in failures:
        print(f"    {failure}", file=sys.stderr)
    raise SystemExit(1)

print(
    f"==> container build inputs gate: {len(definitions)} definitions keep the "
    "root build script in every source build"
)
PY
