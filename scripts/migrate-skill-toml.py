#!/usr/bin/env python3
"""Migrate SkillForge-emitted SKILL.toml files to the [forge] table layout.

Background
----------
Issue #6128 made the `[skill]` block of `SKILL.toml` strict
(`#[serde(deny_unknown_fields)]`). The SkillForge integrator
(`crates/zeroclaw-runtime/src/skillforge/integrate.rs`) used to emit its
provenance fields — `source`, `owner`, `language`, `license`, `stars`,
`updated_at` — and the nested sub-tables `[skill.requirements]` /
`[skill.metadata]` directly inside `[skill]`. After PR #6209 (which closes
both #6128 and #6210), those fields live under a top-level sibling
`[forge]` table instead. This script migrates existing on-disk
SKILL.toml files to the new layout so that operators with
`auto_integrate = true` are not surprised by silent skill load failures.

What it does
------------
For each `SKILL.toml` it finds:

  1. Skip files that already contain a top-level `[forge]` table
     (idempotence: re-running on already-migrated files is a no-op).
  2. From the `[skill]` block, lift these top-level keys into a new
     `[forge]` table (preserving order and inline values verbatim):

       source, owner, language, license, stars, updated_at

  3. Rename `[skill.requirements]` → `[forge.requirements]` and
     `[skill.metadata]` → `[forge.metadata]`, preserving their
     contents.

  4. Leave `[skill]` keys not in the list above untouched
     (`name`, `version`, `description`, `author`, `tags`, `prompts`,
     and any other field declared in `SkillMeta`).

What it does NOT do
-------------------
- Reformat unrelated whitespace, comments, or table ordering beyond
  what the migration requires.
- Migrate hand-authored SKILL.toml files that don't carry SkillForge
  provenance (those have nothing to move and remain untouched).
- Touch `SKILL.md` (it doesn't carry the affected schema).

Usage
-----
Dry-run (default) — report what would change but do not write:

    python3 scripts/migrate-skill-toml.py /path/to/skills/

Apply changes in place:

    python3 scripts/migrate-skill-toml.py /path/to/skills/ --apply

Migrate a single file:

    python3 scripts/migrate-skill-toml.py /path/to/skill/SKILL.toml --apply

Exit codes
----------
  0 — Success (or dry-run completed). All files either migrated cleanly
      or were already in the new layout.
  1 — At least one file could not be parsed / migrated. Per-file
      errors are printed to stderr.
  2 — Bad invocation (missing path, etc.).

Dependencies
------------
Standard library only (Python 3.8+). No `tomli` / `tomli_w` required.
The integrator emits a deterministic line-based format, so a textual
migration is sufficient and avoids depending on a TOML round-trip
library that would re-format every file.
"""
from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import List, Optional, Tuple

# Top-level keys that the SkillForge integrator emits inside `[skill]`
# and that must be moved to `[forge]`. This list mirrors
# `Integrator::generate_toml` exactly — keep it in sync if that
# function's emit format changes.
PROVENANCE_KEYS: Tuple[str, ...] = (
    "source",
    "owner",
    "language",
    "license",
    "stars",
    "updated_at",
)

# Sub-tables emitted under `[skill]` that must be renamed to `[forge.*]`.
PROVENANCE_SUBTABLES: Tuple[str, ...] = (
    "requirements",
    "metadata",
)

# Match a TOML table header line, capturing the dotted name. Whitespace
# tolerant, doesn't try to parse inline tables (the integrator never
# emits those for the affected fields).
RE_TABLE = re.compile(r"^\s*\[\s*([A-Za-z0-9_.\-]+)\s*\]\s*$")

# Match `key = ...` at the beginning of a line. Captures the key.
# We deliberately don't try to parse the value — we lift the entire
# line verbatim into `[forge]`.
RE_KEY = re.compile(r"^\s*([A-Za-z0-9_-]+)\s*=")


@dataclass
class MigrationPlan:
    """The before/after of a single SKILL.toml migration."""

    path: Path
    before: str
    after: str
    moved_keys: List[str] = field(default_factory=list)
    moved_subtables: List[str] = field(default_factory=list)
    already_migrated: bool = False
    note: Optional[str] = None

    @property
    def changed(self) -> bool:
        return self.before != self.after


