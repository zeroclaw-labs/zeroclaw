#!/usr/bin/env python3
"""Checks for the reference-vector generator's provenance gate.

The generator stamps a reference commit into every fixture it writes. That stamp
is what makes the vectors independent evidence, so a checkout that is not the
pinned revision, or one whose imported source has been altered, must not be able
to produce a file carrying it.

These run against throwaway repositories and never touch a real reference
checkout, so they need neither the reference nor its Python dependencies. The
gate takes its expected commit as an argument for exactly that reason: a
temporary repository cannot be made to have the pinned hash, so the
right-revision-but-modified case would otherwise be untestable.

    python3 scripts/dev/generate-vi-reference-vectors.test.py
"""

from __future__ import annotations

import importlib.util
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
GENERATOR = HERE / "generate-vi-reference-vectors.py"


def load_generator():
    """Import the generator by path, since its filename is not an identifier."""
    spec = importlib.util.spec_from_file_location("vi_reference_vectors", GENERATOR)
    if spec is None or spec.loader is None:
        raise SystemExit(f"cannot load {GENERATOR}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def git(root: Path, *args: str) -> str:
    completed = subprocess.run(
        ["git", "-C", str(root), *args],
        capture_output=True,
        text=True,
        check=True,
    )
    return completed.stdout.strip()


def make_repo(root: Path) -> str:
    """A minimal repository shaped like the reference: one file under src."""
    (root / "src").mkdir(parents=True)
    (root / "src" / "module.py").write_text("VALUE = 1\n")
    git(root, "init", "--quiet", ".")
    git(root, "config", "user.email", "checks@example.invalid")
    git(root, "config", "user.name", "provenance checks")
    git(root, "add", "-A")
    git(root, "commit", "--quiet", "-m", "initial")
    return git(root, "rev-parse", "HEAD")


def expect_refusal(module, root: Path, expected: str, label: str) -> None:
    try:
        module.verify_reference_source(root, expected)
    except SystemExit as refusal:
        print(f"  ok   {label}: refused ({refusal})")
        return
    raise SystemExit(f"  FAIL {label}: generation was allowed to proceed")


def expect_accepted(module, root: Path, expected: str, label: str) -> None:
    try:
        module.verify_reference_source(root, expected)
    except SystemExit as refusal:
        raise SystemExit(f"  FAIL {label}: refused a valid checkout ({refusal})") from None
    print(f"  ok   {label}: accepted")


def main() -> int:
    module = load_generator()
    print("provenance gate checks")

    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp) / "reference"
        head = make_repo(root)

        # The positive control comes first: without it a gate that refused
        # everything would pass every case below.
        expect_accepted(module, root, head, "pinned revision, clean tree")

        expect_refusal(
            module,
            root,
            "0000000000000000000000000000000000000000",
            "wrong revision",
        )

        # The reason the expected commit is a parameter: this case needs a
        # checkout that *is* at the expected revision and has been altered.
        (root / "src" / "module.py").write_text("VALUE = 2\n")
        expect_refusal(module, root, head, "pinned revision, modified source")
        git(root, "checkout", "--", "src")

        # An untracked module would shadow an import without modifying a
        # tracked file, so the scoped status check has to report it.
        (root / "src" / "shadow.py").write_text("VALUE = 3\n")
        expect_refusal(module, root, head, "pinned revision, untracked source")
        (root / "src" / "shadow.py").unlink()

        # Changes outside the imported directory are not this gate's business.
        (root / "README").write_text("unrelated\n")
        expect_accepted(module, root, head, "unrelated change outside src")

    with tempfile.TemporaryDirectory() as temp:
        plain = Path(temp) / "not-a-repo"
        (plain / "src").mkdir(parents=True)
        expect_refusal(module, plain, "0" * 40, "directory that is not a git checkout")

    print("all provenance gate checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
