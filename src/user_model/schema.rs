//! Database schema for user profile conclusions.

use rusqlite::Connection;

/// Run user profile table migrations on the given connection.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS user_profile_conclusions (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            dimension       TEXT NOT NULL,
            conclusion      TEXT NOT NULL,
            confidence      REAL DEFAULT 0.5,
            evidence_count  INTEGER DEFAULT 1,
            first_observed  INTEGER DEFAULT (unixepoch()),
            last_updated    INTEGER DEFAULT (unixepoch()),
            device_id       TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_upc_dimension
            ON user_profile_conclusions(dimension);

        CREATE INDEX IF NOT EXISTS idx_upc_confidence
            ON user_profile_conclusions(confidence);
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
