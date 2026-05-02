#!/usr/bin/env python3
"""
build_domain_delta.py — cumulative delta SQLite builder

Compares an operator-side **baseline** domain.db (the file that was
published as `baseline.url`) against the **current** domain.db (a
freshly rebuilt full corpus) and emits a small SQLite file that the
client's `vault domain update` apply-delta path consumes.

Output schema (matches `src/vault/domain_delta.rs::ensure_delta_schema`):
  - the standard vault tables (vault_documents/links/aliases/...)
    holding only changed/new rows, copied from current
  - vault_deletes(uuid, deleted_at) — rows present in baseline but
    not in current
  - meta (key, value) — schema_kind='domain-delta', delta_version,
    applies_to_baseline, baseline_sha256

Cumulative semantics: the delta carries every change since the
published baseline, not just changes since the prior delta. A
client that fell behind for N weeks downloads exactly one delta —
this one — and is fully caught up. See
docs/domain-db-incremental-design.md §2.1.

Usage:
  python scripts/build_domain_delta.py \
      --baseline out/korean-legal-2026.01.15.db \
      --current  out/staging-2026.04.22.db \
      --out      out/korean-legal-delta-2026.04.22.sqlite \
      --version  2026.04.22 \
      --applies-to-baseline 2026.01.15 \
      [--baseline-sha256 <64hex>]   # optional override; default = sha256 of baseline file
"""
import argparse
import hashlib
import os
import sqlite3
import sys
from pathlib import Path

# Tables copied verbatim from `current` for every changed/new doc id.
AUX_TABLES = [
    ("vault_links", "source_doc_id"),
    ("vault_aliases", "doc_id"),
    ("vault_frontmatter", "doc_id"),
    ("vault_tags", "doc_id"),
    ("vault_embeddings", "doc_id"),
]


