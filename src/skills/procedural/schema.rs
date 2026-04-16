//! Database schema for the procedural skill system.
//!
//! Tables live inside brain.db alongside memory_entries and timeline_entries.
//! Migration is idempotent (CREATE TABLE IF NOT EXISTS).

use rusqlite::Connection;

/// Run all skill-related table migrations on the given connection.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS skills (
            id            TEXT PRIMARY KEY,
            name          TEXT UNIQUE NOT NULL,
            category      TEXT,
            description   TEXT NOT NULL,
            content_md    TEXT NOT NULL,
            version       INTEGER DEFAULT 1,
            use_count     INTEGER DEFAULT 0,
            success_count INTEGER DEFAULT 0,
            created_at    INTEGER DEFAULT (unixepoch()),
            updated_at    INTEGER DEFAULT (unixepoch()),
            created_by    TEXT DEFAULT 'agent',
            device_id     TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS skill_references (
            id        INTEGER PRIMARY KEY AUTOINCREMENT,
            skill_id  TEXT REFERENCES skills(id) ON DELETE CASCADE,
            file_path TEXT NOT NULL,
            content   TEXT NOT NULL,
            UNIQUE(skill_id, file_path)
        );

        -- Standalone FTS5 table (no content= binding) populated manually from
        -- Rust after each write. We key by the skill_id string stored as
        -- an extra column so we can DELETE … MATCH by skill_id.
        CREATE VIRTUAL TABLE IF NOT EXISTS skills_fts USING fts5(
            skill_id UNINDEXED,
            name,
            description,
            content_md,
            tokenize='trigram'
        );
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
        migrate(&conn).unwrap(); // second call must not error
    }

    #[test]
    fn fts5_table_exists_after_migration() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='skills_fts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}
