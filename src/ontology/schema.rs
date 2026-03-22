//! SQLite schema initialization and seed data for the ontology layer.
//!
//! All ontology tables live in the same `brain.db` as existing memory tables,
//! avoiding the need for a separate database file.

use rusqlite::Connection;

/// Initialize the ontology schema in the given SQLite connection.
///
/// Safe to call repeatedly — all statements use `IF NOT EXISTS`.
pub fn init_ontology_schema(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        -- ================================================================
        -- 1. Meta-type tables (schema definitions)
        -- ================================================================

        CREATE TABLE IF NOT EXISTS ontology_object_types (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            name        TEXT NOT NULL UNIQUE,
            description TEXT
        );

        CREATE TABLE IF NOT EXISTS ontology_link_types (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            name          TEXT NOT NULL UNIQUE,
            description   TEXT,
            from_type_id  INTEGER NOT NULL DEFAULT 0,
            to_type_id    INTEGER NOT NULL DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS ontology_action_types (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            name          TEXT NOT NULL UNIQUE,
            description   TEXT,
            params_schema TEXT
        );

        -- ================================================================
        -- 2. Instance tables (actual data)
        -- ================================================================

        CREATE TABLE IF NOT EXISTS ontology_objects (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            type_id         INTEGER NOT NULL REFERENCES ontology_object_types(id),
            title           TEXT,
            properties      TEXT NOT NULL DEFAULT '{}',
            owner_user_id   TEXT NOT NULL,
            created_at      INTEGER NOT NULL,
            updated_at      INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_onto_objects_type
            ON ontology_objects(type_id);
        CREATE INDEX IF NOT EXISTS idx_onto_objects_owner
            ON ontology_objects(owner_user_id);
        CREATE INDEX IF NOT EXISTS idx_onto_objects_updated
            ON ontology_objects(updated_at);

        CREATE TABLE IF NOT EXISTS ontology_links (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            link_type_id    INTEGER NOT NULL REFERENCES ontology_link_types(id),
            from_object_id  INTEGER NOT NULL REFERENCES ontology_objects(id),
            to_object_id    INTEGER NOT NULL REFERENCES ontology_objects(id),
            properties      TEXT,
            created_at      INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_onto_links_from
            ON ontology_links(from_object_id);
        CREATE INDEX IF NOT EXISTS idx_onto_links_to
            ON ontology_links(to_object_id);
        CREATE INDEX IF NOT EXISTS idx_onto_links_type
            ON ontology_links(link_type_id);

        -- Prevent duplicate links of the same type between the same objects.
        CREATE UNIQUE INDEX IF NOT EXISTS idx_onto_links_unique_triple
            ON ontology_links(link_type_id, from_object_id, to_object_id);

        -- ================================================================
        -- 3. Action log table
        -- ================================================================

        CREATE TABLE IF NOT EXISTS ontology_actions (
            id                  INTEGER PRIMARY KEY AUTOINCREMENT,
            action_type_id      INTEGER NOT NULL REFERENCES ontology_action_types(id),
            actor_user_id       TEXT NOT NULL,
            actor_kind          TEXT NOT NULL DEFAULT 'agent',
            primary_object_id   INTEGER REFERENCES ontology_objects(id),
            related_object_ids  TEXT,
            params              TEXT NOT NULL DEFAULT '{}',
            result              TEXT,
            channel             TEXT,
            context_id          INTEGER REFERENCES ontology_objects(id),
            -- When (UTC): absolute UTC time (ISO-8601 ending in Z).
            -- PRIMARY SORT KEY for cross-device timeline ordering.
            occurred_at_utc     TEXT,
            -- When (device-local): same instant in device timezone with offset.
            occurred_at_local   TEXT,
            -- IANA timezone of the recording device (e.g. America/New_York).
            timezone            TEXT,
            -- When (home): same instant converted to user home timezone.
            -- DISPLAY TIME for consistent single-timeline view.
            occurred_at_home    TEXT,
            -- User home timezone IANA name (e.g. Asia/Seoul).
            home_timezone       TEXT,
            -- Where: real-world location (free-form text).
            location            TEXT,
            status              TEXT NOT NULL DEFAULT 'pending',
            error_message       TEXT,
            created_at          INTEGER NOT NULL,
            updated_at          INTEGER NOT NULL
        );

        -- Migration: add occurred_at and location to existing tables
        -- (ALTER TABLE ... ADD COLUMN is a no-op if the column already exists
        -- in SQLite 3.35+, but we guard with a pragma check pattern below).

        CREATE INDEX IF NOT EXISTS idx_onto_actions_actor
            ON ontology_actions(actor_user_id);
        CREATE INDEX IF NOT EXISTS idx_onto_actions_type
            ON ontology_actions(action_type_id);
        CREATE INDEX IF NOT EXISTS idx_onto_actions_channel
            ON ontology_actions(channel);
        CREATE INDEX IF NOT EXISTS idx_onto_actions_created
            ON ontology_actions(created_at);
        CREATE INDEX IF NOT EXISTS idx_onto_actions_status
            ON ontology_actions(status);
        -- UTC time is the PRIMARY sort key for cross-device timeline.
        CREATE INDEX IF NOT EXISTS idx_onto_actions_utc
            ON ontology_actions(occurred_at_utc);
        -- Home timezone time for display-oriented queries.
        CREATE INDEX IF NOT EXISTS idx_onto_actions_home
            ON ontology_actions(occurred_at_home);
        -- Location index for place-based grouping.
        CREATE INDEX IF NOT EXISTS idx_onto_actions_location
            ON ontology_actions(location);
        -- Composite: UTC time + location (primary categorization axis).
        CREATE INDEX IF NOT EXISTS idx_onto_actions_when_where
            ON ontology_actions(occurred_at_utc, location);
        -- Composite: location + UTC time (place-first queries).
        CREATE INDEX IF NOT EXISTS idx_onto_actions_where_when
            ON ontology_actions(location, occurred_at_utc);

        -- ================================================================
        -- 4. FTS5 indexes for ontology search
        -- ================================================================

        CREATE VIRTUAL TABLE IF NOT EXISTS ontology_objects_fts USING fts5(
            title,
            properties,
            content='ontology_objects',
            content_rowid='id'
        );

        -- Keep FTS5 in sync with ontology_objects via triggers.
        CREATE TRIGGER IF NOT EXISTS onto_objects_fts_ai
            AFTER INSERT ON ontology_objects BEGIN
            INSERT INTO ontology_objects_fts(rowid, title, properties)
            VALUES (new.id, new.title, new.properties);
        END;

        CREATE TRIGGER IF NOT EXISTS onto_objects_fts_ad
            AFTER DELETE ON ontology_objects BEGIN
            INSERT INTO ontology_objects_fts(ontology_objects_fts, rowid, title, properties)
            VALUES ('delete', old.id, old.title, old.properties);
        END;

        CREATE TRIGGER IF NOT EXISTS onto_objects_fts_au
            AFTER UPDATE ON ontology_objects BEGIN
            INSERT INTO ontology_objects_fts(ontology_objects_fts, rowid, title, properties)
            VALUES ('delete', old.id, old.title, old.properties);
            INSERT INTO ontology_objects_fts(rowid, title, properties)
            VALUES (new.id, new.title, new.properties);
        END;
        ",
    )?;

    // ── Migration: add time/timezone/location columns to existing tables ──
    // Safe to call on every startup — skips if column already exists.
    migrate_add_column(conn, "ontology_actions", "occurred_at_utc", "TEXT")?;
    migrate_add_column(conn, "ontology_actions", "occurred_at_local", "TEXT")?;
    migrate_add_column(conn, "ontology_actions", "timezone", "TEXT")?;
    migrate_add_column(conn, "ontology_actions", "occurred_at_home", "TEXT")?;
    migrate_add_column(conn, "ontology_actions", "home_timezone", "TEXT")?;
    migrate_add_column(conn, "ontology_actions", "location", "TEXT")?;
    // Legacy migration: rename old occurred_at → occurred_at_utc if present.
    migrate_add_column(conn, "ontology_actions", "occurred_at", "TEXT")?;
    // Copy legacy occurred_at data to occurred_at_utc (best-effort).
    let _ = conn.execute_batch(
        "UPDATE ontology_actions
         SET occurred_at_utc = occurred_at
         WHERE occurred_at IS NOT NULL AND occurred_at_utc IS NULL",
    );

    Ok(())
}

/// Add a column to an existing table if it does not already exist.
///
/// Uses `PRAGMA table_info` to check for the column, then runs
/// `ALTER TABLE ... ADD COLUMN` only when needed. This is safe to
/// call on every startup.
fn migrate_add_column(
    conn: &Connection,
    table: &str,
    column: &str,
    col_type: &str,
) -> anyhow::Result<()> {
    let sql = format!("PRAGMA table_info({})", table);
    let mut stmt = conn.prepare(&sql)?;
    let has_column = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .any(|r| r.as_deref() == Ok(column));
    if !has_column {
        let alter = format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, col_type);
        conn.execute_batch(&alter)?;
        tracing::info!(table, column, "ontology schema migration: added column");
    }
    Ok(())
}

/// Seed the default object types, link types, and action types.
///
/// Uses `INSERT OR IGNORE` so it is safe to call on every startup.
pub fn seed_default_types(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        -- ================================================================
        -- Object Types (nouns: entities in the user's world)
        -- ================================================================
        INSERT OR IGNORE INTO ontology_object_types (name, description) VALUES
            ('User',        'MoA account owner'),
            ('Contact',     'Person the user interacts with'),
            ('Device',      'Physical device (PC, phone, tablet)'),
            ('Channel',     'Communication channel (Kakao, Telegram, Desktop, etc.)'),
            ('Task',        'A unit of work or to-do item'),
            ('Project',     'A collection of related tasks and documents'),
            ('Document',    'File, link, contract, specification, or other artifact'),
            ('Meeting',     'Time-bounded session (meeting, call, interpretation)'),
            ('Context',     'Situational context (e.g. CommuteSubwayPhone, OfficePC)'),
            ('Preference',  'User preference or behavioral pattern');

        -- ================================================================
        -- Link Types (relationships between objects)
        --
        -- from_type_id / to_type_id use 0 as wildcard (any type).
        -- The application layer enforces constraints; the DB stores
        -- the intent for documentation and future validation.
        -- ================================================================

        -- Generic relationships (any → any)
        INSERT OR IGNORE INTO ontology_link_types (name, description, from_type_id, to_type_id) VALUES
            ('related_to',          'General association between objects', 0, 0),
            ('belongs_to',          'Hierarchical containment (child → parent)', 0, 0);

        -- User-centric links
        INSERT OR IGNORE INTO ontology_link_types (name, description, from_type_id, to_type_id) VALUES
            ('uses',                'User uses a device or channel', 0, 0),
            ('knows',               'User knows a contact', 0, 0),
            ('has_context',         'User has a situational context', 0, 0),
            ('has_preference',      'User has a preference', 0, 0);

        -- Communication links
        INSERT OR IGNORE INTO ontology_link_types (name, description, from_type_id, to_type_id) VALUES
            ('communicates_via',    'Contact communicates via a channel', 0, 0);

        -- Work links
        INSERT OR IGNORE INTO ontology_link_types (name, description, from_type_id, to_type_id) VALUES
            ('assigned_to',         'Task assigned to a contact or user', 0, 0),
            ('created_for',         'Document created for a project or task', 0, 0),
            ('involves',            'Meeting involves a contact or project', 0, 0),
            ('has_summary',         'Object has a summary document', 0, 0);

        -- ================================================================
        -- Action Types (verbs: things that happen in the user's world)
        -- ================================================================

        -- Communication
        INSERT OR IGNORE INTO ontology_action_types (name, description) VALUES
            ('SendMessage',         'Send a message via a communication channel'),
            ('ReadMessages',        'Read/retrieve messages from a channel'),
            ('ReplyToMessage',      'Reply to a specific message');

        -- Task / Project management
        INSERT OR IGNORE INTO ontology_action_types (name, description) VALUES
            ('CreateTask',          'Create a new task'),
            ('UpdateTask',          'Update task status, priority, or other properties'),
            ('ListTasks',           'List tasks matching criteria'),
            ('LinkTaskToProject',   'Associate a task with a project');

        -- Document / File
        INSERT OR IGNORE INTO ontology_action_types (name, description) VALUES
            ('ReadDocument',        'Open and read a document'),
            ('SummarizeDocument',   'Generate a summary of a document'),
            ('SearchDocuments',     'Search across documents');

        -- Web / HTTP
        INSERT OR IGNORE INTO ontology_action_types (name, description) VALUES
            ('FetchResource',       'Fetch a web/HTTP resource'),
            ('WebSearch',           'Perform a web search');

        -- Schedule / Calendar
        INSERT OR IGNORE INTO ontology_action_types (name, description) VALUES
            ('CreateEvent',         'Create a calendar event'),
            ('UpdateEvent',         'Modify an existing calendar event'),
            ('ListEvents',          'List upcoming calendar events');

        -- Session management
        INSERT OR IGNORE INTO ontology_action_types (name, description) VALUES
            ('StartSession',        'Start a meeting or interpretation session'),
            ('EndSession',          'End a session and trigger summary');

        -- Preference / Insight
        INSERT OR IGNORE INTO ontology_action_types (name, description) VALUES
            ('SavePreference',      'Record a user preference or behavioral pattern'),
            ('RecordDecision',      'Log an important decision for future reference');

        -- System / Meta
        INSERT OR IGNORE INTO ontology_action_types (name, description) VALUES
            ('RunCommand',          'Execute a shell command'),
            ('PlanTasks',           'Generate a task plan for a complex goal');
        ",
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn schema_init_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        // Call twice — should not panic or error.
        init_ontology_schema(&conn).unwrap();
        init_ontology_schema(&conn).unwrap();
    }

    #[test]
    fn seed_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_ontology_schema(&conn).unwrap();
        seed_default_types(&conn).unwrap();
        seed_default_types(&conn).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ontology_object_types", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(
            count >= 10,
            "expected at least 10 object types, got {count}"
        );
    }
}