def sha256_of_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(64 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def init_delta_schema(conn: sqlite3.Connection) -> None:
    """Mirror `src/vault/schema.rs::init_schema` for the columns this
    builder writes, plus the delta-only `vault_deletes` + `meta`.

    We keep this in sync by *inserting from current via ATTACH* rather
    than recreating each table from scratch — see `_clone_table_schema`.
    The only tables defined explicitly here are the delta-only ones."""
    conn.executescript(
        """
        CREATE TABLE IF NOT EXISTS vault_deletes (
            uuid       TEXT NOT NULL PRIMARY KEY,
            deleted_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        """
    )


def _clone_table_schema(conn: sqlite3.Connection, table: str) -> None:
    """Copy a table's CREATE TABLE definition from the attached
    `current` DB into the live (delta) connection. Idempotent."""
    row = conn.execute(
        "SELECT sql FROM current.sqlite_master "
        "WHERE type = 'table' AND name = ?",
        (table,),
    ).fetchone()
    if not row or not row[0]:
        # Table missing in current → nothing to clone (e.g. embeddings
        # never populated). Caller skips inserts gracefully.
        return
    create_sql = row[0]
    # Already exists?
    exists = conn.execute(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?",
        (table,),
    ).fetchone()
    if exists:
        return
    conn.executescript(create_sql + ";")


def build_delta(
    baseline_path: Path,
    current_path: Path,
    out_path: Path,
    delta_version: str,
    applies_to_baseline: str,
    baseline_sha256: str | None,
) -> dict:
    if out_path.exists():
        out_path.unlink()
    out_path.parent.mkdir(parents=True, exist_ok=True)

    delta_conn = sqlite3.connect(str(out_path))
    delta_conn.execute("PRAGMA foreign_keys = OFF")

    # Stage the schemas we need before any INSERT…SELECT runs.
    delta_conn.execute(f"ATTACH DATABASE ? AS baseline", (str(baseline_path),))
    delta_conn.execute(f"ATTACH DATABASE ? AS current", (str(current_path),))
    init_delta_schema(delta_conn)
    for table in ("vault_documents", *[t for (t, _) in AUX_TABLES]):
        _clone_table_schema(delta_conn, table)

    # ── Upserts: doc rows changed or new since baseline ─────────────
    cur_rows = delta_conn.execute(
        """
        SELECT cur.id, cur.uuid, cur.checksum
          FROM current.vault_documents AS cur
     LEFT JOIN baseline.vault_documents AS base
            ON base.uuid = cur.uuid
         WHERE base.uuid IS NULL                      -- brand-new
            OR base.checksum IS NULL
            OR base.checksum != cur.checksum          -- content changed
        """
    ).fetchall()
    changed_ids = [row[0] for row in cur_rows]
    upserted = len(changed_ids)

    if upserted > 0:
        placeholders = ",".join("?" * len(changed_ids))
        delta_conn.execute(
            f"""
            INSERT INTO vault_documents
              SELECT * FROM current.vault_documents
               WHERE id IN ({placeholders})
            """,
            changed_ids,
        )
        for table, fk_col in AUX_TABLES:
            # Skip cleanly if the table doesn't exist in current.
            exists = delta_conn.execute(
                "SELECT 1 FROM current.sqlite_master "
                "WHERE type='table' AND name=?",
                (table,),
            ).fetchone()
            if not exists:
                continue
            delta_conn.execute(
                f"""
                INSERT INTO {table}
                  SELECT * FROM current.{table}
                   WHERE {fk_col} IN ({placeholders})
                """,
                changed_ids,
            )

    # ── Deletes: uuids in baseline but not in current ───────────────
    doomed = delta_conn.execute(
        """
        SELECT base.uuid
          FROM baseline.vault_documents AS base
     LEFT JOIN current.vault_documents AS cur
            ON cur.uuid = base.uuid
         WHERE cur.uuid IS NULL
        """
    ).fetchall()
    deleted = len(doomed)

    if deleted > 0:
        # Use unixepoch() on the delta connection — applies_at is
        # informational, the integrity check is on uuid identity.
        now_unix = int(__import__("time").time())
        delta_conn.executemany(
            "INSERT OR IGNORE INTO vault_deletes(uuid, deleted_at) VALUES (?, ?)",
            [(uuid, now_unix) for (uuid,) in doomed],
        )

    # ── meta rows (must match `domain_delta.rs::stamp_delta_meta`) ──
    if baseline_sha256 is None:
        baseline_sha256 = sha256_of_file(baseline_path)
    delta_conn.executemany(
        "INSERT OR REPLACE INTO meta(key, value) VALUES (?, ?)",
        [
            ("schema_kind", "domain-delta"),
            ("delta_version", delta_version),
            ("applies_to_baseline", applies_to_baseline),
            ("baseline_sha256", baseline_sha256),
        ],
    )

    delta_conn.commit()
    delta_conn.execute("DETACH DATABASE baseline")
    delta_conn.execute("DETACH DATABASE current")
    delta_conn.execute("VACUUM")
    delta_conn.close()

    size = out_path.stat().st_size
    return {
        "upserted_documents": upserted,
        "deleted_documents": deleted,
        "size_bytes": size,
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Build a cumulative domain.db delta from baseline → current."
    )
    parser.add_argument("--baseline", type=Path, required=True,
                        help="Published baseline domain.db (the one clients already have).")
    parser.add_argument("--current", type=Path, required=True,
                        help="Fresh full domain.db built from latest corpus.")
    parser.add_argument("--out", type=Path, required=True,
                        help="Output delta SQLite path.")
    parser.add_argument("--version", required=True,
                        help="Delta version, e.g. 2026.04.22.")
    parser.add_argument("--applies-to-baseline", required=True,
                        help="Must equal baseline.version in the manifest.")
    parser.add_argument("--baseline-sha256",
                        help="Override baseline sha256 (default: compute from --baseline file).")
    args = parser.parse_args()

    for label, path in [("baseline", args.baseline), ("current", args.current)]:
        if not path.exists():
            print(f"error: {label} DB not found: {path}", file=sys.stderr)
            return 1

    report = build_delta(
        baseline_path=args.baseline,
        current_path=args.current,
        out_path=args.out,
        delta_version=args.version,
        applies_to_baseline=args.applies_to_baseline,
        baseline_sha256=args.baseline_sha256,
    )

    print(f"✓ delta:    {args.out}")
    print(f"  +{report['upserted_documents']} upserts, "
          f"-{report['deleted_documents']} deletes")
    size = report["size_bytes"]
    if size < 1024:
        size_h = f"{size} B"
    elif size < 1024 * 1024:
        size_h = f"{size / 1024:.1f} KB"
    else:
        size_h = f"{size / (1024 * 1024):.2f} MB"
    print(f"  size:    {size} bytes ({size_h})")
    print()
    print("Next:")
    print(f"  zeroclaw vault domain publish-delta \\")
    print(f"      --delta {args.out} \\")
    print(f"      --delta-url https://r2.example.com/<bucket>/$(basename {args.out}) \\")
    print(f"      --delta-version {args.version} \\")
    print(f"      --in-manifest  out/<name>.manifest.json \\")
    print(f"      --out-manifest out/<name>.manifest.json")
    return 0


if __name__ == "__main__":
    sys.exit(main())
