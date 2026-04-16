//! Database schema for the self-learning correction system.

use rusqlite::Connection;

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS correction_observations (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            uuid            TEXT UNIQUE NOT NULL,
            original_text   TEXT NOT NULL,
            corrected_text  TEXT NOT NULL,
            context_before  TEXT,
            context_after   TEXT,
            document_type   TEXT,
            category        TEXT,
            source          TEXT NOT NULL,
            grammar_valid   INTEGER DEFAULT 1,
            observed_at     INTEGER DEFAULT (unixepoch()),
            session_id      TEXT,
            device_id       TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_co_category
            ON correction_observations(category);

        CREATE INDEX IF NOT EXISTS idx_co_observed
            ON correction_observations(observed_at);

        CREATE TABLE IF NOT EXISTS correction_patterns (
            id                INTEGER PRIMARY KEY AUTOINCREMENT,
            pattern_type      TEXT NOT NULL,
            original_regex    TEXT NOT NULL,
            replacement       TEXT NOT NULL,
            scope             TEXT DEFAULT 'all',
            confidence        REAL DEFAULT 0.3,
            observation_count INTEGER DEFAULT 1,
            accept_count      INTEGER DEFAULT 0,
            reject_count      INTEGER DEFAULT 0,
            is_active         INTEGER DEFAULT 1,
            created_at        INTEGER DEFAULT (unixepoch()),
            updated_at        INTEGER DEFAULT (unixepoch()),
            device_id         TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_cp_active
            ON correction_patterns(is_active, confidence);

        CREATE INDEX IF NOT EXISTS idx_cp_scope
            ON correction_patterns(scope);

        CREATE TABLE IF NOT EXISTS pattern_observations (
            pattern_id     INTEGER REFERENCES correction_patterns(id) ON DELETE CASCADE,
            observation_id INTEGER REFERENCES correction_observations(id) ON DELETE CASCADE,
            PRIMARY KEY (pattern_id, observation_id)
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS correction_patterns_fts
            USING fts5(original_regex, replacement, scope, tokenize='trigram');
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
