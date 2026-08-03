#!/usr/bin/env python3
"""Restore conversation history that the platform could not redeliver.

WHY THIS EXISTS
    WhatsApp acks a message as soon as the client decrypts it, which tells the
    server to drop it from the offline queue. If the daemon dies after the ack
    and before the agent replies, that message is gone: no redelivery, and the
    person is left waiting. Between 2026-08-03 03:38 and 16:46 this deployment
    was down; the phone still holds the thread, the agent's store does not.
    Without a repair the agent would answer as if those turns never happened,
    which is worse than silence — it contradicts what the person can see.

WHY IT IS SAFE TO RUN TWICE
    Re-running a repair is how repairs get run: someone is unsure it worked and
    tries again. `sessions` has no unique constraint, so a naive INSERT would
    silently double every turn and the agent would read the conversation
    stuttering. Idempotency here is structural, not procedural: each row is
    keyed by a content-derived fingerprint recorded in a companion table, and a
    fingerprint already present is skipped. Ten runs converge to one history.

WHAT IT REFUSES TO DO
    It will not touch a session_key that has messages the import does not know
    about in the same time window — that would mean the live agent has been
    talking since the transcript was captured, and blind insertion would
    interleave two versions of reality. It stops and reports instead.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sqlite3
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path

# The transcript's clock. WhatsApp shows local time; the store keeps UTC.
LOCAL = timezone(timedelta(hours=-6))  # America/Mexico_City, no DST in 2026

FINGERPRINT_TABLE = "imported_turns"


def fingerprint(session_key: str, role: str, content: str, created_at: str) -> str:
    """Stable identity for one turn.

    Derived from the payload rather than assigned, so the same transcript
    imported from a different machine, or after the row's autoincrement id has
    changed, still collides with the row already stored.
    """
    payload = "\x1f".join((session_key, role, content, created_at))
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def ensure_ledger(conn: sqlite3.Connection) -> None:
    """Create the fingerprint ledger.

    Separate from `sessions` on purpose: `sessions` is the agent's own schema
    and a repair tool has no business altering it. The UNIQUE constraint lives
    here, where this tool owns it, and the database enforces the guarantee
    rather than the script remembering to check.
    """
    conn.execute(
        f"""CREATE TABLE IF NOT EXISTS {FINGERPRINT_TABLE} (
                fingerprint TEXT PRIMARY KEY,
                session_key TEXT NOT NULL,
                session_row INTEGER NOT NULL,
                imported_at TEXT NOT NULL,
                source      TEXT NOT NULL
            )"""
    )


def load_transcript(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def to_utc(stamp: str) -> str:
    """'2026-08-02 14:12' (local) -> the store's UTC ISO-8601 form."""
    naive = datetime.strptime(stamp, "%Y-%m-%d %H:%M")
    return naive.replace(tzinfo=LOCAL).astimezone(timezone.utc).isoformat()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--db", required=True, type=Path)
    ap.add_argument("--transcript", required=True, type=Path)
    ap.add_argument("--source", default="whatsapp-screenshot")
    ap.add_argument("--apply", action="store_true",
                    help="write; default is a dry run that changes nothing")
    args = ap.parse_args()

    doc = load_transcript(args.transcript)
    session_key = doc["session_key"]
    turns = doc["turns"]
    rows = [
        {
            "role": t["role"],
            "content": t["content"],
            "created_at": to_utc(t["at"]),
        }
        for t in turns
    ]
    for r in rows:
        r["fingerprint"] = fingerprint(
            session_key, r["role"], r["content"], r["created_at"]
        )

    conn = sqlite3.connect(args.db)
    conn.row_factory = sqlite3.Row
    try:
        ensure_ledger(conn)

        # Refuse to merge into a thread that moved on without us.
        window_start = min(r["created_at"] for r in rows)
        known = {
            row["fingerprint"]
            for row in conn.execute(
                f"SELECT fingerprint FROM {FINGERPRINT_TABLE} WHERE session_key = ?",
                (session_key,),
            )
        }
        live = conn.execute(
            """SELECT COUNT(*) AS n FROM sessions
                WHERE session_key = ? AND created_at >= ?""",
            (session_key, window_start),
        ).fetchone()["n"]
        accounted = sum(1 for r in rows if r["fingerprint"] in known)
        if live > accounted:
            print(
                f"REFUSING: {session_key} already holds {live} message(s) at or after "
                f"{window_start}, but only {accounted} came from this transcript.\n"
                "The live conversation has moved on; inserting would interleave two "
                "versions of the same thread. Reconcile by hand.",
                file=sys.stderr,
            )
            return 2

        inserted = skipped = 0
        for r in rows:
            if r["fingerprint"] in known:
                skipped += 1
                continue
            if args.apply:
                cur = conn.execute(
                    """INSERT INTO sessions (session_key, role, content, created_at)
                       VALUES (?, ?, ?, ?)""",
                    (session_key, r["role"], r["content"], r["created_at"]),
                )
                conn.execute(
                    f"""INSERT INTO {FINGERPRINT_TABLE}
                        (fingerprint, session_key, session_row, imported_at, source)
                        VALUES (?, ?, ?, ?, ?)""",
                    (
                        r["fingerprint"],
                        session_key,
                        cur.lastrowid,
                        datetime.now(timezone.utc).isoformat(),
                        args.source,
                    ),
                )
            inserted += 1

        if args.apply:
            # message_count and last_activity are derived from `sessions`;
            # recompute rather than adjust, so the metadata cannot drift away
            # from the rows it describes.
            #
            # UPSERT, not UPDATE: a thread that was lost entirely has no
            # metadata row, and `SessionStore::list_sessions` reads from this
            # table. An UPDATE would touch nothing, the restored turns would
            # never be hydrated, and the repair would look like it worked.
            agg = conn.execute(
                """SELECT COUNT(*) AS n, MIN(created_at) AS first,
                          MAX(created_at) AS last
                     FROM sessions WHERE session_key = ?""",
                (session_key,),
            ).fetchone()
            peer = doc.get("peer", {})
            conn.execute(
                """INSERT INTO session_metadata
                       (session_key, created_at, last_activity, message_count,
                        state, agent_alias, channel_id, room_id, sender_id)
                   VALUES (?, ?, ?, ?, 'idle', ?, ?, ?, ?)
                   ON CONFLICT(session_key) DO UPDATE SET
                       message_count = excluded.message_count,
                       last_activity = excluded.last_activity""",
                (
                    session_key,
                    agg["first"],
                    agg["last"],
                    agg["n"],
                    doc.get("agent_alias"),
                    doc.get("channel_id"),
                    peer.get("lid"),
                    peer.get("phone"),
                ),
            )
            conn.commit()

        verb = "inserted" if args.apply else "would insert"
        print(f"{verb} {inserted}, skipped {skipped} already present")
        if not args.apply:
            print("dry run — nothing written; pass --apply to commit")
        return 0
    finally:
        conn.close()


if __name__ == "__main__":
    raise SystemExit(main())
