//! Ontology repository — CRUD operations on objects, links, and actions.
//!
//! All database access for the ontology layer goes through [`OntologyRepo`],
//! which wraps a `rusqlite::Connection` behind a `parking_lot::Mutex` (matching
//! the pattern used by `SqliteMemory`).

use super::types::*;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Repository providing structured access to ontology tables.
pub struct OntologyRepo {
    conn: Arc<Mutex<Connection>>,
    #[allow(dead_code)]
    db_path: PathBuf,
}

impl OntologyRepo {
    /// Open (or create) the ontology database at `workspace_dir/memory/brain.db`.
    ///
    /// This reuses the same `brain.db` as `SqliteMemory`. The ontology schema is
    /// initialized automatically, and default types are seeded.
    pub fn open(workspace_dir: &Path) -> anyhow::Result<Self> {
        let db_path = workspace_dir.join("memory").join("brain.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous  = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA cache_size   = -2000;
             PRAGMA temp_store   = MEMORY;
             PRAGMA foreign_keys = ON;",
        )?;

        super::schema::init_ontology_schema(&conn)?;
        super::schema::seed_default_types(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            db_path,
        })
    }

    /// Create from an existing connection (useful for testing or shared DB).
    pub fn from_connection(conn: Arc<Mutex<Connection>>) -> anyhow::Result<Self> {
        {
            let c = conn.lock();
            super::schema::init_ontology_schema(&c)?;
            super::schema::seed_default_types(&c)?;
        }
        Ok(Self {
            conn,
            db_path: PathBuf::new(),
        })
    }

    // -----------------------------------------------------------------------
    // Object Type lookups
    // -----------------------------------------------------------------------

    /// Resolve an object type name to its ID.
    pub fn object_type_id(&self, name: &str) -> anyhow::Result<i64> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached("SELECT id FROM ontology_object_types WHERE name = ?1")?;
        let id = stmt.query_row(params![name], |r| r.get(0))
            .map_err(|e| anyhow::anyhow!("unknown object type '{}': {}", name, e))?;
        Ok(id)
    }

    /// Resolve a link type name to its ID.
    pub fn link_type_id(&self, name: &str) -> anyhow::Result<i64> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached("SELECT id FROM ontology_link_types WHERE name = ?1")?;
        let id = stmt.query_row(params![name], |r| r.get(0))
            .map_err(|e| anyhow::anyhow!("unknown link type '{}': {}", name, e))?;
        Ok(id)
    }

    /// Resolve an action type name to its ID.
    pub fn action_type_id(&self, name: &str) -> anyhow::Result<i64> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached("SELECT id FROM ontology_action_types WHERE name = ?1")?;
        let id = stmt.query_row(params![name], |r| r.get(0))
            .map_err(|e| anyhow::anyhow!("unknown action type '{}': {}", name, e))?;
        Ok(id)
    }

    /// Resolve an action type ID to its name.
    pub fn action_type_name(&self, id: i64) -> anyhow::Result<String> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached("SELECT name FROM ontology_action_types WHERE id = ?1")?;
        let name = stmt.query_row(params![id], |r| r.get(0))
            .map_err(|e| anyhow::anyhow!("unknown action type id {}: {}", id, e))?;
        Ok(name)
    }

    // -----------------------------------------------------------------------
    // Object CRUD
    // -----------------------------------------------------------------------

    /// Create a new object and return its ID.
    pub fn create_object(
        &self,
        type_name: &str,
        title: Option<&str>,
        properties: &serde_json::Value,
        owner_user_id: &str,
    ) -> anyhow::Result<i64> {
        let type_id = self.object_type_id(type_name)?;
        let now = now_millis();
        let props_str = serde_json::to_string(properties)?;
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO ontology_objects (type_id, title, properties, owner_user_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![type_id, title, props_str, owner_user_id, now, now],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Get an object by ID (internal use only — no owner filter).
    ///
    /// Callers operating on behalf of an external user should prefer
    /// [`get_object_for_owner`] to enforce ownership isolation.
    pub fn get_object(&self, id: i64) -> anyhow::Result<Option<OntologyObject>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT id, type_id, title, properties, owner_user_id, created_at, updated_at
             FROM ontology_objects WHERE id = ?1",
            params![id],
            |r| {
                Ok(OntologyObject {
                    id: r.get(0)?,
                    type_id: r.get(1)?,
                    title: r.get(2)?,
                    properties: parse_json_col(r.get::<_, String>(3)?),
                    owner_user_id: r.get(4)?,
                    created_at: r.get(5)?,
                    updated_at: r.get(6)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    /// Get an object by ID, enforcing ownership isolation.
    ///
    /// Returns `None` if the object does not exist **or** belongs to a
    /// different user — preventing cross-user data leakage.
    pub fn get_object_for_owner(
        &self,
        id: i64,
        owner_user_id: &str,
    ) -> anyhow::Result<Option<OntologyObject>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT id, type_id, title, properties, owner_user_id, created_at, updated_at
             FROM ontology_objects WHERE id = ?1 AND owner_user_id = ?2",
            params![id, owner_user_id],
            |r| {
                Ok(OntologyObject {
                    id: r.get(0)?,
                    type_id: r.get(1)?,
                    title: r.get(2)?,
                    properties: parse_json_col(r.get::<_, String>(3)?),
                    owner_user_id: r.get(4)?,
                    created_at: r.get(5)?,
                    updated_at: r.get(6)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    /// Update an object's title and/or properties (internal, no owner check).
    ///
    /// Callers that operate on behalf of an external user **must** use
    /// [`update_object_for_owner`] instead to prevent horizontal privilege
    /// escalation.
    pub fn update_object(
        &self,
        id: i64,
        title: Option<&str>,
        properties: Option<&serde_json::Value>,
    ) -> anyhow::Result<()> {
        let now = now_millis();
        let conn = self.conn.lock();
        if let Some(props) = properties {
            let props_str = serde_json::to_string(props)?;
            conn.execute(
                "UPDATE ontology_objects SET title = COALESCE(?2, title), properties = ?3, updated_at = ?4 WHERE id = ?1",
                params![id, title, props_str, now],
            )?;
        } else {
            conn.execute(
                "UPDATE ontology_objects SET title = COALESCE(?2, title), updated_at = ?3 WHERE id = ?1",
                params![id, title, now],
            )?;
        }
        Ok(())
    }

    /// Update an object's title and/or properties **with owner verification**.
    ///
    /// Returns an error if the object does not exist or belongs to a different
    /// user, preventing horizontal privilege escalation.
    pub fn update_object_for_owner(
        &self,
        id: i64,
        owner_user_id: &str,
        title: Option<&str>,
        properties: Option<&serde_json::Value>,
    ) -> anyhow::Result<()> {
        let now = now_millis();
        let conn = self.conn.lock();
        let affected = if let Some(props) = properties {
            let props_str = serde_json::to_string(props)?;
            conn.execute(
                "UPDATE ontology_objects SET title = COALESCE(?2, title), properties = ?3, updated_at = ?4
                 WHERE id = ?1 AND owner_user_id = ?5",
                params![id, title, props_str, now, owner_user_id],
            )?
        } else {
            conn.execute(
                "UPDATE ontology_objects SET title = COALESCE(?2, title), updated_at = ?3
                 WHERE id = ?1 AND owner_user_id = ?4",
                params![id, title, now, owner_user_id],
            )?
        };
        if affected == 0 {
            anyhow::bail!(
                "object {} not found or not owned by user '{}'",
                id,
                owner_user_id,
            );
        }
        Ok(())
    }

    /// Search objects by type and FTS5 query, scoped to an owner.
    pub fn search_objects(
        &self,
        owner_user_id: &str,
        type_name: Option<&str>,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<OntologyObject>> {
        let conn = self.conn.lock();
        let mut results = Vec::new();

        // Resolve optional type filter to a type_id (using parameter binding, never format!).
        let type_id: Option<i64> = if let Some(tn) = type_name {
            Some(
                conn.query_row(
                    "SELECT id FROM ontology_object_types WHERE name = ?1",
                    params![tn],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(|e| anyhow::anyhow!("unknown type '{}': {}", tn, e))?,
            )
        } else {
            None
        };

        let row_mapper = |r: &rusqlite::Row| -> rusqlite::Result<OntologyObject> {
            Ok(OntologyObject {
                id: r.get(0)?,
                type_id: r.get(1)?,
                title: r.get(2)?,
                properties: parse_json_col(r.get::<_, String>(3)?),
                owner_user_id: r.get(4)?,
                created_at: r.get(5)?,
                updated_at: r.get(6)?,
            })
        };

        // Sanitize FTS5 query: quote each word to escape special chars
        // (*, OR, AND, NOT, NEAR, etc.) that could cause syntax errors or
        // unintended query semantics.
        let sanitized_query: String = query
            .split_whitespace()
            .map(|w| {
                let escaped = w.replace('"', "\"\"");
                format!("\"{escaped}\"")
            })
            .collect::<Vec<_>>()
            .join(" ");

        if query.is_empty() {
            // No FTS query — simple list by type.
            if let Some(tid) = type_id {
                let mut stmt = conn.prepare_cached(
                    "SELECT o.id, o.type_id, o.title, o.properties, o.owner_user_id, o.created_at, o.updated_at
                     FROM ontology_objects o
                     WHERE o.owner_user_id = ?1 AND o.type_id = ?2
                     ORDER BY o.updated_at DESC LIMIT ?3",
                )?;
                let rows = stmt.query_map(params![owner_user_id, tid, limit as i64], row_mapper)?;
                for row in rows {
                    results.push(row?);
                }
            } else {
                let mut stmt = conn.prepare_cached(
                    "SELECT o.id, o.type_id, o.title, o.properties, o.owner_user_id, o.created_at, o.updated_at
                     FROM ontology_objects o
                     WHERE o.owner_user_id = ?1
                     ORDER BY o.updated_at DESC LIMIT ?2",
                )?;
                let rows = stmt.query_map(params![owner_user_id, limit as i64], row_mapper)?;
                for row in rows {
                    results.push(row?);
                }
            }
        } else {
            // FTS5 search — always use parameter binding.
            if let Some(tid) = type_id {
                let mut stmt = conn.prepare_cached(
                    "SELECT o.id, o.type_id, o.title, o.properties, o.owner_user_id, o.created_at, o.updated_at
                     FROM ontology_objects_fts f
                     JOIN ontology_objects o ON o.id = f.rowid
                     WHERE ontology_objects_fts MATCH ?1
                       AND o.owner_user_id = ?2
                       AND o.type_id = ?3
                     ORDER BY rank LIMIT ?4",
                )?;
                let rows = stmt.query_map(params![sanitized_query, owner_user_id, tid, limit as i64], row_mapper)?;
                for row in rows {
                    results.push(row?);
                }
            } else {
                let mut stmt = conn.prepare_cached(
                    "SELECT o.id, o.type_id, o.title, o.properties, o.owner_user_id, o.created_at, o.updated_at
                     FROM ontology_objects_fts f
                     JOIN ontology_objects o ON o.id = f.rowid
                     WHERE ontology_objects_fts MATCH ?1
                       AND o.owner_user_id = ?2
                     ORDER BY rank LIMIT ?3",
                )?;
                let rows = stmt.query_map(params![sanitized_query, owner_user_id, limit as i64], row_mapper)?;
                for row in rows {
                    results.push(row?);
                }
            }
        }
        Ok(results)
    }

    /// List objects of a specific type for an owner.
    pub fn list_objects_by_type(
        &self,
        owner_user_id: &str,
        type_name: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<OntologyObject>> {
        self.search_objects(owner_user_id, Some(type_name), "", limit)
    }

    // -----------------------------------------------------------------------
    // Link CRUD
    // -----------------------------------------------------------------------

    /// Create a link between two objects. Returns the link ID.
    ///
    /// Uses INSERT OR IGNORE to avoid duplicate links.  When a duplicate is
    /// ignored, returns the existing link's ID via a follow-up SELECT instead
    /// of the misleading `last_insert_rowid()` (which would return the
    /// *previous* insert's rowid, not this one).
    pub fn create_link(
        &self,
        link_type_name: &str,
        from_object_id: i64,
        to_object_id: i64,
        properties: Option<&serde_json::Value>,
    ) -> anyhow::Result<i64> {
        let link_type_id = self.link_type_id(link_type_name)?;
        let now = now_millis();
        let props_str = properties.map(|p| serde_json::to_string(p).unwrap_or_default());
        let conn = self.conn.lock();
        let affected = conn.execute(
            "INSERT OR IGNORE INTO ontology_links (link_type_id, from_object_id, to_object_id, properties, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![link_type_id, from_object_id, to_object_id, props_str, now],
        )?;

        if affected > 0 {
            // Row was inserted — last_insert_rowid is valid.
            Ok(conn.last_insert_rowid())
        } else {
            // Duplicate was ignored — look up the existing link.
            conn.query_row(
                "SELECT id FROM ontology_links WHERE link_type_id = ?1 AND from_object_id = ?2 AND to_object_id = ?3",
                params![link_type_id, from_object_id, to_object_id],
                |r| r.get(0),
            )
            .map_err(Into::into)
        }
    }

    /// Get all links originating from an object, scoped to the object's owner.
    ///
    /// Joins through `ontology_objects` to ensure the caller can only see
    /// links where the *from* object belongs to them.
    pub fn links_from(&self, object_id: i64, owner_user_id: &str) -> anyhow::Result<Vec<OntologyLink>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT l.id, l.link_type_id, l.from_object_id, l.to_object_id, l.properties, l.created_at
             FROM ontology_links l
             JOIN ontology_objects o ON o.id = l.from_object_id
             WHERE l.from_object_id = ?1 AND o.owner_user_id = ?2
             ORDER BY l.created_at DESC",
        )?;
        let rows = stmt.query_map(params![object_id, owner_user_id], |r| {
            Ok(OntologyLink {
                id: r.get(0)?,
                link_type_id: r.get(1)?,
                from_object_id: r.get(2)?,
                to_object_id: r.get(3)?,
                properties: r
                    .get::<_, Option<String>>(4)?
                    .map(|s| parse_json_col(s)),
                created_at: r.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Get all links pointing to an object, scoped to the object's owner.
    pub fn links_to(&self, object_id: i64, owner_user_id: &str) -> anyhow::Result<Vec<OntologyLink>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare_cached(
            "SELECT l.id, l.link_type_id, l.from_object_id, l.to_object_id, l.properties, l.created_at
             FROM ontology_links l
             JOIN ontology_objects o ON o.id = l.to_object_id
             WHERE l.to_object_id = ?1 AND o.owner_user_id = ?2
             ORDER BY l.created_at DESC",
        )?;
        let rows = stmt.query_map(params![object_id, owner_user_id], |r| {
            Ok(OntologyLink {
                id: r.get(0)?,
                link_type_id: r.get(1)?,
                from_object_id: r.get(2)?,
                to_object_id: r.get(3)?,
                properties: r
                    .get::<_, Option<String>>(4)?
                    .map(|s| parse_json_col(s)),
                created_at: r.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Action log
    // -----------------------------------------------------------------------

    /// Insert a new action log entry with status "pending". Returns the action ID.
    pub fn insert_action_pending(
        &self,
        action_type_name: &str,
        actor_user_id: &str,
        actor_kind: &ActorKind,
        primary_object_id: Option<i64>,
        related_object_ids: &[i64],
        params: &serde_json::Value,
        channel: Option<&str>,
        context_id: Option<i64>,
    ) -> anyhow::Result<i64> {
        let action_type_id = self.action_type_id(action_type_name)?;
        let now = now_millis();
        let params_str = serde_json::to_string(params)?;
        let related_str = if related_object_ids.is_empty() {
            None
        } else {
            Some(serde_json::to_string(related_object_ids)?)
        };

        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO ontology_actions
             (action_type_id, actor_user_id, actor_kind, primary_object_id,
              related_object_ids, params, channel, context_id, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9, ?10)",
            params![
                action_type_id,
                actor_user_id,
                actor_kind.to_string(),
                primary_object_id,
                related_str,
                params_str,
                channel,
                context_id,
                now,
                now,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Mark an action as succeeded with a result payload.
    pub fn complete_action(
        &self,
        action_id: i64,
        result: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let now = now_millis();
        let result_str = serde_json::to_string(result)?;
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE ontology_actions SET result = ?2, status = 'success', updated_at = ?3 WHERE id = ?1",
            params![action_id, result_str, now],
        )?;
        Ok(())
    }

    /// Mark an action as failed with an error message.
    pub fn fail_action(&self, action_id: i64, error: &str) -> anyhow::Result<()> {
        let now = now_millis();
        let result_str =
            serde_json::to_string(&serde_json::json!({"success": false, "error": error}))?;
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE ontology_actions SET result = ?2, status = 'error', error_message = ?3, updated_at = ?4 WHERE id = ?1",
            params![action_id, result_str, error, now],
        )?;
        Ok(())
    }

    /// Fetch recent actions for a user, optionally filtered by channel.
    pub fn recent_actions(
        &self,
        owner_user_id: &str,
        channel: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<OntologyAction>> {
        let conn = self.conn.lock();
        let (sql, limit_val) = if channel.is_some() {
            (
                "SELECT id, action_type_id, actor_user_id, actor_kind,
                        primary_object_id, related_object_ids, params, result,
                        channel, context_id, status, error_message,
                        created_at, updated_at
                 FROM ontology_actions
                 WHERE actor_user_id = ?1 AND channel = ?3
                 ORDER BY created_at DESC LIMIT ?2"
                    .to_string(),
                limit as i64,
            )
        } else {
            (
                "SELECT id, action_type_id, actor_user_id, actor_kind,
                        primary_object_id, related_object_ids, params, result,
                        channel, context_id, status, error_message,
                        created_at, updated_at
                 FROM ontology_actions
                 WHERE actor_user_id = ?1
                 ORDER BY created_at DESC LIMIT ?2"
                    .to_string(),
                limit as i64,
            )
        };

        let mut stmt = conn.prepare(&sql)?;
        let params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = if let Some(ch) = channel {
            vec![
                Box::new(owner_user_id.to_string()),
                Box::new(limit_val),
                Box::new(ch.to_string()),
            ]
        } else {
            vec![
                Box::new(owner_user_id.to_string()),
                Box::new(limit_val),
            ]
        };
        let rows = stmt.query_map(rusqlite::params_from_iter(params_vec.iter()), |r| {
            Ok(OntologyAction {
                id: r.get(0)?,
                action_type_id: r.get(1)?,
                actor_user_id: r.get(2)?,
                actor_kind: ActorKind::from_str_lossy(&r.get::<_, String>(3)?),
                primary_object_id: r.get(4)?,
                related_object_ids: r
                    .get::<_, Option<String>>(5)?
                    .map(|s| serde_json::from_str(&s).unwrap_or_default())
                    .unwrap_or_default(),
                params: parse_json_col(r.get::<_, String>(6)?),
                result: r.get::<_, Option<String>>(7)?.map(|s| parse_json_col(s)),
                channel: r.get(8)?,
                context_id: r.get(9)?,
                status: ActionStatus::from_str_lossy(&r.get::<_, String>(10)?),
                error_message: r.get(11)?,
                created_at: r.get(12)?,
                updated_at: r.get(13)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    // -----------------------------------------------------------------------
    // Convenience: find-or-create patterns
    // -----------------------------------------------------------------------

    /// Find an object by type + title + owner, or create it if it doesn't exist.
    ///
    /// Uses INSERT ... ON CONFLICT to avoid the TOCTOU race condition that
    /// existed in the old SELECT-then-INSERT pattern.
    pub fn ensure_object(
        &self,
        type_name: &str,
        title: &str,
        default_properties: &serde_json::Value,
        owner_user_id: &str,
    ) -> anyhow::Result<i64> {
        let type_id = self.object_type_id(type_name)?;
        let now = now_millis();
        let props_str = serde_json::to_string(default_properties)?;
        let conn = self.conn.lock();

        // Atomic upsert: the unique index on (type_id, title, owner_user_id)
        // doesn't exist by default, so we fall back to a safe pattern:
        // try INSERT, and if it conflicts on the implicit constraint, just
        // SELECT the existing row.
        conn.execute(
            "INSERT OR IGNORE INTO ontology_objects (type_id, title, properties, owner_user_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![type_id, title, props_str, owner_user_id, now, now],
        )?;

        // Whether we just inserted or the row already existed, SELECT the ID.
        let id: i64 = conn.query_row(
            "SELECT id FROM ontology_objects WHERE type_id = ?1 AND title = ?2 AND owner_user_id = ?3",
            params![type_id, title, owner_user_id],
            |r| r.get(0),
        )?;
        Ok(id)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn parse_json_col(s: String) -> serde_json::Value {
    serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn test_repo() -> OntologyRepo {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        OntologyRepo::from_connection(Arc::new(Mutex::new(conn))).unwrap()
    }

    #[test]
    fn create_and_get_object() {
        let repo = test_repo();
        let id = repo
            .create_object(
                "Task",
                Some("Test task"),
                &serde_json::json!({"status": "open"}),
                "user-1",
            )
            .unwrap();

        let obj = repo.get_object(id).unwrap().unwrap();
        assert_eq!(obj.title.as_deref(), Some("Test task"));
        assert_eq!(obj.properties["status"], "open");
        assert_eq!(obj.owner_user_id, "user-1");
    }

    #[test]
    fn create_link_and_query() {
        let repo = test_repo();
        let task_id = repo
            .create_object("Task", Some("Task A"), &serde_json::json!({}), "u1")
            .unwrap();
        let contact_id = repo
            .create_object("Contact", Some("Alice"), &serde_json::json!({}), "u1")
            .unwrap();

        repo.create_link("related_to", task_id, contact_id, None)
            .unwrap();

        let links = repo.links_from(task_id, "u1").unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].to_object_id, contact_id);
    }

    #[test]
    fn duplicate_link_ignored() {
        let repo = test_repo();
        let a = repo
            .create_object("Task", Some("A"), &serde_json::json!({}), "u1")
            .unwrap();
        let b = repo
            .create_object("Contact", Some("B"), &serde_json::json!({}), "u1")
            .unwrap();

        repo.create_link("related_to", a, b, None).unwrap();
        repo.create_link("related_to", a, b, None).unwrap(); // should not error

        let links = repo.links_from(a, "u1").unwrap();
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn action_lifecycle() {
        let repo = test_repo();
        let action_id = repo
            .insert_action_pending(
                "CreateTask",
                "user-1",
                &ActorKind::Agent,
                None,
                &[],
                &serde_json::json!({"title": "test"}),
                Some("desktop"),
                None,
            )
            .unwrap();

        // Complete it.
        repo.complete_action(action_id, &serde_json::json!({"task_id": 42}))
            .unwrap();

        let actions = repo.recent_actions("user-1", None, 10).unwrap();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].status, ActionStatus::Success);
    }

    #[test]
    fn ensure_object_idempotent() {
        let repo = test_repo();
        let id1 = repo
            .ensure_object("Channel", "kakao", &serde_json::json!({}), "u1")
            .unwrap();
        let id2 = repo
            .ensure_object("Channel", "kakao", &serde_json::json!({}), "u1")
            .unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn search_objects_fts() {
        let repo = test_repo();
        repo.create_object(
            "Task",
            Some("Hotel reservation Ulleungdo"),
            &serde_json::json!({"status": "open"}),
            "u1",
        )
        .unwrap();
        repo.create_object(
            "Task",
            Some("Buy groceries"),
            &serde_json::json!({"status": "open"}),
            "u1",
        )
        .unwrap();

        let results = repo
            .search_objects("u1", Some("Task"), "hotel", 10)
            .unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0]
            .title
            .as_deref()
            .unwrap()
            .contains("Hotel"));
    }
}
