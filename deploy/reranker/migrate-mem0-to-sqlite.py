#!/usr/bin/env python3
"""
Migrate memories from mem0 server to ZeroClaw's built-in SQLite brain.db.

Usage:
    # Export from mem0 server, then import to brain.db
    python migrate-mem0-to-sqlite.py \
        --mem0-url http://localhost:18080 \
        --brain-db ~/zeroclaw-workspace/memory/brain.db

    # Or export from mem0 to JSON first, then import separately
    python migrate-mem0-to-sqlite.py --export-only --mem0-url http://localhost:18080 --output memories.json
    python migrate-mem0-to-sqlite.py --import-only --input memories.json --brain-db ~/zeroclaw-workspace/memory/brain.db
"""

import argparse
import json
import os
import sqlite3
import sys
import uuid
from datetime import datetime, timezone


def export_from_mem0(mem0_url: str, output_path: str = None) -> list[dict]:
    """Export all memories from mem0 server via /api/v1/memories/export endpoint."""
    import urllib.request

    url = f"{mem0_url.rstrip('/')}/api/v1/memories/export"
    print(f"Fetching memories from {url} ...")

    try:
        req = urllib.request.Request(url, method="GET")
        with urllib.request.urlopen(req, timeout=30) as resp:
            data = json.loads(resp.read())
    except Exception as e:
        print(f"Error fetching from mem0: {e}")
        print("Make sure the mem0 server has the /export endpoint.")
        sys.exit(1)

    # Support both "memories" and "items" keys (mem0 server uses "items")
    memories = data.get("memories", data.get("items", []))
    total = data.get("total", len(memories))
    print(f"Exported {total} memories from mem0 server")

    if output_path:
        with open(output_path, "w") as f:
            json.dump({"memories": memories, "total": total, "exported_at": datetime.now(timezone.utc).isoformat()}, f, indent=2)
        print(f"Saved to {output_path}")

    return memories


