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
        -- 3c. Denormalized materialized index (Q1 step #5 -- 50-year scale)
        -- ================================================================
        -- One row per action, maintained by triggers against the normalized
        -- detail tables (times/places/themes). The cross-search path can
        -- answer ~90% of queries (when/where/who/theme) with a single
        -- table scan, avoiding 4-way joins that become costly at 10M-row
        -- scale. The base ontology_actions row is still the source of
        -- truth; this table is a projection.

        CREATE TABLE IF NOT EXISTS ontology_action_index (
            action_id           INTEGER PRIMARY KEY
                                  REFERENCES ontology_actions(id) ON DELETE CASCADE,
            tier                INTEGER NOT NULL DEFAULT 1,
            actor_user_id       TEXT NOT NULL,
            action_type_id      INTEGER NOT NULL,
            primary_time_utc    INTEGER,
            primary_geohash_5   TEXT,     -- 5-char geohash (~5 km grid)
            primary_place_id    INTEGER REFERENCES ontology_objects(id),
            theme_ids           TEXT,     -- comma-separated; '' if no themes
            confidence_max      REAL NOT NULL DEFAULT 0.0,
            participant_count   INTEGER NOT NULL DEFAULT 0,
            updated_at          INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_action_idx_time_tier
            ON ontology_action_index(tier, primary_time_utc DESC);
        CREATE INDEX IF NOT EXISTS idx_action_idx_geohash
            ON ontology_action_index(primary_geohash_5)
            WHERE primary_geohash_5 IS NOT NULL;
        CREATE INDEX IF NOT EXISTS idx_action_idx_actor_time
            ON ontology_action_index(actor_user_id, primary_time_utc DESC);
        CREATE INDEX IF NOT EXISTS idx_action_idx_place
            ON ontology_action_index(primary_place_id)
            WHERE primary_place_id IS NOT NULL;

        -- ── Population triggers ──
        -- 1) New action row → bootstrap an index row with whatever the
        --    main row already knows (cache columns); detail-table rows
        --    update it below as they land.
        CREATE TRIGGER IF NOT EXISTS trg_action_index_ai
            AFTER INSERT ON ontology_actions
        BEGIN
            INSERT OR REPLACE INTO ontology_action_index
                (action_id, tier, actor_user_id, action_type_id,
                 primary_time_utc, updated_at)
            VALUES (
                NEW.id, 1, NEW.actor_user_id, NEW.action_type_id,
                CASE
                    WHEN NEW.occurred_at_utc IS NOT NULL
                    THEN CAST(strftime('%s', NEW.occurred_at_utc) AS INTEGER)
                    ELSE NULL
                END,
                NEW.updated_at
            );
        END;

        -- 2) Main action row updated → refresh actor/type/time cache fields.
        CREATE TRIGGER IF NOT EXISTS trg_action_index_au
            AFTER UPDATE ON ontology_actions
        BEGIN
            UPDATE ontology_action_index SET
                actor_user_id    = NEW.actor_user_id,
                action_type_id   = NEW.action_type_id,
                primary_time_utc = CASE
                    WHEN NEW.occurred_at_utc IS NOT NULL
                    THEN CAST(strftime('%s', NEW.occurred_at_utc) AS INTEGER)
                    ELSE primary_time_utc
                END,
                updated_at       = NEW.updated_at
             WHERE action_id = NEW.id;
        END;

        -- 3) New place row → mirror into the index if this is the 'primary'
        --    role. Prefer the explicit object pointer, fall back to label.
        CREATE TRIGGER IF NOT EXISTS trg_action_index_place_ai
            AFTER INSERT ON ontology_action_places
            WHEN NEW.place_role = 'primary'
        BEGIN
            UPDATE ontology_action_index SET
                primary_place_id  = NEW.place_object_id,
                primary_geohash_5 = CASE
                    WHEN NEW.geohash IS NOT NULL THEN substr(NEW.geohash, 1, 5)
                    ELSE primary_geohash_5
                END,
                confidence_max    = MAX(confidence_max, NEW.confidence),
                updated_at        = strftime('%s','now')
             WHERE action_id = NEW.action_id;
        END;

        -- 4) New time row with 'occurred' or 'started' → promote to primary
        --    time cache when the index row is still NULL-only.
        CREATE TRIGGER IF NOT EXISTS trg_action_index_time_ai
            AFTER INSERT ON ontology_action_times
            WHEN NEW.time_kind IN ('occurred','started') AND NEW.at_utc IS NOT NULL
        BEGIN
            UPDATE ontology_action_index SET
                primary_time_utc = COALESCE(primary_time_utc, NEW.at_utc),
                confidence_max   = MAX(confidence_max, NEW.confidence),
                updated_at       = strftime('%s','now')
             WHERE action_id = NEW.action_id;
        END;

        -- 5) Theme link (ontology_action_themes) add/remove → rebuild the
        --    theme_ids column as a comma-separated sorted list of ids.
        CREATE TRIGGER IF NOT EXISTS trg_action_index_theme_ai
            AFTER INSERT ON ontology_action_themes
        BEGIN
            UPDATE ontology_action_index SET
                theme_ids = (
                    SELECT group_concat(theme_id, ',')
                      FROM (SELECT theme_id FROM ontology_action_themes
                             WHERE action_id = NEW.action_id
                             ORDER BY theme_id)
                ),
                updated_at = strftime('%s','now')
             WHERE action_id = NEW.action_id;
        END;

        CREATE TRIGGER IF NOT EXISTS trg_action_index_theme_ad
            AFTER DELETE ON ontology_action_themes
        BEGIN
            UPDATE ontology_action_index SET
                theme_ids = (
                    SELECT group_concat(theme_id, ',')
                      FROM (SELECT theme_id FROM ontology_action_themes
                             WHERE action_id = OLD.action_id
                             ORDER BY theme_id)
                ),
                updated_at = strftime('%s','now')
             WHERE action_id = OLD.action_id;
        END;

        -- ================================================================
        -- 3d. R-Tree spatial index (Q1 step #6 -- O(log n) geo lookup)
        -- ================================================================
        -- Built-in SQLite R*Tree for O(log n) bounding-box spatial queries.
        -- Separate from geohash-prefix matching: R-Tree handles continuous
        -- lat/lng ranges (e.g. within-5km queries), geohash handles
        -- discretized grid equality (same-cell-as matches). Cross-search
        -- uses geohash for disambiguation ties, R-Tree for proximity.
        --
        -- The virtual table stores bounding boxes. For point-locations the
        -- min/max collapse (min_lat == max_lat == lat). Populated and torn
        -- down via triggers from ontology_action_places.

        CREATE VIRTUAL TABLE IF NOT EXISTS ontology_action_geo USING rtree(
            action_place_id,  -- rowid = ontology_action_places.id (1:1)
            min_lat, max_lat,
            min_lng, max_lng
        );

        -- Populate on INSERT when both coords are present.
        CREATE TRIGGER IF NOT EXISTS trg_action_geo_ai
            AFTER INSERT ON ontology_action_places
            WHEN NEW.geo_lat IS NOT NULL AND NEW.geo_lng IS NOT NULL
        BEGIN
            INSERT OR REPLACE INTO ontology_action_geo
                (action_place_id, min_lat, max_lat, min_lng, max_lng)
            VALUES
                (NEW.id, NEW.geo_lat, NEW.geo_lat, NEW.geo_lng, NEW.geo_lng);
        END;

        -- Update when coordinates change on an existing place row.
        CREATE TRIGGER IF NOT EXISTS trg_action_geo_au
            AFTER UPDATE OF geo_lat, geo_lng ON ontology_action_places
        BEGIN
            DELETE FROM ontology_action_geo WHERE action_place_id = OLD.id;
            INSERT INTO ontology_action_geo
                (action_place_id, min_lat, max_lat, min_lng, max_lng)
            SELECT NEW.id, NEW.geo_lat, NEW.geo_lat, NEW.geo_lng, NEW.geo_lng
             WHERE NEW.geo_lat IS NOT NULL AND NEW.geo_lng IS NOT NULL;
        END;

        -- Clean up the R-Tree when the place row is deleted.
        CREATE TRIGGER IF NOT EXISTS trg_action_geo_ad
            AFTER DELETE ON ontology_action_places
        BEGIN
            DELETE FROM ontology_action_geo WHERE action_place_id = OLD.id;
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

    /// Q1 Commit #5 — denormalized index table (ontology_action_index) is
    /// maintained by triggers against action + times + places + themes.
    /// Cross-search 90% paths hit a single table scan instead of 4-way joins.
    #[test]
    fn materialized_action_index_stays_consistent_with_source() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        init_ontology_schema(&conn).unwrap();
        seed_default_types(&conn).unwrap();

        // Seed place object + themes.
        let contact_type_id: i64 = conn
            .query_row(
                "SELECT id FROM ontology_object_types WHERE name = 'Contact'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO ontology_objects
                (type_id, title, properties, owner_user_id, created_at, updated_at)
             VALUES (?1, '제주 ** 골프장', '{}', 'user_test', 1000, 1000)",
            rusqlite::params![contact_type_id],
        )
        .unwrap();
        let place_id: i64 = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO ontology_themes (name, parent_theme_id, description, created_at)
             VALUES ('골프', NULL, '', 1000), ('사교', NULL, '', 1000)",
            [],
        )
        .unwrap();
        let golf_id: i64 = conn
            .query_row(
                "SELECT id FROM ontology_themes WHERE name = '골프'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let social_id: i64 = conn
            .query_row(
                "SELECT id FROM ontology_themes WHERE name = '사교'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        // Step 1: insert action. Index row should be bootstrapped.
        conn.execute(
            "INSERT INTO ontology_actions
                (action_type_id, actor_user_id,
                 primary_object_id, occurred_at_utc, params, created_at, updated_at)
             VALUES (
                (SELECT id FROM ontology_action_types WHERE name='RecordDecision'),
                'user_test', NULL, '2026-04-12T09:00:00Z', '{}', 1000, 1000
             )",
            [],
        )
        .unwrap();
        let action_id: i64 = conn.last_insert_rowid();

        let (actor, type_id, time, place_from_index, themes): (
            String,
            i64,
            Option<i64>,
            Option<i64>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT actor_user_id, action_type_id,
                        primary_time_utc, primary_place_id, theme_ids
                 FROM ontology_action_index WHERE action_id = ?1",
                rusqlite::params![action_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(actor, "user_test");
        assert!(type_id > 0);
        // primary_time_utc should have been parsed from occurred_at_utc.
        assert!(time.is_some());
        assert!(time.unwrap() > 1_700_000_000);
        assert!(place_from_index.is_none());
        assert!(themes.is_none());

        // Step 2: insert a primary place row. Index gets place_id + geohash.
        conn.execute(
            "INSERT INTO ontology_action_places
                (action_id, place_role, place_object_id, place_label,
                 geo_lat, geo_lng, geohash, confidence, created_at)
             VALUES (?1, 'primary', ?2, NULL, 33.43, 126.54, 'wy74bc1', 0.95, 1000)",
            rusqlite::params![action_id, place_id],
        )
        .unwrap();

        let (place_from_index, geohash5, conf_max): (i64, String, f64) = conn
            .query_row(
                "SELECT primary_place_id, primary_geohash_5, confidence_max
                 FROM ontology_action_index WHERE action_id = ?1",
                rusqlite::params![action_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(place_from_index, place_id);
        assert_eq!(geohash5, "wy74b"); // truncated to 5 chars
        assert!((conf_max - 0.95).abs() < 0.01);

        // Step 3: attach themes — index theme_ids should be a sorted csv.
        conn.execute(
            "INSERT INTO ontology_action_themes (action_id, theme_id, weight)
             VALUES (?1, ?2, 0.8), (?1, ?3, 0.5)",
            rusqlite::params![action_id, golf_id, social_id],
        )
        .unwrap();

        let themes: String = conn
            .query_row(
                "SELECT theme_ids FROM ontology_action_index WHERE action_id = ?1",
                rusqlite::params![action_id],
                |r| r.get(0),
            )
            .unwrap();
        // Order is ascending theme_id, joined by comma.
        let expected = {
            let mut ids = vec![golf_id, social_id];
            ids.sort();
            ids.iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
        assert_eq!(themes, expected);

        // Step 4: theme removal → theme_ids rebuilt.
        conn.execute(
            "DELETE FROM ontology_action_themes
             WHERE action_id = ?1 AND theme_id = ?2",
            rusqlite::params![action_id, social_id],
        )
        .unwrap();
        let themes: Option<String> = conn
            .query_row(
                "SELECT theme_ids FROM ontology_action_index WHERE action_id = ?1",
                rusqlite::params![action_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(themes.as_deref(), Some(golf_id.to_string().as_str()));

        // Step 5: ON DELETE CASCADE — drop the action, index row follows.
        conn.execute(
            "DELETE FROM ontology_actions WHERE id = ?1",
            rusqlite::params![action_id],
        )
        .unwrap();
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ontology_action_index WHERE action_id = ?1",
                rusqlite::params![action_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0);
    }

    /// Q1 Commit #6 — SQLite R-Tree bounding-box index gives O(log n)
    /// spatial lookups for "what happened near lat/lng". Populated by
    /// triggers from ontology_action_places.
    #[test]
    fn rtree_spatial_index_finds_points_within_bbox() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        init_ontology_schema(&conn).unwrap();
        seed_default_types(&conn).unwrap();

        // Insert three actions, each with one place at a known coordinate.
        // * Action 1: Jeju ** 골프장  (33.43, 126.54)  ← inside bbox
        // * Action 2: Jeju Airport   (33.51, 126.49)  ← inside bbox
        // * Action 3: Seoul City Hall (37.57, 126.98) ← outside bbox
        let action_type_id: i64 = conn
            .query_row(
                "SELECT id FROM ontology_action_types WHERE name = 'RecordDecision'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        for (_label, _lat, _lng) in [
            ("golf", 33.43_f64, 126.54_f64),
            ("airport", 33.51, 126.49),
            ("seoul", 37.57, 126.98),
        ] {
            conn.execute(
                "INSERT INTO ontology_actions
                    (action_type_id, actor_user_id, params, created_at, updated_at)
                 VALUES (?1, 'user_test', '{}', 1000, 1000)",
                rusqlite::params![action_type_id],
            )
            .unwrap();
        }
        let action_ids: Vec<i64> = conn
            .prepare("SELECT id FROM ontology_actions ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, i64>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        let places: [(i64, &str, f64, f64); 3] = [
            (action_ids[0], "Jeju Golf",    33.43, 126.54),
            (action_ids[1], "Jeju Airport", 33.51, 126.49),
            (action_ids[2], "Seoul Hall",   37.57, 126.98),
        ];
        for (aid, label, lat, lng) in places {
            conn.execute(
                "INSERT INTO ontology_action_places
                    (action_id, place_role, place_label,
                     geo_lat, geo_lng, geohash, confidence, created_at)
                 VALUES (?1, 'primary', ?2, ?3, ?4, NULL, 1.0, 1000)",
                rusqlite::params![aid, label, lat, lng],
            )
            .unwrap();
        }

        // Bounding box covering all of Jeju but not Seoul.
        let jeju_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ontology_action_geo
                 WHERE min_lat >= 33.0 AND max_lat <= 34.0
                   AND min_lng >= 126.0 AND max_lng <= 127.0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(jeju_count, 2, "R-Tree should find the 2 Jeju points");

        // A single-point query (±0.05°) around the golf course.
        let golf_vicinity: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ontology_action_geo
                 WHERE min_lat >= 33.38 AND max_lat <= 33.48
                   AND min_lng >= 126.49 AND max_lng <= 126.59",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(golf_vicinity, 1);

        // Deletion propagates: drop the airport place row, R-Tree shrinks.
        conn.execute(
            "DELETE FROM ontology_action_places
             WHERE place_label = 'Jeju Airport'",
            [],
        )
        .unwrap();
        let jeju_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ontology_action_geo
                 WHERE min_lat >= 33.0 AND max_lat <= 34.0",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(jeju_count, 1, "trigger should clean the R-Tree on DELETE");

        // Inserting a place row with NULL coords must NOT populate R-Tree.
        conn.execute(
            "INSERT INTO ontology_action_places
                (action_id, place_role, place_label,
                 geo_lat, geo_lng, confidence, created_at)
             VALUES (?1, 'primary', 'unknown venue', NULL, NULL, 0.5, 1000)",
            rusqlite::params![action_ids[0]],
        )
        .unwrap();
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM ontology_action_geo", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 2, "NULL-coord place should not enter R-Tree");
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