def find_skill_tomls(root: Path) -> List[Path]:
    """Return SKILL.toml files under `root` (or `root` itself if it is one)."""
    if root.is_file():
        if root.name == "SKILL.toml":
            return [root]
        return []
    return sorted(p for p in root.rglob("SKILL.toml") if p.is_file())


def plan_migration(path: Path) -> MigrationPlan:
    """Build a migration plan for a single file. Pure function — no I/O writes."""
    content = path.read_text(encoding="utf-8")
    lines = content.splitlines(keepends=True)

    # Idempotence guard: if a top-level `[forge]` table already exists,
    # the file is either already migrated or hand-authored with [forge].
    # In either case, skip.
    for line in lines:
        m = RE_TABLE.match(line)
        if m and m.group(1) == "forge":
            return MigrationPlan(
                path=path,
                before=content,
                after=content,
                already_migrated=True,
                note="[forge] table already present — skipping (idempotent re-run)",
            )

    # Walk the file once and bucket lines by table. We track:
    #   - top-level header section (before any [table])
    #   - [skill] block (and its nested sub-tables)
    #   - everything else (preserved as-is)
    sections: List[Tuple[Optional[str], List[str]]] = []
    # current table name (None = pre-table preamble), current line buffer
    current_table: Optional[str] = None
    current_buf: List[str] = []

    def flush() -> None:
        sections.append((current_table, current_buf.copy()))
        current_buf.clear()

    for line in lines:
        m = RE_TABLE.match(line)
        if m:
            flush()
            current_table = m.group(1)
            current_buf = [line]
        else:
            current_buf.append(line)
    flush()

    # Build the new structure.
    new_sections: List[Tuple[Optional[str], List[str]]] = []
    forge_keys_lines: List[str] = []
    forge_subtables: List[Tuple[str, List[str]]] = []
    moved_keys: List[str] = []
    moved_subtables: List[str] = []

    for table, buf in sections:
        if table is None:
            # Preamble (comments, blank lines before first table).
            new_sections.append((table, buf))
            continue

        if table == "skill":
            # Split the [skill] block into "kept" lines and "moved" lines.
            kept: List[str] = []
            for i, line in enumerate(buf):
                if i == 0:
                    # The `[skill]` header itself.
                    kept.append(line)
                    continue
                key_m = RE_KEY.match(line)
                if key_m and key_m.group(1) in PROVENANCE_KEYS:
                    forge_keys_lines.append(line)
                    moved_keys.append(key_m.group(1))
                else:
                    kept.append(line)
            new_sections.append((table, kept))
            continue

        # Nested `[skill.requirements]`, `[skill.metadata]` → rename.
        if table.startswith("skill."):
            suffix = table[len("skill.") :]
            if suffix in PROVENANCE_SUBTABLES:
                # Rebuild the header line and keep the body verbatim.
                renamed_header = f"[forge.{suffix}]\n"
                # Preserve any trailing whitespace style by checking if the
                # original header line had a trailing newline.
                if not buf[0].endswith("\n"):
                    renamed_header = renamed_header.rstrip("\n")
                renamed_buf = [renamed_header] + buf[1:]
                forge_subtables.append((suffix, renamed_buf))
                moved_subtables.append(suffix)
                continue

        # Anything else (e.g. `[[tools]]`, `[skill.something_else]`,
        # `[forge.*]` if a hand-author added one already, etc.) passes
        # through untouched.
        new_sections.append((table, buf))

    # If nothing moved, we have no migration to perform — but flag it
    # so dry-run output makes sense.
    if not moved_keys and not moved_subtables:
        return MigrationPlan(
            path=path,
            before=content,
            after=content,
            note="no SkillForge provenance fields found — file unchanged",
        )

    # Assemble the output. The `[forge]` table is inserted after `[skill]`
    # (and after any pass-through tables that originally appeared between
    # `[skill]` and the first sub-table we lifted), positioned before any
    # moved sub-tables. To keep the output stable and easy to reason about
    # we put `[forge]` immediately after `[skill]`, then any preserved
    # tables, then `[forge.requirements]` / `[forge.metadata]` at the end.
    out_lines: List[str] = []
    inserted_forge = False
    for table, buf in new_sections:
        out_lines.extend(buf)
        if table == "skill" and not inserted_forge:
            # Build the [forge] block.
            # Ensure the previous block ends with a newline.
            if out_lines and not out_lines[-1].endswith("\n"):
                out_lines.append("\n")
            out_lines.append("\n[forge]\n")
            out_lines.extend(forge_keys_lines)
            inserted_forge = True

    # If for some reason the file has no [skill] table (defensive), append
    # [forge] at the end.
    if not inserted_forge and (forge_keys_lines or forge_subtables):
        if out_lines and not out_lines[-1].endswith("\n"):
            out_lines.append("\n")
        out_lines.append("\n[forge]\n")
        out_lines.extend(forge_keys_lines)

    # Append the renamed sub-tables at the end, in their original order
    # within the source (requirements, then metadata, per integrator emit).
    for _suffix, sub_buf in forge_subtables:
        if out_lines and not out_lines[-1].endswith("\n"):
            out_lines.append("\n")
        # Keep the blank-line spacing the integrator originally emitted.
        if not (sub_buf and sub_buf[0].startswith("[")):
            # Defensive: shouldn't happen
            pass
        # Add a separating blank line before each sub-table for readability.
        out_lines.append("\n")
        out_lines.extend(sub_buf)

    after = "".join(out_lines)
    return MigrationPlan(
        path=path,
        before=content,
        after=after,
        moved_keys=moved_keys,
        moved_subtables=moved_subtables,
    )