def import_to_sqlite(memories: list[dict], brain_db_path: str):
    """Import memories into ZeroClaw's SQLite brain.db."""

    os.makedirs(os.path.dirname(brain_db_path) or ".", exist_ok=True)

    conn = sqlite3.connect(brain_db_path)

    # Create schema if not exists (matches zeroclaw's init_schema)
    conn.executescript("""
        CREATE TABLE IF NOT EXISTS memories (
            id          TEXT PRIMARY KEY,
            key         TEXT NOT NULL UNIQUE,
            content     TEXT NOT NULL,
            category    TEXT NOT NULL DEFAULT 'core',
            embedding   BLOB,
            created_at  TEXT NOT NULL,
            updated_at  TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_memories_category ON memories(category);
        CREATE INDEX IF NOT EXISTS idx_memories_key ON memories(key);

        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
            key, content, content=memories, content_rowid=rowid
        );

        CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
            INSERT INTO memories_fts(rowid, key, content)
            VALUES (new.rowid, new.key, new.content);
        END;
        CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, key, content)
            VALUES ('delete', old.rowid, old.key, old.content);
        END;
        CREATE TRIGGER IF NOT EXISTS memories_au AFTER UPDATE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, key, content)
            VALUES ('delete', old.rowid, old.key, old.content);
            INSERT INTO memories_fts(rowid, key, content)
            VALUES (new.rowid, new.key, new.content);
        END;

        CREATE TABLE IF NOT EXISTS embedding_cache (
            content_hash TEXT PRIMARY KEY,
            embedding    BLOB NOT NULL,
            created_at   TEXT NOT NULL,
            accessed_at  TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_cache_accessed ON embedding_cache(accessed_at);
    """)

    # Run migrations to add missing columns (same as zeroclaw's init_schema)
    schema_sql = conn.execute(
        "SELECT sql FROM sqlite_master WHERE type='table' AND name='memories'"
    ).fetchone()[0]

    if "session_id" not in schema_sql:
        conn.executescript("""
            ALTER TABLE memories ADD COLUMN session_id TEXT;
            CREATE INDEX IF NOT EXISTS idx_memories_session ON memories(session_id);
        """)

    if "namespace" not in schema_sql:
        conn.executescript("""
            ALTER TABLE memories ADD COLUMN namespace TEXT DEFAULT 'default';
            CREATE INDEX IF NOT EXISTS idx_memories_namespace ON memories(namespace);
        """)

    if "importance" not in schema_sql:
        conn.execute("ALTER TABLE memories ADD COLUMN importance REAL DEFAULT 0.5;")

    if "superseded_by" not in schema_sql:
        conn.execute("ALTER TABLE memories ADD COLUMN superseded_by TEXT;")

    conn.commit()

    # Import memories
    inserted = 0
    skipped = 0
    updated = 0

    for mem in memories:
        mem_id = mem.get("id", str(uuid.uuid4()))
        content = mem.get("memory", mem.get("content", "")).strip()
        if not content:
            skipped += 1
            continue

        # Extract metadata
        metadata = mem.get("metadata", {}) or {}

        # Derive key from content (first 80 chars, sanitized)
        key = metadata.get("key", "")
        if not key:
            key = content[:80].replace("\n", " ").strip()
            # Make unique by appending short hash
            import hashlib
            key = f"{key}_{hashlib.sha256(content.encode()).hexdigest()[:8]}"

        category = metadata.get("category", "core")
        session_id = metadata.get("session_id")
        scope = metadata.get("scope")

        # Parse timestamps
        created_at = mem.get("created_at", mem.get("timestamp", datetime.now(timezone.utc).isoformat()))
        updated_at = mem.get("updated_at", created_at)

        try:
            conn.execute(
                """INSERT INTO memories (id, key, content, category, embedding, created_at, updated_at, session_id, namespace, importance)
                   VALUES (?, ?, ?, ?, NULL, ?, ?, ?, 'default', 0.5)
                   ON CONFLICT(key) DO UPDATE SET
                       content = excluded.content,
                       category = excluded.category,
                       updated_at = excluded.updated_at,
                       session_id = excluded.session_id""",
                (mem_id, key, content, category, created_at, updated_at, session_id),
            )
            if conn.total_changes > inserted + updated + skipped:
                inserted += 1
            else:
                updated += 1
        except sqlite3.IntegrityError:
            # UUID collision — regenerate
            new_id = str(uuid.uuid4())
            try:
                conn.execute(
                    """INSERT INTO memories (id, key, content, category, embedding, created_at, updated_at, session_id, namespace, importance)
                       VALUES (?, ?, ?, ?, NULL, ?, ?, ?, 'default', 0.5)
                       ON CONFLICT(key) DO UPDATE SET
                           content = excluded.content,
                           category = excluded.category,
                           updated_at = excluded.updated_at,
                           session_id = excluded.session_id""",
                    (new_id, key, content, category, created_at, updated_at, session_id),
                )
                inserted += 1
            except Exception as e:
                print(f"  Failed to insert memory {mem_id}: {e}")
                skipped += 1

    conn.commit()

    # Verify
    count = conn.execute("SELECT COUNT(*) FROM memories").fetchone()[0]
    fts_count = conn.execute("SELECT COUNT(*) FROM memories_fts").fetchone()[0]

    conn.close()

    print(f"\nMigration complete:")
    print(f"  Inserted: {inserted}")
    print(f"  Updated:  {updated}")
    print(f"  Skipped:  {skipped}")
    print(f"  Total in DB: {count}")
    print(f"  FTS indexed: {fts_count}")
    print(f"  Embeddings: NULL (will be generated on first search by zeroclaw)")


def main():
    parser = argparse.ArgumentParser(description="Migrate mem0 memories to ZeroClaw SQLite")
    parser.add_argument("--mem0-url", default="http://localhost:18080", help="mem0 server URL")
    parser.add_argument("--brain-db", default=os.path.expanduser("~/zeroclaw-workspace/memory/brain.db"), help="Path to brain.db")
    parser.add_argument("--export-only", action="store_true", help="Only export from mem0 to JSON")
    parser.add_argument("--import-only", action="store_true", help="Only import from JSON to brain.db")
    parser.add_argument("--output", "-o", default="mem0-export.json", help="JSON output path (for --export-only)")
    parser.add_argument("--input", "-i", help="JSON input path (for --import-only)")
    args = parser.parse_args()

    if args.export_only:
        export_from_mem0(args.mem0_url, args.output)
    elif args.import_only:
        if not args.input:
            print("--input required for --import-only")
            sys.exit(1)
        with open(args.input) as f:
            data = json.load(f)
        memories = data.get("memories", data) if isinstance(data, dict) else data
        import_to_sqlite(memories, args.brain_db)
    else:
        memories = export_from_mem0(args.mem0_url)
        if memories:
            import_to_sqlite(memories, args.brain_db)
        else:
            print("No memories to migrate.")


if __name__ == "__main__":
    main()
