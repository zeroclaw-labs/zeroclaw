//! Database schema for session-transcript full-text search.

use rusqlite::Connection;

/// Run session search table migrations.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS chat_sessions (
            id          TEXT PRIMARY KEY,
            platform    TEXT,
            category    TEXT,
            title       TEXT,
            started_at  INTEGER DEFAULT (unixepoch()),
            ended_at    INTEGER,
            device_id   TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS chat_messages (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id  TEXT REFERENCES chat_sessions(id) ON DELETE CASCADE,
            role        TEXT NOT NULL,
            content     TEXT NOT NULL,
            timestamp   INTEGER DEFAULT (unixepoch())
        );

        CREATE INDEX IF NOT EXISTS idx_cm_session
            ON chat_messages(session_id, timestamp);

        CREATE VIRTUAL TABLE IF NOT EXISTS chat_messages_fts
            USING fts5(content, tokenize='trigram', content='chat_messages', content_rowid='id');

        CREATE TRIGGER IF NOT EXISTS chat_messages_ai AFTER INSERT ON chat_messages BEGIN
            INSERT INTO chat_messages_fts(rowid, content) VALUES (new.id, new.content);
        END;

        CREATE TRIGGER IF NOT EXISTS chat_messages_ad AFTER DELETE ON chat_messages BEGIN
            INSERT INTO chat_messages_fts(chat_messages_fts, rowid, content)
            VALUES ('delete', old.id, old.content);
        END;

        CREATE TRIGGER IF NOT EXISTS chat_messages_au AFTER UPDATE ON chat_messages BEGIN
            INSERT INTO chat_messages_fts(chat_messages_fts, rowid, content)
            VALUES ('delete', old.id, old.content);
            INSERT INTO chat_messages_fts(rowid, content) VALUES (new.id, new.content);
        END;
        ",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
    }
}