def format_summary(plan: MigrationPlan, apply: bool) -> str:
    rel = str(plan.path)
    if plan.already_migrated:
        return f"  skip  {rel}  [{plan.note}]"
    if not plan.changed:
        return f"  skip  {rel}  [{plan.note}]"
    moved = []
    if plan.moved_keys:
        moved.append(f"keys: {', '.join(plan.moved_keys)}")
    if plan.moved_subtables:
        moved.append(
            "sub-tables: "
            + ", ".join(f"[skill.{s}] -> [forge.{s}]" for s in plan.moved_subtables)
        )
    verb = "MIGRATE" if apply else "would migrate"
    return f"  {verb}  {rel}  ({'; '.join(moved)})"


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        description="Migrate SkillForge-emitted SKILL.toml files to the [forge] table layout.",
    )
    parser.add_argument(
        "path",
        type=Path,
        help="Path to a skills directory (recursed) or a single SKILL.toml file.",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="Write changes to disk. Without this flag, runs in dry-run mode.",
    )
    parser.add_argument(
        "--quiet",
        action="store_true",
        help="Suppress per-file output; only print the final summary.",
    )
    args = parser.parse_args(argv)

    if not args.path.exists():
        print(f"error: path does not exist: {args.path}", file=sys.stderr)
        return 2

    files = find_skill_tomls(args.path)
    if not files:
        print(f"no SKILL.toml files found under {args.path}", file=sys.stderr)
        return 0

    if not args.quiet:
        mode = "APPLY" if args.apply else "DRY-RUN (use --apply to write)"
        print(f"migrate-skill-toml.py — mode: {mode}")
        print(f"scanning {len(files)} file(s) under {args.path}\n")

    n_changed = 0
    n_skipped = 0
    n_errors = 0

    for path in files:
        try:
            plan = plan_migration(path)
        except OSError as e:
            print(f"  ERROR  {path}: {e}", file=sys.stderr)
            n_errors += 1
            continue

        if plan.changed:
            n_changed += 1
            if args.apply:
                try:
                    path.write_text(plan.after, encoding="utf-8")
                except OSError as e:
                    print(f"  ERROR  {path}: failed to write: {e}", file=sys.stderr)
                    n_errors += 1
                    continue
        else:
            n_skipped += 1

        if not args.quiet:
            print(format_summary(plan, args.apply))

    if not args.quiet:
        print()
    verb = "migrated" if args.apply else "would migrate"
    print(
        f"summary: {n_changed} {verb}, {n_skipped} skipped, {n_errors} error(s)"
    )

    if n_errors:
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
