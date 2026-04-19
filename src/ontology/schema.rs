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
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            name             TEXT NOT NULL UNIQUE,
            description      TEXT,
            from_type_id     INTEGER NOT NULL DEFAULT 0,
            to_type_id       INTEGER NOT NULL DEFAULT 0,
            cardinality      TEXT NOT NULL DEFAULT 'N:M'
                               CHECK(cardinality IN ('1:1','1:N','N:1','N:M')),
            is_bidirectional INTEGER NOT NULL DEFAULT 1
                               CHECK(is_bidirectional IN (0, 1)),
            inverse_name     TEXT
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
            id                   INTEGER PRIMARY KEY AUTOINCREMENT,
            action_type_id       INTEGER NOT NULL REFERENCES ontology_action_types(id),
            actor_user_id        TEXT NOT NULL,
            actor_kind           TEXT NOT NULL DEFAULT 'agent',
            primary_object_id    INTEGER REFERENCES ontology_objects(id),
            related_object_ids   TEXT,
            -- Palantir-style action metadata (Q1):
            -- Explicit pointers to the typed schema so the cross-search
            -- disambiguation layer can filter without scanning JSON.
            target_type_id       INTEGER REFERENCES ontology_object_types(id),
            relationship_type_id INTEGER REFERENCES ontology_link_types(id),
            params               TEXT NOT NULL DEFAULT '{}',
            result               TEXT,
            channel              TEXT,
            context_id           INTEGER REFERENCES ontology_objects(id),
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

        -- ================================================================
        -- 3b. Palantir-style hybrid schema (Q1 refactor)
        -- ================================================================
        -- Strategy: keep primary `occurred_at_utc`, `location`, `themes` on
        -- the main ontology_actions row as a fast-path cache, and normalize
        -- multi-valued / richer data into dedicated tables. Triggers sync
        -- the cache columns from the normalized source of truth.

        -- ── Typed property schema (replaces ad-hoc JSON for typed fields)
        CREATE TABLE IF NOT EXISTS ontology_property_types (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            object_type_id  INTEGER NOT NULL REFERENCES ontology_object_types(id),
            name            TEXT NOT NULL,
            value_type      TEXT NOT NULL
                             CHECK(value_type IN
                                ('string','int','float','date','geo','object_ref','bool')),
            is_required     INTEGER NOT NULL DEFAULT 0,
            description     TEXT,
            UNIQUE(object_type_id, name)
        );

        CREATE TABLE IF NOT EXISTS ontology_object_properties (
            object_id         INTEGER NOT NULL
                                REFERENCES ontology_objects(id) ON DELETE CASCADE,
            property_type_id  INTEGER NOT NULL
                                REFERENCES ontology_property_types(id),
            value_text        TEXT,
            value_num         REAL,
            value_ts          INTEGER,
            value_ref         INTEGER REFERENCES ontology_objects(id),
            confidence        REAL NOT NULL DEFAULT 1.0,
            updated_at        INTEGER NOT NULL,
            PRIMARY KEY (object_id, property_type_id)
        );
        CREATE INDEX IF NOT EXISTS idx_onto_obj_props_ref
            ON ontology_object_properties(value_ref)
            WHERE value_ref IS NOT NULL;

        -- ── Per-action time table (multi-point, range, recurrence) ──
        CREATE TABLE IF NOT EXISTS ontology_action_times (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            action_id       INTEGER NOT NULL
                                REFERENCES ontology_actions(id) ON DELETE CASCADE,
            time_kind       TEXT NOT NULL
                                CHECK(time_kind IN
                                    ('occurred','started','ended','scheduled','recurring')),
            at_utc          INTEGER,       -- unix sec; point or range start
            at_utc_end      INTEGER,       -- NULL = point; set for ranges
            recurrence_rule TEXT,          -- iCal RRULE for 'recurring'
            confidence      REAL NOT NULL DEFAULT 1.0,
            hlc_stamp       TEXT,
            created_at      INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_onto_action_times_action
            ON ontology_action_times(action_id);
        CREATE INDEX IF NOT EXISTS idx_onto_action_times_at
            ON ontology_action_times(at_utc);
        CREATE INDEX IF NOT EXISTS idx_onto_action_times_kind
            ON ontology_action_times(action_id, time_kind);

        -- ── Per-action place table (multi-location, geohash) ──
        CREATE TABLE IF NOT EXISTS ontology_action_places (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            action_id       INTEGER NOT NULL
                                REFERENCES ontology_actions(id) ON DELETE CASCADE,
            place_role      TEXT NOT NULL
                                CHECK(place_role IN
                                    ('primary','origin','destination','waypoint','stayed_at')),
            place_object_id INTEGER REFERENCES ontology_objects(id),
            place_label     TEXT,          -- free text fallback
            geo_lat         REAL,
            geo_lng         REAL,
            geohash         TEXT,          -- typically 5 chars for ~5 km grid
            arrived_at      INTEGER,
            departed_at     INTEGER,
            confidence      REAL NOT NULL DEFAULT 1.0,
            created_at      INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_onto_action_places_action
            ON ontology_action_places(action_id);
        CREATE INDEX IF NOT EXISTS idx_onto_action_places_geohash
            ON ontology_action_places(geohash);
        CREATE INDEX IF NOT EXISTS idx_onto_action_places_object
            ON ontology_action_places(place_object_id)
            WHERE place_object_id IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_onto_action_places_role
            ON ontology_action_places(action_id, place_role);

        -- ── Normalized theme taxonomy (hierarchy + weighted M:N) ──
        CREATE TABLE IF NOT EXISTS ontology_themes (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            name             TEXT NOT NULL UNIQUE,
            parent_theme_id  INTEGER REFERENCES ontology_themes(id),
            description      TEXT,
            created_at       INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_onto_themes_parent
            ON ontology_themes(parent_theme_id)
            WHERE parent_theme_id IS NOT NULL;

        CREATE TABLE IF NOT EXISTS ontology_action_themes (
            action_id   INTEGER NOT NULL
                            REFERENCES ontology_actions(id) ON DELETE CASCADE,
            theme_id    INTEGER NOT NULL
                            REFERENCES ontology_themes(id),
            weight      REAL NOT NULL DEFAULT 1.0
                            CHECK(weight >= 0.0 AND weight <= 1.0),
            PRIMARY KEY (action_id, theme_id)
        );
        CREATE INDEX IF NOT EXISTS idx_onto_action_themes_theme
            ON ontology_action_themes(theme_id);

        CREATE TABLE IF NOT EXISTS ontology_object_themes (
            object_id   INTEGER NOT NULL
                            REFERENCES ontology_objects(id) ON DELETE CASCADE,
            theme_id    INTEGER NOT NULL
                            REFERENCES ontology_themes(id),
            weight      REAL NOT NULL DEFAULT 1.0
                            CHECK(weight >= 0.0 AND weight <= 1.0),
            PRIMARY KEY (object_id, theme_id)
        );
        CREATE INDEX IF NOT EXISTS idx_onto_object_themes_theme
            ON ontology_object_themes(theme_id);

        -- ── Primary-cache sync triggers ──
        -- Keep ontology_actions.occurred_at_utc / location backing the fast
        -- path in sync with the first row written to the detail tables.
        -- Only fills when the main row is NULL to avoid clobbering an
        -- explicit write; callers that want to replace the primary value
        -- should UPDATE it directly.
        CREATE TRIGGER IF NOT EXISTS trg_action_times_primary_sync
            AFTER INSERT ON ontology_action_times
            WHEN NEW.time_kind IN ('occurred','started') AND NEW.at_utc IS NOT NULL
        BEGIN
            UPDATE ontology_actions
               SET occurred_at_utc = datetime(NEW.at_utc, 'unixepoch') || 'Z'
             WHERE id = NEW.action_id AND occurred_at_utc IS NULL;
        END;

        CREATE TRIGGER IF NOT EXISTS trg_action_places_primary_sync
            AFTER INSERT ON ontology_action_places
            WHEN NEW.place_role = 'primary'
        BEGIN
            UPDATE ontology_actions
               SET location = COALESCE(NEW.place_label,
                                       (SELECT title FROM ontology_objects
                                         WHERE id = NEW.place_object_id))
             WHERE id = NEW.action_id AND location IS NULL;
        END;

        -- ================================================================
        -- 4. Community layer (PR #9 — GraphRAG Phase 5)
        -- ================================================================
        -- One row per detected community in the ontology graph. `level`
        -- supports future hierarchical clustering (Leiden's hierarchy);
        -- this commit only writes level=0. `parent_community_id` is
        -- nullable for the root level. `summary_embedding` is a LE-f32
        -- BLOB so it slots into the existing embedding cache pipeline.

        CREATE TABLE IF NOT EXISTS ontology_communities (
            id                   INTEGER PRIMARY KEY AUTOINCREMENT,
            community_id         INTEGER NOT NULL,
            level                INTEGER NOT NULL DEFAULT 0,
            parent_community_id  INTEGER REFERENCES ontology_communities(id),
            summary              TEXT NOT NULL,
            summary_embedding    BLOB,
            object_ids           TEXT NOT NULL,
            keywords             TEXT NOT NULL DEFAULT '[]',
            created_at           INTEGER NOT NULL DEFAULT (unixepoch())
        );
        CREATE INDEX IF NOT EXISTS idx_onto_communities_level
            ON ontology_communities(level);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_onto_communities_unique
            ON ontology_communities(level, community_id);

        -- ================================================================
        -- 5. FTS5 indexes for ontology search
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
    // ── Theme (주제/테마) migration ──
    // A JSON array of theme strings extracted from the action.
    // Example: ["차용금반환청구", "물품대금청구", "소장"] for a lawsuit filing.
    // Enables fast theme-based categorization and retrieval.
    migrate_add_column(conn, "ontology_actions", "themes", "TEXT")?;
    migrate_add_column(conn, "ontology_objects", "themes", "TEXT")?;
    // ── Q1 Commit #4: link-type strengthening + action target/relationship ─
    migrate_add_column(conn, "ontology_link_types", "cardinality",
                       "TEXT NOT NULL DEFAULT 'N:M'")?;
    migrate_add_column(conn, "ontology_link_types", "is_bidirectional",
                       "INTEGER NOT NULL DEFAULT 1")?;
    migrate_add_column(conn, "ontology_link_types", "inverse_name", "TEXT")?;
    migrate_add_column(conn, "ontology_actions", "target_type_id",
                       "INTEGER REFERENCES ontology_object_types(id)")?;
    migrate_add_column(conn, "ontology_actions", "relationship_type_id",
                       "INTEGER REFERENCES ontology_link_types(id)")?;
    // Legacy migration: rename old occurred_at → occurred_at_utc if present.
    migrate_add_column(conn, "ontology_actions", "occurred_at", "TEXT")?;
    // Copy legacy occurred_at data to occurred_at_utc (best-effort).
    let _ = conn.execute_batch(
        "UPDATE ontology_actions
         SET occurred_at_utc = occurred_at
         WHERE occurred_at IS NOT NULL AND occurred_at_utc IS NULL",
    );

    // Create indexes on migrated columns AFTER migration ensures they exist.
    // These were previously in execute_batch but caused failures on old DBs
    // that lacked these columns (CREATE INDEX ran before ALTER TABLE).
    let _ = conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_onto_actions_utc
             ON ontology_actions(occurred_at_utc);
         CREATE INDEX IF NOT EXISTS idx_onto_actions_home
             ON ontology_actions(occurred_at_home);
         CREATE INDEX IF NOT EXISTS idx_onto_actions_location
             ON ontology_actions(location);
         CREATE INDEX IF NOT EXISTS idx_onto_actions_when_where
             ON ontology_actions(occurred_at_utc, location);
         CREATE INDEX IF NOT EXISTS idx_onto_actions_where_when
             ON ontology_actions(location, occurred_at_utc);
         CREATE INDEX IF NOT EXISTS idx_onto_actions_themes
             ON ontology_actions(themes);
         CREATE INDEX IF NOT EXISTS idx_onto_objects_themes
             ON ontology_objects(themes);
         CREATE INDEX IF NOT EXISTS idx_onto_link_types_pair
             ON ontology_link_types(from_type_id, to_type_id);
         CREATE INDEX IF NOT EXISTS idx_onto_actions_target_type
             ON ontology_actions(target_type_id)
             WHERE target_type_id IS NOT NULL;
         CREATE INDEX IF NOT EXISTS idx_onto_actions_relationship
             ON ontology_actions(relationship_type_id)
             WHERE relationship_type_id IS NOT NULL;",
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

    /// Q1 hybrid schema: ontology_action_times / _places / _themes
    /// should exist, accept normalized rows, and the "primary-cache"
    /// triggers should back-fill ontology_actions.{occurred_at_utc, location}.
    #[test]
    fn hybrid_action_detail_tables_roundtrip_and_trigger_sync() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        init_ontology_schema(&conn).unwrap();
        seed_default_types(&conn).unwrap();

        // Insert a Contact object to act as the "place" (though Place-type
        // objects would normally be used in production).
        conn.execute(
            "INSERT INTO ontology_objects
                (type_id, title, properties, owner_user_id, created_at, updated_at)
             VALUES (
                (SELECT id FROM ontology_object_types WHERE name='Contact'),
                '제주 ** 골프장',
                '{}',
                'user_test', 1000, 1000
             )",
            [],
        )
        .unwrap();
        let place_obj_id: i64 = conn.last_insert_rowid();

        // Insert an action (occurred_at_utc / location left NULL — the triggers
        // should fill them from the normalized detail rows).
        conn.execute(
            "INSERT INTO ontology_actions
                (action_type_id, actor_user_id,
                 primary_object_id, params, created_at, updated_at)
             VALUES (
                (SELECT id FROM ontology_action_types WHERE name='RecordDecision'),
                'user_test', NULL, '{}', 1000, 1000
             )",
            [],
        )
        .unwrap();
        let action_id: i64 = conn.last_insert_rowid();

        // Normalized time rows.
        conn.execute(
            "INSERT INTO ontology_action_times
                (action_id, time_kind, at_utc, at_utc_end, confidence, created_at)
             VALUES (?1, 'started',  1_744_000_000, NULL, 0.9, 1000),
                    (?1, 'ended',    1_744_010_000, NULL, 0.9, 1000),
                    (?1, 'occurred', 1_744_005_000, NULL, 1.0, 1000)",
            rusqlite::params![action_id],
        )
        .unwrap();

        // Normalized place rows — one 'primary' object-backed, one label waypoint.
        conn.execute(
            "INSERT INTO ontology_action_places
                (action_id, place_role, place_object_id, place_label,
                 geo_lat, geo_lng, geohash, confidence, created_at)
             VALUES (?1, 'primary',  ?2, NULL,           33.43, 126.54, 'wy74b', 1.0, 1000),
                    (?1, 'waypoint', NULL, 'Jeju Airport', NULL, NULL, 'wy74a', 1.0, 1000)",
            rusqlite::params![action_id, place_obj_id],
        )
        .unwrap();

        // Normalized theme rows — taxonomy with parent/child.
        conn.execute(
            "INSERT INTO ontology_themes (name, parent_theme_id, description, created_at)
             VALUES ('스포츠', NULL, '운동 전반', 1000)",
            [],
        )
        .unwrap();
        let sport_id: i64 = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO ontology_themes (name, parent_theme_id, description, created_at)
             VALUES ('골프', ?1, '골프 활동', 1000)",
            rusqlite::params![sport_id],
        )
        .unwrap();
        let golf_id: i64 = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO ontology_themes (name, parent_theme_id, description, created_at)
             VALUES ('사교', NULL, '사회적 모임', 1000)",
            [],
        )
        .unwrap();
        let social_id: i64 = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO ontology_action_themes (action_id, theme_id, weight)
             VALUES (?1, ?2, 0.8), (?1, ?3, 0.5)",
            rusqlite::params![action_id, golf_id, social_id],
        )
        .unwrap();

        // Primary-cache triggers should have filled ontology_actions.
        let (primary_at, primary_loc): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT occurred_at_utc, location FROM ontology_actions WHERE id = ?1",
                rusqlite::params![action_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        // First 'started' insert was the trigger source (NEW.time_kind IN ('occurred','started')).
        assert!(
            primary_at.is_some(),
            "trigger should have populated occurred_at_utc"
        );
        assert!(primary_at.as_deref().unwrap().ends_with('Z'));
        assert_eq!(
            primary_loc.as_deref(),
            Some("제주 ** 골프장"),
            "primary place trigger should copy title from place_object_id"
        );

        // Theme hierarchy lookup — find every action tagged under 스포츠 (parent),
        // which should include ones tagged with 골프 via the parent_theme_id chain.
        let action_count: i64 = conn
            .query_row(
                "WITH theme_tree(id) AS (
                    SELECT id FROM ontology_themes WHERE name = '스포츠'
                    UNION ALL
                    SELECT t.id FROM ontology_themes t
                    JOIN theme_tree tt ON t.parent_theme_id = tt.id
                 )
                 SELECT COUNT(DISTINCT at.action_id)
                 FROM ontology_action_themes at
                 WHERE at.theme_id IN theme_tree",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(action_count, 1);

        // FK CASCADE: delete the action, expect detail rows to vanish.
        conn.execute(
            "DELETE FROM ontology_actions WHERE id = ?1",
            rusqlite::params![action_id],
        )
        .unwrap();
        let orphaned: i64 = conn
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM ontology_action_times  WHERE action_id = ?1)
                  + (SELECT COUNT(*) FROM ontology_action_places WHERE action_id = ?1)
                  + (SELECT COUNT(*) FROM ontology_action_themes WHERE action_id = ?1)",
                rusqlite::params![action_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphaned, 0, "ON DELETE CASCADE should clear detail rows");
    }

    /// Q1 Commit #4 — link types gain cardinality + bidirectional + inverse
    /// metadata; actions gain explicit target_type / relationship_type pointers.
    #[test]
    fn link_type_strengthening_and_action_metadata_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        init_ontology_schema(&conn).unwrap();
        seed_default_types(&conn).unwrap();

        // Verify seeded link types default to the new columns safely.
        let (default_card, default_bidir): (String, i64) = conn
            .query_row(
                "SELECT cardinality, is_bidirectional
                 FROM ontology_link_types WHERE name = 'related_to'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(default_card, "N:M");
        assert_eq!(default_bidir, 1);

        // Define a tight 1:N link type: 'employed_by' (one Company employs many People).
        let contact_type_id: i64 = conn
            .query_row(
                "SELECT id FROM ontology_object_types WHERE name = 'Contact'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO ontology_link_types
                (name, description, from_type_id, to_type_id,
                 cardinality, is_bidirectional, inverse_name)
             VALUES ('boyfriend_of',
                     'Dating relationship (symmetric)',
                     ?1, ?1,
                     '1:1', 1, 'girlfriend_of')",
            rusqlite::params![contact_type_id],
        )
        .unwrap();
        let boyfriend_link_id: i64 = conn.last_insert_rowid();

        // CHECK constraint rejects an unknown cardinality value.
        let bad = conn.execute(
            "INSERT INTO ontology_link_types
                (name, cardinality, is_bidirectional)
             VALUES ('bogus', 'infinity', 1)",
            [],
        );
        assert!(bad.is_err(), "cardinality CHECK should reject unknown value");

        // CHECK constraint rejects an out-of-range bidirectional flag.
        let bad = conn.execute(
            "INSERT INTO ontology_link_types
                (name, cardinality, is_bidirectional)
             VALUES ('bogus2', 'N:M', 2)",
            [],
        );
        assert!(
            bad.is_err(),
            "is_bidirectional CHECK should reject values outside {{0,1}}"
        );

        // Action metadata: log an action with explicit target_type_id + relationship_type_id.
        conn.execute(
            "INSERT INTO ontology_objects
                (type_id, title, properties, owner_user_id, created_at, updated_at)
             VALUES (?1, '김필순', '{}', 'user_test', 1000, 1000)",
            rusqlite::params![contact_type_id],
        )
        .unwrap();
        let girlfriend_id: i64 = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO ontology_actions
                (action_type_id, actor_user_id,
                 primary_object_id, target_type_id, relationship_type_id,
                 params, created_at, updated_at)
             VALUES (
                (SELECT id FROM ontology_action_types WHERE name='RecordDecision'),
                'user_test',
                ?1, ?2, ?3,
                '{}', 1000, 1000
             )",
            rusqlite::params![girlfriend_id, contact_type_id, boyfriend_link_id],
        )
        .unwrap();

        // Confirm the action row carries both FK pointers and the partial
        // indexes pick them up (verify by selecting through them).
        let (tgt, rel): (i64, i64) = conn
            .query_row(
                "SELECT target_type_id, relationship_type_id
                 FROM ontology_actions
                 WHERE primary_object_id = ?1",
                rusqlite::params![girlfriend_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(tgt, contact_type_id);
        assert_eq!(rel, boyfriend_link_id);

        // Partial index usage — `target_type_id IS NOT NULL` filter works.
        let count_with_target: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ontology_actions
                 WHERE target_type_id = ?1",
                rusqlite::params![contact_type_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count_with_target, 1);
    }

    /// Typed property system: property types are defined per object type,
    /// and typed values land in the normalized ontology_object_properties table.
    #[test]
    fn typed_property_schema_enforces_object_type_binding() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        init_ontology_schema(&conn).unwrap();
        seed_default_types(&conn).unwrap();

        // Define a 'nickname' string property on Contact.
        let contact_type_id: i64 = conn
            .query_row(
                "SELECT id FROM ontology_object_types WHERE name = 'Contact'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO ontology_property_types
                (object_type_id, name, value_type, is_required, description)
             VALUES (?1, 'nickname', 'string', 0, '별명')",
            rusqlite::params![contact_type_id],
        )
        .unwrap();
        let nickname_prop_id: i64 = conn.last_insert_rowid();

        // Create a Contact and attach the nickname property.
        conn.execute(
            "INSERT INTO ontology_objects
                (type_id, title, properties, owner_user_id, created_at, updated_at)
             VALUES (?1, '김필순', '{}', 'user_test', 1000, 1000)",
            rusqlite::params![contact_type_id],
        )
        .unwrap();
        let contact_id: i64 = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO ontology_object_properties
                (object_id, property_type_id, value_text, confidence, updated_at)
             VALUES (?1, ?2, '필순이', 1.0, 1000)",
            rusqlite::params![contact_id, nickname_prop_id],
        )
        .unwrap();

        let nickname: String = conn
            .query_row(
                "SELECT value_text FROM ontology_object_properties
                 WHERE object_id = ?1 AND property_type_id = ?2",
                rusqlite::params![contact_id, nickname_prop_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(nickname, "필순이");

        // Reject invalid value_type via the CHECK constraint.
        let bad_insert = conn.execute(
            "INSERT INTO ontology_property_types
                (object_type_id, name, value_type, is_required)
             VALUES (?1, 'foo', 'not_a_real_type', 0)",
            rusqlite::params![contact_type_id],
        );
        assert!(bad_insert.is_err(), "CHECK should reject unknown value_type");

        // UNIQUE(object_type_id, name) prevents duplicate property definitions.
        let dup_insert = conn.execute(
            "INSERT INTO ontology_property_types
                (object_type_id, name, value_type, is_required)
             VALUES (?1, 'nickname', 'string', 0)",
            rusqlite::params![contact_type_id],
        );
        assert!(dup_insert.is_err());
    }
}
