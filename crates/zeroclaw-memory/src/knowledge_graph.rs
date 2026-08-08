//! Knowledge graph for capturing, organizing, and reusing expertise.
//!
//! Every read and write goes through a [`KnowledgeScope`]: rows are stamped
//! with the writing agent's alias and reads are filtered to rows the scope
//! may see. Rows with no owner (`owner_agent IS NULL`) predate attribution
//! and remain hidden until startup can assign them to one explicit owner.

use anyhow::Context;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{Connection, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use uuid::Uuid;

// ── Domain types ────────────────────────────────────────────────

macro_rules! knowledge_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $($variant:ident => $value:literal),+ $(,)?
        }
        error = $error_label:literal
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
            pub const SCHEMA_VALUES: &'static [&'static str] = &[$($value),+];

            pub fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }

            pub fn schema_values() -> &'static [&'static str] {
                Self::SCHEMA_VALUES
            }

            pub fn parse(s: &str) -> anyhow::Result<Self> {
                match s {
                    $($value => Ok(Self::$variant),)+
                    other => anyhow::bail!(
                        "unknown {}: {other}",
                        $error_label
                    ),
                }
            }
        }
    };
}

knowledge_enum! {
    /// The kind of knowledge captured in a node.
    pub enum NodeType {
        Pattern => "pattern",
        Decision => "decision",
        Lesson => "lesson",
        Expert => "expert",
        Technology => "technology",
        Client => "client",
        Contact => "contact",
        Interaction => "interaction",
    }
    error = "node type"
}

knowledge_enum! {
    /// Directed relationship between two knowledge nodes.
    pub enum Relation {
        Uses => "uses",
        Replaces => "replaces",
        Extends => "extends",
        AuthoredBy => "authored_by",
        AppliesTo => "applies_to",
        ManagesClient => "manages_client",
        ContactOf => "contact_of",
        InteractedWith => "interacted_with",
    }
    error = "relation"
}

/// A node in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeNode {
    pub id: String,
    pub node_type: NodeType,
    pub title: String,
    pub content: String,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub source_project: Option<String>,
    /// Alias of the agent that captured this node. `None` marks rows
    /// created before attribution existed (or by the unrestricted
    /// maintenance scope); agent-scoped reads never expose those rows.
    pub owner_agent: Option<String>,
}

/// A directed edge in the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEdge {
    pub from_id: String,
    pub to_id: String,
    pub relation: Relation,
}

/// A search result with relevance score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub node: KnowledgeNode,
    pub score: f64,
}

/// Summary statistics for the knowledge graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphStats {
    pub total_nodes: usize,
    pub total_edges: usize,
    pub nodes_by_type: HashMap<String, usize>,
    pub top_tags: Vec<(String, usize)>,
}

// ── Scoping ─────────────────────────────────────────────────────

/// Trusted caller identity for knowledge graph operations.
///
/// The scope is bound at construction time by the runtime (from the
/// configured agent alias), never taken from tool arguments. Reads are
/// filtered to rows the scope may see; writes are stamped with the
/// scope's identity.
#[derive(Debug, Clone)]
pub enum KnowledgeScope {
    /// Operations run as one configured agent. Reads see rows owned by
    /// that agent and rows owned by any alias in the read allowlist.
    /// Writes are stamped with the alias.
    Agent {
        alias: String,
        /// Aliases whose rows this scope may additionally read
        /// (`workspace.read_knowledge_from`). Read-only widening: it
        /// never grants writes on the sibling's behalf.
        read_from: Vec<String>,
    },
    /// Maintenance scope: reads see every row; writes carry no owner.
    /// Not wired to any agent-facing tool; intended for migrations and
    /// tests. Unowned rows are fail-closed to every agent scope.
    Unrestricted,
}

impl KnowledgeScope {
    pub fn for_agent(
        alias: impl Into<String>,
        read_from: impl IntoIterator<Item = String>,
    ) -> Self {
        Self::Agent {
            alias: alias.into(),
            read_from: read_from.into_iter().collect(),
        }
    }

    pub fn unrestricted() -> Self {
        Self::Unrestricted
    }

    /// Owner stamped on writes made through this scope.
    fn write_owner(&self) -> Option<&str> {
        match self {
            Self::Agent { alias, .. } => Some(alias.as_str()),
            Self::Unrestricted => None,
        }
    }

    /// Owner aliases this scope may read, in deterministic order and
    /// without duplicates or empty entries. `None` means unrestricted.
    fn visible_owners(&self) -> Option<Vec<&str>> {
        match self {
            Self::Unrestricted => None,
            Self::Agent { alias, read_from } => {
                let mut owners: Vec<&str> = Vec::with_capacity(1 + read_from.len());
                for candidate in
                    std::iter::once(alias.as_str()).chain(read_from.iter().map(String::as_str))
                {
                    if !candidate.is_empty() && !owners.contains(&candidate) {
                        owners.push(candidate);
                    }
                }
                Some(owners)
            }
        }
    }

    /// SQL predicate limiting `column` (an `owner_agent` column) to rows
    /// this scope may see, with positional parameters starting at
    /// `first_param`. Returns the predicate text and the parameter
    /// values to bind at those positions. Callers that apply the
    /// predicate to several columns in one statement reuse the same
    /// `first_param` so the values are bound once.
    fn visibility_sql(&self, column: &str, first_param: usize) -> (String, Vec<&str>) {
        match self.visible_owners() {
            None => ("1=1".to_string(), Vec::new()),
            Some(owners) if owners.is_empty() => ("0=1".to_string(), owners),
            Some(owners) => {
                let placeholders: Vec<String> = (0..owners.len())
                    .map(|i| format!("?{}", first_param + i))
                    .collect();
                (format!("{column} IN ({})", placeholders.join(", ")), owners)
            }
        }
    }
}

// ── Knowledge graph ─────────────────────────────────────────────

/// SQLite-backed knowledge graph.
pub struct KnowledgeGraph {
    conn: Mutex<Connection>,
    max_nodes: usize,
}

impl KnowledgeGraph {
    /// Open (or create) a knowledge graph database at the given path.
    ///
    /// Databases created before owner attribution existed are migrated
    /// in place: nodes gain a nullable `owner_agent` column and the
    /// edges table is rebuilt with `owner_agent` in its primary key.
    /// Pre-existing rows keep `NULL` owners until
    /// [`Self::prepare_legacy_ownership`] assigns them.
    pub fn new(db_path: &Path, max_nodes: usize) -> anyhow::Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut conn =
            Connection::open(db_path).context("failed to open knowledge graph database")?;

        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous  = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS nodes (
                id TEXT PRIMARY KEY,
                node_type TEXT NOT NULL,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                source_project TEXT,
                owner_agent TEXT
            );

            CREATE TABLE IF NOT EXISTS edges (
                from_id TEXT NOT NULL,
                to_id TEXT NOT NULL,
                relation TEXT NOT NULL,
                owner_agent TEXT,
                PRIMARY KEY (from_id, to_id, relation, owner_agent),
                FOREIGN KEY (from_id) REFERENCES nodes(id) ON DELETE CASCADE,
                FOREIGN KEY (to_id) REFERENCES nodes(id) ON DELETE CASCADE
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS nodes_fts USING fts5(
                title, content, tags, content='nodes', content_rowid='rowid'
            );

            CREATE TRIGGER IF NOT EXISTS nodes_ai AFTER INSERT ON nodes BEGIN
                INSERT INTO nodes_fts(rowid, title, content, tags)
                VALUES (new.rowid, new.title, new.content, new.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS nodes_ad AFTER DELETE ON nodes BEGIN
                INSERT INTO nodes_fts(nodes_fts, rowid, title, content, tags)
                VALUES ('delete', old.rowid, old.title, old.content, old.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS nodes_au AFTER UPDATE ON nodes BEGIN
                INSERT INTO nodes_fts(nodes_fts, rowid, title, content, tags)
                VALUES ('delete', old.rowid, old.title, old.content, old.tags);
                INSERT INTO nodes_fts(rowid, title, content, tags)
                VALUES (new.rowid, new.title, new.content, new.tags);
            END;",
        )?;

        Self::migrate_owner_attribution(&mut conn)?;

        // Indexes are created after the migration so the owner columns
        // exist on legacy databases, and so the edges indexes dropped by
        // the edges-table rebuild are recreated.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_nodes_type ON nodes(node_type);
             CREATE INDEX IF NOT EXISTS idx_nodes_source ON nodes(source_project);
             CREATE INDEX IF NOT EXISTS idx_nodes_owner ON nodes(owner_agent);
             CREATE INDEX IF NOT EXISTS idx_edges_from ON edges(from_id);
             CREATE INDEX IF NOT EXISTS idx_edges_to ON edges(to_id);
             CREATE INDEX IF NOT EXISTS idx_edges_owner ON edges(owner_agent);",
        )?;

        // SQLite permits duplicate NULL values in a composite primary key.
        // Old binaries omit `owner_agent`, so preserve their historical
        // `INSERT OR IGNORE` idempotency across downgrade by enforcing the
        // legacy three-column identity when the owner is NULL.
        conn.execute_batch(
            "DELETE FROM edges
             WHERE owner_agent IS NULL
               AND rowid NOT IN (
                   SELECT MIN(rowid) FROM edges
                   WHERE owner_agent IS NULL
                   GROUP BY from_id, to_id, relation
               );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_edges_owner_identity
                 ON edges(from_id, to_id, relation, COALESCE(owner_agent, ''));",
        )?;

        Ok(Self {
            conn: Mutex::new(conn),
            max_nodes,
        })
    }

    /// Resolve unattributed pre-upgrade rows before exposing an agent tool.
    ///
    /// `owner` is derived by the runtime from the canonical config: either the
    /// sole enabled agent or the operator's explicit legacy-owner mapping. If
    /// legacy rows exist and no owner can be established, initialization fails
    /// closed so a multi-agent install cannot observe ambiguous historical
    /// knowledge.
    pub fn prepare_legacy_ownership(&self, owner: Option<&str>) -> anyhow::Result<usize> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin legacy knowledge ownership assignment")?;
        let unowned_nodes: usize = tx.query_row(
            "SELECT COUNT(*) FROM nodes WHERE owner_agent IS NULL",
            [],
            |row| row.get(0),
        )?;
        let unowned_edges: usize = tx.query_row(
            "SELECT COUNT(*) FROM edges WHERE owner_agent IS NULL",
            [],
            |row| row.get(0),
        )?;
        let unowned = unowned_nodes + unowned_edges;
        if unowned == 0 {
            tx.commit()?;
            return Ok(0);
        }

        let owner = owner
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                anyhow::Error::msg(format!(
                    "knowledge graph contains {unowned} unattributed legacy row(s); set knowledge.legacy_owner_agent to an enabled agent before enabling the tool in a multi-agent install"
                ))
            })?;

        // A partially attributed database can already contain the target
        // owner's copy of an edge. Keep that attributed row and remove the
        // unowned duplicate before assigning the remaining legacy edges.
        tx.execute(
            "DELETE FROM edges AS legacy
             WHERE legacy.owner_agent IS NULL
               AND EXISTS (
                   SELECT 1 FROM edges AS owned
                   WHERE owned.from_id = legacy.from_id
                     AND owned.to_id = legacy.to_id
                     AND owned.relation = legacy.relation
                     AND owned.owner_agent = ?1
               )",
            params![owner],
        )?;
        let nodes = tx.execute(
            "UPDATE nodes SET owner_agent = ?1 WHERE owner_agent IS NULL",
            params![owner],
        )?;
        let edges = tx.execute(
            "UPDATE edges SET owner_agent = ?1 WHERE owner_agent IS NULL",
            params![owner],
        )?;
        tx.commit()
            .context("failed to commit legacy knowledge ownership assignment")?;
        Ok(nodes + edges)
    }

    /// Count graph rows durably owned by one agent alias.
    pub fn count_owner(&self, alias: &str) -> anyhow::Result<usize> {
        let conn = self.conn.lock();
        let nodes: usize = conn.query_row(
            "SELECT COUNT(*) FROM nodes WHERE owner_agent = ?1",
            params![alias],
            |row| row.get(0),
        )?;
        let edges: usize = conn.query_row(
            "SELECT COUNT(*) FROM edges WHERE owner_agent = ?1",
            params![alias],
            |row| row.get(0),
        )?;
        Ok(nodes + edges)
    }

    /// Re-point all graph ownership during the canonical agent rename cascade.
    pub fn rename_owner(&self, from: &str, to: &str) -> anyhow::Result<usize> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin knowledge ownership rename")?;
        tx.execute(
            "DELETE FROM edges AS old
             WHERE old.owner_agent = ?1
               AND EXISTS (
                   SELECT 1 FROM edges AS new
                   WHERE new.from_id = old.from_id
                     AND new.to_id = old.to_id
                     AND new.relation = old.relation
                     AND new.owner_agent = ?2
               )",
            params![from, to],
        )?;
        let nodes = tx.execute(
            "UPDATE nodes SET owner_agent = ?2 WHERE owner_agent = ?1",
            params![from, to],
        )?;
        let edges = tx.execute(
            "UPDATE edges SET owner_agent = ?2 WHERE owner_agent = ?1",
            params![from, to],
        )?;
        tx.commit()
            .context("failed to commit knowledge ownership rename")?;
        Ok(nodes + edges)
    }

    /// Export the rows owned by an agent for the deletion archive.
    pub fn export_owner(&self, alias: &str) -> anyhow::Result<serde_json::Value> {
        let conn = self.conn.lock();
        let mut node_stmt = conn.prepare(
            "SELECT id, node_type, title, content, tags, created_at, updated_at, source_project, owner_agent
             FROM nodes WHERE owner_agent = ?1 ORDER BY id",
        )?;
        let mut node_rows = node_stmt.query(params![alias])?;
        let mut nodes = Vec::new();
        while let Some(row) = node_rows.next()? {
            nodes.push(row_to_node(row)?);
        }
        let mut edge_stmt = conn.prepare(
            "SELECT from_id, to_id, relation FROM edges
             WHERE owner_agent = ?1 ORDER BY from_id, to_id, relation",
        )?;
        let edges = edge_stmt
            .query_map(params![alias], |row| {
                let relation: String = row.get(2)?;
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, relation))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(serde_json::json!({ "nodes": nodes, "edges": edges }))
    }

    /// Remove graph rows owned by an agent during the canonical delete cascade.
    pub fn purge_owner(&self, alias: &str) -> anyhow::Result<usize> {
        let mut conn = self.conn.lock();
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin knowledge ownership purge")?;
        let owned_edges = tx.execute("DELETE FROM edges WHERE owner_agent = ?1", params![alias])?;
        let owned_nodes = tx.execute("DELETE FROM nodes WHERE owner_agent = ?1", params![alias])?;
        tx.commit()
            .context("failed to commit knowledge ownership purge")?;
        Ok(owned_nodes + owned_edges)
    }

    /// Bring a pre-attribution database up to the owned schema. Runs in
    /// one immediate transaction and re-checks under the write lock so
    /// concurrent per-agent connections cannot double-apply it.
    fn migrate_owner_attribution(conn: &mut Connection) -> anyhow::Result<()> {
        if Self::table_has_column(conn, "nodes", "owner_agent")?
            && Self::table_has_column(conn, "edges", "owner_agent")?
        {
            return Ok(());
        }

        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to begin knowledge graph attribution migration")?;

        if !Self::table_has_column(&tx, "nodes", "owner_agent")? {
            tx.execute_batch("ALTER TABLE nodes ADD COLUMN owner_agent TEXT")?;
        }

        if !Self::table_has_column(&tx, "edges", "owner_agent")? {
            // The primary key changes, so the table is rebuilt. Existing
            // Edges keep NULL owners until startup assigns the legacy graph.
            tx.execute_batch(
                "CREATE TABLE edges_owner_migration (
                    from_id TEXT NOT NULL,
                    to_id TEXT NOT NULL,
                    relation TEXT NOT NULL,
                    owner_agent TEXT,
                    PRIMARY KEY (from_id, to_id, relation, owner_agent),
                    FOREIGN KEY (from_id) REFERENCES nodes(id) ON DELETE CASCADE,
                    FOREIGN KEY (to_id) REFERENCES nodes(id) ON DELETE CASCADE
                );
                INSERT INTO edges_owner_migration (from_id, to_id, relation, owner_agent)
                    SELECT from_id, to_id, relation, NULL FROM edges;
                DROP TABLE edges;
                ALTER TABLE edges_owner_migration RENAME TO edges;",
            )?;
        }

        tx.commit()
            .context("failed to commit knowledge graph attribution migration")?;
        Ok(())
    }

    fn table_has_column(
        conn: &Connection,
        table: &'static str,
        column: &str,
    ) -> anyhow::Result<bool> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let name: String = row.get(1)?;
            if name == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Add a node to the graph. Returns the generated node id.
    pub fn add_node(
        &self,
        scope: &KnowledgeScope,
        node_type: NodeType,
        title: &str,
        content: &str,
        tags: &[String],
        source_project: Option<&str>,
    ) -> anyhow::Result<String> {
        let conn = self.conn.lock();

        // The cap is a global disk budget shared by every agent on the
        // one store, so it deliberately counts all rows, not just the
        // scope-visible ones.
        let count: usize = conn.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0))?;
        if count >= self.max_nodes {
            anyhow::bail!(
                "knowledge graph node limit reached ({}/{})",
                count,
                self.max_nodes
            );
        }

        // Reject tags containing commas since comma is the separator in storage.
        for tag in tags {
            if tag.contains(',') {
                anyhow::bail!(
                    "tag '{}' contains a comma, which is used as the tag separator",
                    tag
                );
            }
        }

        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let tags_str = tags.join(",");

        conn.execute(
            "INSERT INTO nodes (id, node_type, title, content, tags, created_at, updated_at, source_project, owner_agent)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                id,
                node_type.as_str(),
                title,
                content,
                tags_str,
                now,
                now,
                source_project,
                scope.write_owner(),
            ],
        )?;

        Ok(id)
    }

    /// Add a directed edge between two nodes the scope can see. The edge
    /// is stamped with the scope's identity, so it is visible only to
    /// scopes that may read the writer's rows. Adding an edge identical
    /// to one the scope already sees is a no-op.
    pub fn add_edge(
        &self,
        scope: &KnowledgeScope,
        from_id: &str,
        to_id: &str,
        relation: Relation,
    ) -> anyhow::Result<()> {
        let conn = self.conn.lock();

        // Both endpoints must exist and be visible to the caller. An
        // invisible node is reported exactly like a missing one so the
        // error is not an existence oracle for foreign rows.
        let (vis_node, vis_params) = scope.visibility_sql("owner_agent", 2);
        let visible = |id: &str| -> anyhow::Result<bool> {
            let sql = format!("SELECT COUNT(*) FROM nodes WHERE id = ?1 AND {vis_node}");
            let mut sql_params: Vec<&dyn rusqlite::ToSql> = vec![&id];
            sql_params.extend(vis_params.iter().map(|owner| owner as &dyn rusqlite::ToSql));
            let c: usize = conn.query_row(&sql, &sql_params[..], |r| r.get(0))?;
            Ok(c > 0)
        };

        if !visible(from_id)? {
            anyhow::bail!("source node not found: {from_id}");
        }
        if !visible(to_id)? {
            anyhow::bail!("target node not found: {to_id}");
        }

        // Idempotence within the scope: if an identical edge is already
        // visible (own or shared-in), adding it again is a
        // no-op rather than a per-owner duplicate.
        let (vis_edge, edge_params) = scope.visibility_sql("owner_agent", 4);
        let dup_sql = format!(
            "SELECT COUNT(*) FROM edges
             WHERE from_id = ?1 AND to_id = ?2 AND relation = ?3 AND {vis_edge}"
        );
        let relation_str = relation.as_str();
        let mut dup_params: Vec<&dyn rusqlite::ToSql> = vec![&from_id, &to_id, &relation_str];
        dup_params.extend(
            edge_params
                .iter()
                .map(|owner| owner as &dyn rusqlite::ToSql),
        );
        let existing: usize = conn.query_row(&dup_sql, &dup_params[..], |r| r.get(0))?;
        if existing > 0 {
            return Ok(());
        }

        conn.execute(
            "INSERT OR IGNORE INTO edges (from_id, to_id, relation, owner_agent) VALUES (?1, ?2, ?3, ?4)",
            params![from_id, to_id, relation.as_str(), scope.write_owner()],
        )?;

        Ok(())
    }

    /// Retrieve a node by id. Nodes outside the scope read as absent.
    pub fn get_node(
        &self,
        scope: &KnowledgeScope,
        id: &str,
    ) -> anyhow::Result<Option<KnowledgeNode>> {
        let conn = self.conn.lock();
        let (vis, vis_params) = scope.visibility_sql("owner_agent", 2);
        let sql = format!(
            "SELECT id, node_type, title, content, tags, created_at, updated_at, source_project, owner_agent
             FROM nodes WHERE id = ?1 AND {vis}"
        );
        let mut stmt = conn.prepare(&sql)?;

        let mut sql_params: Vec<&dyn rusqlite::ToSql> = vec![&id];
        sql_params.extend(vis_params.iter().map(|owner| owner as &dyn rusqlite::ToSql));
        let mut rows = stmt.query(&sql_params[..])?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_node(row)?)),
            None => Ok(None),
        }
    }

    /// Query nodes by tags (all listed tags must be present).
    pub fn query_by_tags(
        &self,
        scope: &KnowledgeScope,
        tags: &[String],
    ) -> anyhow::Result<Vec<KnowledgeNode>> {
        let conn = self.conn.lock();
        let (vis, vis_params) = scope.visibility_sql("owner_agent", 1);
        let sql = format!(
            "SELECT id, node_type, title, content, tags, created_at, updated_at, source_project, owner_agent
             FROM nodes WHERE {vis} ORDER BY updated_at DESC"
        );
        let mut stmt = conn.prepare(&sql)?;

        let sql_params: Vec<&dyn rusqlite::ToSql> = vis_params
            .iter()
            .map(|owner| owner as &dyn rusqlite::ToSql)
            .collect();
        let mut results = Vec::new();
        let mut rows = stmt.query(&sql_params[..])?;
        while let Some(row) = rows.next()? {
            let node = row_to_node(row)?;
            if tags.iter().all(|t| node.tags.contains(t)) {
                results.push(node);
            }
        }
        Ok(results)
    }

    /// Full-text search across node titles, content, and tags.
    pub fn query_by_similarity(
        &self,
        scope: &KnowledgeScope,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<SearchResult>> {
        let conn = self.conn.lock();
        let limit = sql_limit(limit)?;

        // Sanitize FTS query: escape double quotes, wrap tokens in quotes.
        let sanitized: String = query
            .split_whitespace()
            .map(|w| format!("\"{}\"", w.replace('"', "")))
            .collect::<Vec<_>>()
            .join(" ");

        if sanitized.is_empty() {
            return Ok(Vec::new());
        }

        let (vis, vis_params) = scope.visibility_sql("n.owner_agent", 3);
        let sql = format!(
            "SELECT n.id, n.node_type, n.title, n.content, n.tags,
                    n.created_at, n.updated_at, n.source_project, n.owner_agent,
                    rank
             FROM nodes_fts f
             JOIN nodes n ON n.rowid = f.rowid
             WHERE nodes_fts MATCH ?1 AND {vis}
             ORDER BY rank
             LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;

        let mut sql_params: Vec<&dyn rusqlite::ToSql> = vec![&sanitized, &limit];
        sql_params.extend(vis_params.iter().map(|owner| owner as &dyn rusqlite::ToSql));
        let mut results = Vec::new();
        let mut rows = stmt.query(&sql_params[..])?;
        while let Some(row) = rows.next()? {
            let node = row_to_node(row)?;
            let rank: f64 = row.get(9)?;
            results.push(SearchResult {
                node,
                score: -rank, // FTS5 rank is negative (lower = better), invert for intuitive scoring
            });
        }
        Ok(results)
    }

    /// Find nodes directly related to the given node (both outbound and inbound edges).
    pub fn find_related(
        &self,
        scope: &KnowledgeScope,
        node_id: &str,
    ) -> anyhow::Result<Vec<(KnowledgeNode, Relation)>> {
        let conn = self.conn.lock();
        let (vis_edge, vis_params) = scope.visibility_sql("e.owner_agent", 2);
        let (vis_node, _) = scope.visibility_sql("n.owner_agent", 2);
        let sql = format!(
            "SELECT DISTINCT n.id, n.node_type, n.title, n.content, n.tags,
                    n.created_at, n.updated_at, n.source_project, n.owner_agent,
                    e.relation
             FROM edges e
             JOIN nodes n ON n.id = e.to_id
             WHERE e.from_id = ?1 AND {vis_edge} AND {vis_node}
             UNION
             SELECT DISTINCT n.id, n.node_type, n.title, n.content, n.tags,
                    n.created_at, n.updated_at, n.source_project, n.owner_agent,
                    e.relation
             FROM edges e
             JOIN nodes n ON n.id = e.from_id
             WHERE e.to_id = ?1 AND {vis_edge} AND {vis_node}"
        );
        let mut stmt = conn.prepare(&sql)?;

        let mut sql_params: Vec<&dyn rusqlite::ToSql> = vec![&node_id];
        sql_params.extend(vis_params.iter().map(|owner| owner as &dyn rusqlite::ToSql));
        let mut results = Vec::new();
        let mut rows = stmt.query(&sql_params[..])?;
        while let Some(row) = rows.next()? {
            let node = row_to_node(row)?;
            let relation_str: String = row.get(9)?;
            let relation = Relation::parse(&relation_str)?;
            results.push((node, relation));
        }
        Ok(results)
    }

    /// Find nodes reached by edges leaving the given node.
    pub fn find_outbound(
        &self,
        scope: &KnowledgeScope,
        node_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<(KnowledgeNode, Relation)>> {
        self.neighbors_directed(scope, node_id, limit, Direction::Outbound)
    }

    /// Find nodes with edges pointing to the given node.
    pub fn find_inbound(
        &self,
        scope: &KnowledgeScope,
        node_id: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<(KnowledgeNode, Relation)>> {
        self.neighbors_directed(scope, node_id, limit, Direction::Inbound)
    }

    fn neighbors_directed(
        &self,
        scope: &KnowledgeScope,
        node_id: &str,
        limit: usize,
        direction: Direction,
    ) -> anyhow::Result<Vec<(KnowledgeNode, Relation)>> {
        let conn = self.conn.lock();
        let limit = sql_limit(limit)?;
        let (anchor, joined) = match direction {
            Direction::Outbound => ("e.from_id", "e.to_id"),
            Direction::Inbound => ("e.to_id", "e.from_id"),
        };
        let (vis_edge, vis_params) = scope.visibility_sql("e.owner_agent", 3);
        let (vis_node, _) = scope.visibility_sql("n.owner_agent", 3);
        // DISTINCT collapses per-owner copies of the same relation that
        // become jointly visible through a read allowlist.
        let sql = format!(
            "SELECT DISTINCT n.id, n.node_type, n.title, n.content, n.tags,
                    n.created_at, n.updated_at, n.source_project, n.owner_agent,
                    e.relation
             FROM edges e
             JOIN nodes n ON n.id = {joined}
             WHERE {anchor} = ?1 AND {vis_edge} AND {vis_node}
             ORDER BY n.created_at DESC, n.id ASC
             LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;

        let mut sql_params: Vec<&dyn rusqlite::ToSql> = vec![&node_id, &limit];
        sql_params.extend(vis_params.iter().map(|owner| owner as &dyn rusqlite::ToSql));
        let mut results = Vec::new();
        let mut rows = stmt.query(&sql_params[..])?;
        while let Some(row) = rows.next()? {
            let node = row_to_node(row)?;
            let relation_str: String = row.get(9)?;
            let relation = Relation::parse(&relation_str)?;
            results.push((node, relation));
        }
        Ok(results)
    }

    /// Query nodes by type, ordered by most recently updated.
    pub fn query_by_type(
        &self,
        scope: &KnowledgeScope,
        node_type: NodeType,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeNode>> {
        let conn = self.conn.lock();
        let limit = sql_limit(limit)?;
        let (vis, vis_params) = scope.visibility_sql("owner_agent", 3);
        let sql = format!(
            "SELECT id, node_type, title, content, tags, created_at, updated_at, source_project, owner_agent
             FROM nodes WHERE node_type = ?1 AND {vis} ORDER BY updated_at DESC LIMIT ?2"
        );
        let mut stmt = conn.prepare(&sql)?;

        let node_type_str = node_type.as_str();
        let mut sql_params: Vec<&dyn rusqlite::ToSql> = vec![&node_type_str, &limit];
        sql_params.extend(vis_params.iter().map(|owner| owner as &dyn rusqlite::ToSql));
        let mut results = Vec::new();
        let mut rows = stmt.query(&sql_params[..])?;
        while let Some(row) = rows.next()? {
            results.push(row_to_node(row)?);
        }
        Ok(results)
    }

    /// Find outbound nodes matching a relation and node type.
    pub fn find_outbound_by_relation_and_type(
        &self,
        scope: &KnowledgeScope,
        node_id: &str,
        relation: Relation,
        node_type: NodeType,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeNode>> {
        self.neighbors_by_relation_and_type(
            scope,
            node_id,
            relation,
            node_type,
            limit,
            Direction::Outbound,
        )
    }

    /// Find inbound nodes matching a relation and node type.
    pub fn find_inbound_by_relation_and_type(
        &self,
        scope: &KnowledgeScope,
        node_id: &str,
        relation: Relation,
        node_type: NodeType,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeNode>> {
        self.neighbors_by_relation_and_type(
            scope,
            node_id,
            relation,
            node_type,
            limit,
            Direction::Inbound,
        )
    }

    fn neighbors_by_relation_and_type(
        &self,
        scope: &KnowledgeScope,
        node_id: &str,
        relation: Relation,
        node_type: NodeType,
        limit: usize,
        direction: Direction,
    ) -> anyhow::Result<Vec<KnowledgeNode>> {
        let conn = self.conn.lock();
        let limit = sql_limit(limit)?;
        let (anchor, joined) = match direction {
            Direction::Outbound => ("e.from_id", "e.to_id"),
            Direction::Inbound => ("e.to_id", "e.from_id"),
        };
        let (vis_edge, vis_params) = scope.visibility_sql("e.owner_agent", 5);
        let (vis_node, _) = scope.visibility_sql("n.owner_agent", 5);
        let sql = format!(
            "SELECT DISTINCT n.id, n.node_type, n.title, n.content, n.tags,
                    n.created_at, n.updated_at, n.source_project, n.owner_agent
             FROM edges e
             JOIN nodes n ON n.id = {joined}
             WHERE {anchor} = ?1 AND e.relation = ?2 AND n.node_type = ?3
               AND {vis_edge} AND {vis_node}
             ORDER BY n.created_at DESC, n.id ASC
             LIMIT ?4"
        );
        let mut stmt = conn.prepare(&sql)?;

        let relation_str = relation.as_str();
        let node_type_str = node_type.as_str();
        let mut sql_params: Vec<&dyn rusqlite::ToSql> =
            vec![&node_id, &relation_str, &node_type_str, &limit];
        sql_params.extend(vis_params.iter().map(|owner| owner as &dyn rusqlite::ToSql));
        let mut results = Vec::new();
        let mut rows = stmt.query(&sql_params[..])?;
        while let Some(row) = rows.next()? {
            results.push(row_to_node(row)?);
        }
        Ok(results)
    }

    /// Maximum allowed subgraph traversal depth.
    const MAX_SUBGRAPH_DEPTH: usize = 100;

    /// Extract a subgraph starting from `root_id` up to `depth` hops.
    /// `depth` must be between 1 and `MAX_SUBGRAPH_DEPTH` (100).
    /// Uses a recursive CTE for efficient single-query bidirectional
    /// traversal. Traversal never crosses edges or nodes outside the
    /// scope, so invisible regions of the graph are neither returned
    /// nor used as bridges.
    pub fn get_subgraph(
        &self,
        scope: &KnowledgeScope,
        root_id: &str,
        depth: usize,
    ) -> anyhow::Result<(Vec<KnowledgeNode>, Vec<KnowledgeEdge>)> {
        if depth == 0 {
            anyhow::bail!("subgraph depth must be greater than 0");
        }
        let depth = depth.min(Self::MAX_SUBGRAPH_DEPTH);
        let conn = self.conn.lock();

        let (vis_edge, vis_params) = scope.visibility_sql("e.owner_agent", 3);
        let (vis_step, _) = scope.visibility_sql("sn.owner_agent", 3);
        let (vis_node, _) = scope.visibility_sql("n.owner_agent", 3);

        // Collect reachable node IDs via recursive CTE (bidirectional traversal).
        let node_sql = format!(
            "WITH RECURSIVE reachable(id, depth) AS (
                SELECT ?1, 0
                UNION
                SELECT sn.id, r.depth + 1
                FROM reachable r
                JOIN edges e ON e.from_id = r.id OR e.to_id = r.id
                JOIN nodes sn ON sn.id = CASE WHEN e.from_id = r.id THEN e.to_id ELSE e.from_id END
                WHERE r.depth < ?2 AND {vis_edge} AND {vis_step}
             )
             SELECT DISTINCT n.id, n.node_type, n.title, n.content, n.tags,
                    n.created_at, n.updated_at, n.source_project, n.owner_agent
             FROM reachable rc
             JOIN nodes n ON n.id = rc.id
             WHERE {vis_node}"
        );
        let mut node_stmt = conn.prepare(&node_sql)?;

        let depth_param = depth as i64;
        let mut sql_params: Vec<&dyn rusqlite::ToSql> = vec![&root_id, &depth_param];
        sql_params.extend(vis_params.iter().map(|owner| owner as &dyn rusqlite::ToSql));

        let mut nodes = Vec::new();
        let mut node_ids: HashSet<String> = HashSet::new();
        let mut rows = node_stmt.query(&sql_params[..])?;
        while let Some(row) = rows.next()? {
            let node = row_to_node(row)?;
            node_ids.insert(node.id.clone());
            nodes.push(node);
        }
        drop(rows);

        // Collect all visible edges where both endpoints are in the
        // subgraph, collapsing per-owner copies of the same edge.
        let (vis_edge_only, edge_vis_params) = scope.visibility_sql("owner_agent", 1);
        let edge_sql =
            format!("SELECT DISTINCT from_id, to_id, relation FROM edges WHERE {vis_edge_only}");
        let mut edge_stmt = conn.prepare(&edge_sql)?;

        let edge_params: Vec<&dyn rusqlite::ToSql> = edge_vis_params
            .iter()
            .map(|owner| owner as &dyn rusqlite::ToSql)
            .collect();
        let mut edges = Vec::new();
        let mut edge_rows = edge_stmt.query(&edge_params[..])?;
        while let Some(row) = edge_rows.next()? {
            let from_id: String = row.get(0)?;
            let to_id: String = row.get(1)?;
            if node_ids.contains(&from_id) && node_ids.contains(&to_id) {
                let relation_str: String = row.get(2)?;
                let relation = Relation::parse(&relation_str)?;
                edges.push(KnowledgeEdge {
                    from_id,
                    to_id,
                    relation,
                });
            }
        }

        Ok((nodes, edges))
    }

    /// Find experts associated with the given tags via `authored_by` edges.
    pub fn find_experts(
        &self,
        scope: &KnowledgeScope,
        tags: &[String],
    ) -> anyhow::Result<Vec<SearchResult>> {
        // Find nodes matching the tags, then follow authored_by edges to experts.
        let matching = self.query_by_tags(scope, tags)?;
        let mut expert_scores: HashMap<String, f64> = HashMap::new();

        {
            let conn = self.conn.lock();
            let (vis_edge, vis_params) = scope.visibility_sql("owner_agent", 2);
            let sql = format!(
                "SELECT DISTINCT to_id FROM edges
                 WHERE from_id = ?1 AND relation = 'authored_by' AND {vis_edge}"
            );
            for node in &matching {
                let mut stmt = conn.prepare(&sql)?;
                let mut sql_params: Vec<&dyn rusqlite::ToSql> = vec![&node.id];
                sql_params.extend(vis_params.iter().map(|owner| owner as &dyn rusqlite::ToSql));
                let mut rows = stmt.query(&sql_params[..])?;
                while let Some(row) = rows.next()? {
                    let expert_id: String = row.get(0)?;
                    *expert_scores.entry(expert_id).or_default() += 1.0;
                }
            }
        }

        let mut results: Vec<SearchResult> = Vec::new();
        for (eid, score) in expert_scores {
            if let Some(node) = self.get_node(scope, &eid)?
                && node.node_type == NodeType::Expert
            {
                results.push(SearchResult { node, score });
            }
        }

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(results)
    }

    /// Return summary statistics for the scope-visible part of the graph.
    pub fn stats(&self, scope: &KnowledgeScope) -> anyhow::Result<GraphStats> {
        let conn = self.conn.lock();
        let (vis, vis_params) = scope.visibility_sql("owner_agent", 1);
        let plain_params: Vec<&dyn rusqlite::ToSql> = vis_params
            .iter()
            .map(|owner| owner as &dyn rusqlite::ToSql)
            .collect();

        let total_nodes: usize = conn.query_row(
            &format!("SELECT COUNT(*) FROM nodes WHERE {vis}"),
            &plain_params[..],
            |r| r.get(0),
        )?;

        let (vis_e, _) = scope.visibility_sql("e.owner_agent", 1);
        let (vis_a, _) = scope.visibility_sql("a.owner_agent", 1);
        let (vis_b, _) = scope.visibility_sql("b.owner_agent", 1);
        let total_edges: usize = conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM (
                    SELECT DISTINCT e.from_id, e.to_id, e.relation
                    FROM edges e
                    JOIN nodes a ON a.id = e.from_id
                    JOIN nodes b ON b.id = e.to_id
                    WHERE {vis_e} AND {vis_a} AND {vis_b}
                 )"
            ),
            &plain_params[..],
            |r| r.get(0),
        )?;

        let mut by_type = HashMap::new();
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT node_type, COUNT(*) FROM nodes WHERE {vis} GROUP BY node_type"
            ))?;
            let mut rows = stmt.query(&plain_params[..])?;
            while let Some(row) = rows.next()? {
                let t: String = row.get(0)?;
                let c: usize = row.get(1)?;
                by_type.insert(t, c);
            }
        }

        // Top 10 tags by frequency.
        let mut tag_counts: HashMap<String, usize> = HashMap::new();
        {
            let mut stmt = conn.prepare(&format!(
                "SELECT tags FROM nodes WHERE tags != '' AND {vis}"
            ))?;
            let mut rows = stmt.query(&plain_params[..])?;
            while let Some(row) = rows.next()? {
                let tags_str: String = row.get(0)?;
                for tag in tags_str.split(',') {
                    let tag = tag.trim();
                    if !tag.is_empty() {
                        *tag_counts.entry(tag.to_string()).or_default() += 1;
                    }
                }
            }
        }
        let mut top_tags: Vec<(String, usize)> = tag_counts.into_iter().collect();
        top_tags.sort_by_key(|tag| std::cmp::Reverse(tag.1));
        top_tags.truncate(10);

        Ok(GraphStats {
            total_nodes,
            total_edges,
            nodes_by_type: by_type,
            top_tags,
        })
    }
}

/// Direction of an edge relative to the anchor node in neighbor lookups.
#[derive(Clone, Copy)]
enum Direction {
    Outbound,
    Inbound,
}

/// Parse a database row into a `KnowledgeNode`.
fn row_to_node(row: &rusqlite::Row<'_>) -> anyhow::Result<KnowledgeNode> {
    let id: String = row.get(0)?;
    let node_type_str: String = row.get(1)?;
    let title: String = row.get(2)?;
    let content: String = row.get(3)?;
    let tags_str: String = row.get(4)?;
    let created_at_str: String = row.get(5)?;
    let updated_at_str: String = row.get(6)?;
    let source_project: Option<String> = row.get(7)?;
    let owner_agent: Option<String> = row.get(8)?;

    let tags: Vec<String> = tags_str
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let created_at = DateTime::parse_from_rfc3339(&created_at_str)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let updated_at = DateTime::parse_from_rfc3339(&updated_at_str)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());

    Ok(KnowledgeNode {
        id,
        node_type: NodeType::parse(&node_type_str)?,
        title,
        content,
        tags,
        created_at,
        updated_at,
        source_project,
        owner_agent,
    })
}

fn sql_limit(limit: usize) -> anyhow::Result<i64> {
    i64::try_from(limit).context("knowledge graph query limit exceeds SQLite range")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const TEST_AGENT: &str = "test-agent";

    fn agent(alias: &str) -> KnowledgeScope {
        KnowledgeScope::for_agent(alias, Vec::new())
    }

    fn agent_reading_from(alias: &str, read_from: &[&str]) -> KnowledgeScope {
        KnowledgeScope::for_agent(alias, read_from.iter().map(|s| s.to_string()))
    }

    fn test_graph() -> (TempDir, KnowledgeGraph, KnowledgeScope) {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("knowledge.db");
        let graph = KnowledgeGraph::new(&db_path, 1000).unwrap();
        (tmp, graph, agent(TEST_AGENT))
    }

    #[test]
    fn add_node_returns_unique_id() {
        let (_tmp, graph, scope) = test_graph();
        let id1 = graph
            .add_node(
                &scope,
                NodeType::Pattern,
                "Caching",
                "Use Redis for caching",
                &["redis".into()],
                None,
            )
            .unwrap();
        let id2 = graph
            .add_node(&scope, NodeType::Lesson, "Lesson A", "Content A", &[], None)
            .unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn get_node_returns_stored_data() {
        let (_tmp, graph, scope) = test_graph();
        let id = graph
            .add_node(
                &scope,
                NodeType::Decision,
                "Use Postgres",
                "Chose Postgres over MySQL",
                &["database".into(), "postgres".into()],
                Some("project_alpha"),
            )
            .unwrap();

        let node = graph.get_node(&scope, &id).unwrap().unwrap();
        assert_eq!(node.title, "Use Postgres");
        assert_eq!(node.node_type, NodeType::Decision);
        assert_eq!(node.tags, vec!["database", "postgres"]);
        assert_eq!(node.source_project.as_deref(), Some("project_alpha"));
        assert_eq!(node.owner_agent.as_deref(), Some(TEST_AGENT));
    }

    #[test]
    fn get_node_missing_returns_none() {
        let (_tmp, graph, scope) = test_graph();
        assert!(graph.get_node(&scope, "nonexistent").unwrap().is_none());
    }

    #[test]
    fn add_edge_creates_relationship() {
        let (_tmp, graph, scope) = test_graph();
        let id1 = graph
            .add_node(&scope, NodeType::Pattern, "P1", "Pattern one", &[], None)
            .unwrap();
        let id2 = graph
            .add_node(&scope, NodeType::Technology, "T1", "Tech one", &[], None)
            .unwrap();

        graph.add_edge(&scope, &id1, &id2, Relation::Uses).unwrap();

        // Outbound: from id1 → id2
        let outbound = graph.find_outbound(&scope, &id1, 10).unwrap();
        assert!(
            outbound
                .iter()
                .any(|(n, r)| n.id == id2 && *r == Relation::Uses)
        );

        // Inbound: id2 sees id1 via the same edge
        let inbound = graph.find_inbound(&scope, &id2, 10).unwrap();
        assert!(
            inbound
                .iter()
                .any(|(n, r)| n.id == id1 && *r == Relation::Uses)
        );

        // Bidirectional related lookup still sees both directions.
        let related = graph.find_related(&scope, &id2).unwrap();
        assert!(
            related
                .iter()
                .any(|(n, r)| n.id == id1 && *r == Relation::Uses)
        );
    }

    #[test]
    fn add_edge_rejects_missing_node() {
        let (_tmp, graph, scope) = test_graph();
        let id = graph
            .add_node(&scope, NodeType::Lesson, "L1", "Lesson", &[], None)
            .unwrap();
        let err = graph
            .add_edge(&scope, &id, "nonexistent", Relation::Extends)
            .unwrap_err();
        assert!(err.to_string().contains("target node not found"));
    }

    #[test]
    fn query_by_tags_filters_correctly() {
        let (_tmp, graph, scope) = test_graph();
        graph
            .add_node(
                &scope,
                NodeType::Pattern,
                "P1",
                "Content",
                &["rust".into(), "async".into()],
                None,
            )
            .unwrap();
        graph
            .add_node(
                &scope,
                NodeType::Pattern,
                "P2",
                "Content",
                &["rust".into()],
                None,
            )
            .unwrap();
        graph
            .add_node(
                &scope,
                NodeType::Pattern,
                "P3",
                "Content",
                &["python".into()],
                None,
            )
            .unwrap();

        let results = graph.query_by_tags(&scope, &["rust".into()]).unwrap();
        assert_eq!(results.len(), 2);

        let results = graph
            .query_by_tags(&scope, &["rust".into(), "async".into()])
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "P1");
    }

    #[test]
    fn query_by_similarity_returns_ranked_results() {
        let (_tmp, graph, scope) = test_graph();
        graph
            .add_node(
                &scope,
                NodeType::Decision,
                "Choose Rust for performance",
                "Rust gives memory safety and speed",
                &["rust".into()],
                None,
            )
            .unwrap();
        graph
            .add_node(
                &scope,
                NodeType::Lesson,
                "Python scaling issues",
                "Python had GIL bottleneck",
                &["python".into()],
                None,
            )
            .unwrap();

        let results = graph
            .query_by_similarity(&scope, "Rust performance", 10)
            .unwrap();
        assert!(!results.is_empty());
        assert!(results[0].score > 0.0);
    }

    #[test]
    fn subgraph_traversal_collects_connected_nodes() {
        let (_tmp, graph, scope) = test_graph();
        let a = graph
            .add_node(&scope, NodeType::Pattern, "A", "Node A", &[], None)
            .unwrap();
        let b = graph
            .add_node(&scope, NodeType::Pattern, "B", "Node B", &[], None)
            .unwrap();
        let c = graph
            .add_node(&scope, NodeType::Pattern, "C", "Node C", &[], None)
            .unwrap();
        graph.add_edge(&scope, &a, &b, Relation::Extends).unwrap();
        graph.add_edge(&scope, &b, &c, Relation::Uses).unwrap();

        // Forward traversal from A reaches all 3 nodes.
        let (nodes, edges) = graph.get_subgraph(&scope, &a, 2).unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(edges.len(), 2);

        // Bidirectional: starting from C with depth 2 also reaches A.
        let (nodes, edges) = graph.get_subgraph(&scope, &c, 2).unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn expert_ranking_by_authored_contributions() {
        let (_tmp, graph, scope) = test_graph();
        let expert = graph
            .add_node(
                &scope,
                NodeType::Expert,
                "zeroclaw_user",
                "Backend expert",
                &[],
                None,
            )
            .unwrap();
        let p1 = graph
            .add_node(
                &scope,
                NodeType::Pattern,
                "Cache pattern",
                "Redis caching",
                &["caching".into()],
                None,
            )
            .unwrap();
        let p2 = graph
            .add_node(
                &scope,
                NodeType::Pattern,
                "Queue pattern",
                "Message queue",
                &["caching".into()],
                None,
            )
            .unwrap();

        graph
            .add_edge(&scope, &p1, &expert, Relation::AuthoredBy)
            .unwrap();
        graph
            .add_edge(&scope, &p2, &expert, Relation::AuthoredBy)
            .unwrap();

        let experts = graph.find_experts(&scope, &["caching".into()]).unwrap();
        assert_eq!(experts.len(), 1);
        assert_eq!(experts[0].node.title, "zeroclaw_user");
        assert!((experts[0].score - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn max_nodes_limit_enforced() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("knowledge.db");
        let graph = KnowledgeGraph::new(&db_path, 2).unwrap();
        let scope = agent(TEST_AGENT);

        graph
            .add_node(&scope, NodeType::Lesson, "L1", "C1", &[], None)
            .unwrap();
        graph
            .add_node(&scope, NodeType::Lesson, "L2", "C2", &[], None)
            .unwrap();
        let err = graph
            .add_node(&scope, NodeType::Lesson, "L3", "C3", &[], None)
            .unwrap_err();
        assert!(err.to_string().contains("node limit reached"));
    }

    #[test]
    fn stats_reports_correct_counts() {
        let (_tmp, graph, scope) = test_graph();
        graph
            .add_node(&scope, NodeType::Pattern, "P", "C", &["rust".into()], None)
            .unwrap();
        graph
            .add_node(
                &scope,
                NodeType::Lesson,
                "L",
                "C",
                &["rust".into(), "async".into()],
                None,
            )
            .unwrap();

        let stats = graph.stats(&scope).unwrap();
        assert_eq!(stats.total_nodes, 2);
        assert_eq!(stats.nodes_by_type.get("pattern"), Some(&1));
        assert_eq!(stats.nodes_by_type.get("lesson"), Some(&1));
        assert!(!stats.top_tags.is_empty());
    }

    #[test]
    fn node_type_roundtrip() {
        for nt in NodeType::ALL {
            assert_eq!(NodeType::parse(nt.as_str()).unwrap(), *nt);
        }
        assert_eq!(
            NodeType::schema_values(),
            &[
                "pattern",
                "decision",
                "lesson",
                "expert",
                "technology",
                "client",
                "contact",
                "interaction",
            ],
        );
    }

    #[test]
    fn relation_roundtrip() {
        for r in Relation::ALL {
            assert_eq!(Relation::parse(r.as_str()).unwrap(), *r);
        }
        assert_eq!(
            Relation::schema_values(),
            &[
                "uses",
                "replaces",
                "extends",
                "authored_by",
                "applies_to",
                "manages_client",
                "contact_of",
                "interacted_with",
            ],
        );
    }

    #[test]
    fn client_relationship_types_roundtrip_through_queries() {
        let (_tmp, graph, scope) = test_graph();
        let client = graph
            .add_node(
                &scope,
                NodeType::Client,
                "Example Account",
                "Enterprise account for relationship tracking",
                &["enterprise".into()],
                None,
            )
            .unwrap();
        let contact = graph
            .add_node(
                &scope,
                NodeType::Contact,
                "Contact Alpha",
                "Primary technical contact",
                &["technical".into()],
                None,
            )
            .unwrap();
        let expert = graph
            .add_node(
                &scope,
                NodeType::Expert,
                "Expert Alpha",
                "Owns the account relationship",
                &["relationship-owner".into()],
                None,
            )
            .unwrap();
        let interaction = graph
            .add_node(
                &scope,
                NodeType::Interaction,
                "Discovery call",
                "Discussed integration requirements",
                &["call".into()],
                None,
            )
            .unwrap();

        graph
            .add_edge(&scope, &contact, &client, Relation::ContactOf)
            .unwrap();
        graph
            .add_edge(&scope, &expert, &client, Relation::ManagesClient)
            .unwrap();
        graph
            .add_edge(&scope, &client, &interaction, Relation::InteractedWith)
            .unwrap();

        let clients = graph.query_by_type(&scope, NodeType::Client, 10).unwrap();
        assert_eq!(clients.len(), 1);
        assert_eq!(clients[0].id, client);

        let interactions = graph
            .find_outbound_by_relation_and_type(
                &scope,
                &client,
                Relation::InteractedWith,
                NodeType::Interaction,
                10,
            )
            .unwrap();
        assert_eq!(interactions.len(), 1);
        assert_eq!(interactions[0].id, interaction);

        let inbound = graph.find_inbound(&scope, &client, 10).unwrap();
        assert!(
            inbound
                .iter()
                .any(|(node, relation)| node.id == contact && *relation == Relation::ContactOf)
        );
        assert!(
            inbound
                .iter()
                .any(|(node, relation)| node.id == expert && *relation == Relation::ManagesClient)
        );
        let contacts = graph
            .find_inbound_by_relation_and_type(
                &scope,
                &client,
                Relation::ContactOf,
                NodeType::Contact,
                10,
            )
            .unwrap();
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].id, contact);

        let (nodes, edges) = graph.get_subgraph(&scope, &client, 1).unwrap();
        assert_eq!(nodes.len(), 4);
        assert_eq!(edges.len(), 3);
        assert!(
            edges
                .iter()
                .any(|edge| edge.relation == Relation::InteractedWith)
        );
    }

    // ── Attribution and scoping ─────────────────────────────────

    #[test]
    fn foreign_nodes_are_invisible_across_every_read_path() {
        let (_tmp, graph, rowan) = test_graph();
        let sable = agent("sable");

        let secret = graph
            .add_node(
                &sable,
                NodeType::Client,
                "Sable client",
                "confidential client roster",
                &["confidential".into()],
                None,
            )
            .unwrap();
        let note = graph
            .add_node(
                &sable,
                NodeType::Interaction,
                "Sable call",
                "confidential call notes",
                &["confidential".into()],
                None,
            )
            .unwrap();
        graph
            .add_edge(&sable, &secret, &note, Relation::InteractedWith)
            .unwrap();

        assert!(graph.get_node(&rowan, &secret).unwrap().is_none());
        assert!(
            graph
                .query_by_tags(&rowan, &["confidential".into()])
                .unwrap()
                .is_empty()
        );
        assert!(
            graph
                .query_by_similarity(&rowan, "confidential", 10)
                .unwrap()
                .is_empty()
        );
        assert!(
            graph
                .query_by_type(&rowan, NodeType::Client, 10)
                .unwrap()
                .is_empty()
        );
        assert!(graph.find_related(&rowan, &secret).unwrap().is_empty());
        assert!(graph.find_outbound(&rowan, &secret, 10).unwrap().is_empty());
        assert!(graph.find_inbound(&rowan, &note, 10).unwrap().is_empty());
        assert!(
            graph
                .find_outbound_by_relation_and_type(
                    &rowan,
                    &secret,
                    Relation::InteractedWith,
                    NodeType::Interaction,
                    10,
                )
                .unwrap()
                .is_empty()
        );
        let (nodes, edges) = graph.get_subgraph(&rowan, &secret, 3).unwrap();
        assert!(nodes.is_empty());
        assert!(edges.is_empty());

        let stats = graph.stats(&rowan).unwrap();
        assert_eq!(stats.total_nodes, 0);
        assert_eq!(stats.total_edges, 0);
        assert!(stats.top_tags.is_empty());

        // The owning scope still sees everything.
        assert!(graph.get_node(&sable, &secret).unwrap().is_some());
        assert_eq!(graph.stats(&sable).unwrap().total_nodes, 2);
    }

    #[test]
    fn relate_refuses_foreign_endpoints_without_leaking_existence() {
        let (_tmp, graph, rowan) = test_graph();
        let sable = agent("sable");

        let own = graph
            .add_node(&rowan, NodeType::Pattern, "Mine", "Own node", &[], None)
            .unwrap();
        let foreign = graph
            .add_node(
                &sable,
                NodeType::Pattern,
                "Theirs",
                "Foreign node",
                &[],
                None,
            )
            .unwrap();

        let err = graph
            .add_edge(&rowan, &own, &foreign, Relation::Uses)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("target node not found: {foreign}"),
            "foreign endpoints must read exactly like missing ones"
        );
        let err = graph
            .add_edge(&rowan, &foreign, &own, Relation::Uses)
            .unwrap_err();
        assert_eq!(err.to_string(), format!("source node not found: {foreign}"));
    }

    #[test]
    fn legacy_rows_fail_closed_until_assigned_to_one_agent() {
        let (_tmp, graph, _unused) = test_graph();
        let rowan = agent("rowan");
        let sable = agent("sable");
        let admin = KnowledgeScope::unrestricted();

        // Unrestricted writes model unattributed pre-upgrade rows.
        let shared_a = graph
            .add_node(
                &admin,
                NodeType::Pattern,
                "Shared pattern",
                "legacy row",
                &["shared".into()],
                None,
            )
            .unwrap();
        let shared_b = graph
            .add_node(
                &admin,
                NodeType::Technology,
                "Shared tech",
                "legacy row",
                &["shared".into()],
                None,
            )
            .unwrap();

        graph
            .add_edge(&admin, &shared_a, &shared_b, Relation::Uses)
            .unwrap();
        for scope in [&rowan, &sable] {
            assert!(graph.get_node(scope, &shared_a).unwrap().is_none());
            assert!(
                graph
                    .query_by_tags(scope, &["shared".into()])
                    .unwrap()
                    .is_empty()
            );
        }
        let err = graph.prepare_legacy_ownership(None).unwrap_err();
        assert!(err.to_string().contains("legacy_owner_agent"));

        assert_eq!(graph.prepare_legacy_ownership(Some("rowan")).unwrap(), 3);
        let node = graph.get_node(&rowan, &shared_a).unwrap().unwrap();
        assert_eq!(node.owner_agent.as_deref(), Some("rowan"));
        assert!(graph.get_node(&sable, &shared_a).unwrap().is_none());
        assert_eq!(graph.find_outbound(&rowan, &shared_a, 10).unwrap().len(), 1);

        // The assigned graph behaves like any other private agent state.
        graph
            .add_edge(&rowan, &shared_a, &shared_b, Relation::Uses)
            .unwrap();
        assert_eq!(graph.find_outbound(&rowan, &shared_a, 10).unwrap().len(), 1);
        assert!(
            graph
                .find_outbound(&sable, &shared_a, 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn read_allowlist_widens_reads_but_not_writes() {
        let (_tmp, graph, _unused) = test_graph();
        let sable = agent("sable");
        let rowan = agent_reading_from("rowan", &["sable"]);

        let sable_node = graph
            .add_node(
                &sable,
                NodeType::Decision,
                "Sable decision",
                "shared with rowan",
                &["ops".into()],
                None,
            )
            .unwrap();

        // Reads widen to the allowlisted sibling.
        assert!(graph.get_node(&rowan, &sable_node).unwrap().is_some());
        assert_eq!(
            graph
                .query_by_similarity(&rowan, "shared rowan", 10)
                .unwrap()
                .len(),
            1
        );

        // Writes still stamp the caller, never the sibling.
        let rowan_node = graph
            .add_node(
                &rowan,
                NodeType::Pattern,
                "Rowan note",
                "annotation",
                &[],
                None,
            )
            .unwrap();
        let stored = graph.get_node(&rowan, &rowan_node).unwrap().unwrap();
        assert_eq!(stored.owner_agent.as_deref(), Some("rowan"));

        // Rowan may annotate the shared node; sable does not see the
        // annotation edge and the allowlist is directional.
        graph
            .add_edge(&rowan, &rowan_node, &sable_node, Relation::AppliesTo)
            .unwrap();
        assert!(
            graph
                .find_inbound(&sable, &sable_node, 10)
                .unwrap()
                .is_empty()
        );
        assert!(graph.get_node(&sable, &rowan_node).unwrap().is_none());
    }

    #[test]
    fn duplicate_edges_across_scopes_collapse_for_joint_readers() {
        let (_tmp, graph, _unused) = test_graph();
        let seed = agent("seed");
        let rowan = agent_reading_from("rowan", &["seed"]);
        let sable = agent_reading_from("sable", &["seed"]);
        let carol = agent_reading_from("carol", &["seed", "rowan", "sable"]);

        let a = graph
            .add_node(&seed, NodeType::Pattern, "Seed A", "shared", &[], None)
            .unwrap();
        let b = graph
            .add_node(&seed, NodeType::Technology, "Seed B", "shared", &[], None)
            .unwrap();

        // Rowan and sable independently record the same relation; each
        // write is invisible to the other, so both rows exist.
        graph.add_edge(&rowan, &a, &b, Relation::Uses).unwrap();
        graph.add_edge(&sable, &a, &b, Relation::Uses).unwrap();

        // A reader who sees both scopes gets the relation once.
        let neighbors = graph.find_outbound(&carol, &a, 10).unwrap();
        assert_eq!(neighbors.len(), 1);
        let related = graph.find_related(&carol, &a).unwrap();
        assert_eq!(related.len(), 1);
        let (_, edges) = graph.get_subgraph(&carol, &a, 1).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(graph.stats(&carol).unwrap().total_edges, 1);

        // Re-adding an edge the caller can already see stays a no-op.
        graph.add_edge(&rowan, &a, &b, Relation::Uses).unwrap();
        assert_eq!(graph.find_outbound(&rowan, &a, 10).unwrap().len(), 1);
    }

    #[test]
    fn subgraph_traversal_does_not_bridge_through_foreign_regions() {
        let (_tmp, graph, _unused) = test_graph();
        let seed = agent("seed");
        let rowan = agent_reading_from("rowan", &["seed"]);
        let sable = agent_reading_from("sable", &["seed"]);

        // Layout: rowan_node -> shared (rowan's edge), and sable's own
        // chain shared -> sable_private (sable's edge).
        let rowan_node = graph
            .add_node(&rowan, NodeType::Pattern, "Rowan node", "mine", &[], None)
            .unwrap();
        let shared = graph
            .add_node(&seed, NodeType::Technology, "Shared", "shared", &[], None)
            .unwrap();
        let sable_private = graph
            .add_node(
                &sable,
                NodeType::Client,
                "Sable client",
                "private",
                &[],
                None,
            )
            .unwrap();
        graph
            .add_edge(&rowan, &rowan_node, &shared, Relation::Uses)
            .unwrap();
        graph
            .add_edge(&sable, &shared, &sable_private, Relation::AppliesTo)
            .unwrap();

        let (nodes, edges) = graph.get_subgraph(&rowan, &rowan_node, 5).unwrap();
        let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&rowan_node.as_str()));
        assert!(ids.contains(&shared.as_str()));
        assert!(
            !ids.contains(&sable_private.as_str()),
            "traversal must not cross into another agent's region"
        );
        assert_eq!(edges.len(), 1);

        // Sable's walk from the shared node sees only sable's region.
        let (nodes, _) = graph.get_subgraph(&sable, &shared, 5).unwrap();
        let ids: Vec<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&sable_private.as_str()));
        assert!(!ids.contains(&rowan_node.as_str()));
    }

    #[test]
    fn experts_from_foreign_scopes_stay_hidden() {
        let (_tmp, graph, rowan) = test_graph();
        let sable = agent("sable");

        let expert = graph
            .add_node(
                &sable,
                NodeType::Expert,
                "Sable expert",
                "expert",
                &[],
                None,
            )
            .unwrap();
        let pattern = graph
            .add_node(
                &sable,
                NodeType::Pattern,
                "Sable pattern",
                "pattern",
                &["caching".into()],
                None,
            )
            .unwrap();
        graph
            .add_edge(&sable, &pattern, &expert, Relation::AuthoredBy)
            .unwrap();

        assert!(
            graph
                .find_experts(&rowan, &["caching".into()])
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            graph
                .find_experts(&sable, &["caching".into()])
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn unrestricted_scope_sees_every_row() {
        let (_tmp, graph, rowan) = test_graph();
        let sable = agent("sable");
        let admin = KnowledgeScope::unrestricted();

        graph
            .add_node(&rowan, NodeType::Pattern, "R", "rowan row", &[], None)
            .unwrap();
        graph
            .add_node(&sable, NodeType::Pattern, "S", "sable row", &[], None)
            .unwrap();

        let stats = graph.stats(&admin).unwrap();
        assert_eq!(stats.total_nodes, 2);
        assert_eq!(
            graph
                .query_by_type(&admin, NodeType::Pattern, 10)
                .unwrap()
                .len(),
            2
        );
    }

    // ── Migration ───────────────────────────────────────────────

    /// Build a database with the pre-attribution schema and some rows,
    /// exactly as v0.8.3 created it.
    fn legacy_database(db_path: &Path) -> (String, String) {
        let conn = Connection::open(db_path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE nodes (
                id TEXT PRIMARY KEY,
                node_type TEXT NOT NULL,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                source_project TEXT
             );
             CREATE TABLE edges (
                from_id TEXT NOT NULL,
                to_id TEXT NOT NULL,
                relation TEXT NOT NULL,
                PRIMARY KEY (from_id, to_id, relation),
                FOREIGN KEY (from_id) REFERENCES nodes(id) ON DELETE CASCADE,
                FOREIGN KEY (to_id) REFERENCES nodes(id) ON DELETE CASCADE
             );
             CREATE VIRTUAL TABLE nodes_fts USING fts5(
                title, content, tags, content='nodes', content_rowid='rowid'
             );
             CREATE TRIGGER nodes_ai AFTER INSERT ON nodes BEGIN
                INSERT INTO nodes_fts(rowid, title, content, tags)
                VALUES (new.rowid, new.title, new.content, new.tags);
             END;
             CREATE INDEX idx_edges_from ON edges(from_id);
             CREATE INDEX idx_edges_to ON edges(to_id);",
        )
        .unwrap();
        let now = Utc::now().to_rfc3339();
        let id_a = "legacy-node-a".to_string();
        let id_b = "legacy-node-b".to_string();
        for (id, title) in [(&id_a, "Legacy pattern"), (&id_b, "Legacy tech")] {
            conn.execute(
                "INSERT INTO nodes (id, node_type, title, content, tags, created_at, updated_at, source_project)
                 VALUES (?1, ?2, ?3, 'legacy content', 'legacy', ?4, ?4, NULL)",
                params![id, "pattern", title, now],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO edges (from_id, to_id, relation) VALUES (?1, ?2, 'uses')",
            params![id_a, id_b],
        )
        .unwrap();
        (id_a, id_b)
    }

    #[test]
    fn migration_assigns_legacy_rows_to_one_owner_and_is_idempotent() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("knowledge.db");
        let (id_a, id_b) = legacy_database(&db_path);

        let graph = KnowledgeGraph::new(&db_path, 1000).unwrap();
        let rowan = agent("rowan");

        assert!(graph.get_node(&rowan, &id_a).unwrap().is_none());
        assert!(graph.prepare_legacy_ownership(None).is_err());
        assert_eq!(graph.prepare_legacy_ownership(Some("rowan")).unwrap(), 3);

        // Legacy nodes and their edge survive and belong only to the selected
        // owner. FTS remains intact after attribution.
        let node = graph.get_node(&rowan, &id_a).unwrap().unwrap();
        assert_eq!(node.owner_agent.as_deref(), Some("rowan"));
        assert_eq!(node.title, "Legacy pattern");
        let neighbors = graph.find_outbound(&rowan, &id_a, 10).unwrap();
        assert_eq!(neighbors.len(), 1);
        assert_eq!(neighbors[0].0.id, id_b);

        assert!(
            !graph
                .query_by_similarity(&rowan, "legacy content", 10)
                .unwrap()
                .is_empty()
        );

        // New writes are attributed.
        let new_id = graph
            .add_node(&rowan, NodeType::Lesson, "New", "new row", &[], None)
            .unwrap();
        assert_eq!(
            graph
                .get_node(&rowan, &new_id)
                .unwrap()
                .unwrap()
                .owner_agent
                .as_deref(),
            Some("rowan")
        );
        drop(graph);

        // Reopening (running the migration path again) changes nothing.
        let graph = KnowledgeGraph::new(&db_path, 1000).unwrap();
        assert_eq!(graph.prepare_legacy_ownership(None).unwrap(), 0);
        let stats = graph.stats(&agent("sable")).unwrap();
        assert_eq!(stats.total_nodes, 0);
        assert_eq!(stats.total_edges, 0);
        assert_eq!(graph.stats(&rowan).unwrap().total_nodes, 3);
    }

    #[test]
    fn migrated_schema_preserves_old_insert_or_ignore_idempotency() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("knowledge.db");
        let (id_a, id_b) = legacy_database(&db_path);
        drop(KnowledgeGraph::new(&db_path, 1000).unwrap());

        // Simulate a downgraded pre-attribution binary: its insert omits the
        // owner column and relies on INSERT OR IGNORE for idempotency.
        let conn = Connection::open(&db_path).unwrap();
        for _ in 0..2 {
            conn.execute(
                "INSERT OR IGNORE INTO edges (from_id, to_id, relation) VALUES (?1, ?2, 'uses')",
                params![id_a, id_b],
            )
            .unwrap();
        }
        let count: usize = conn
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE from_id = ?1 AND to_id = ?2 AND relation = 'uses'",
                params![id_a, id_b],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn rename_and_delete_follow_durable_owner_lifecycle() {
        let (_tmp, graph, _unused) = test_graph();
        let rowan = agent("rowan");
        let a = graph
            .add_node(&rowan, NodeType::Pattern, "A", "owned", &[], None)
            .unwrap();
        let b = graph
            .add_node(&rowan, NodeType::Technology, "B", "owned", &[], None)
            .unwrap();
        graph.add_edge(&rowan, &a, &b, Relation::Uses).unwrap();

        assert_eq!(graph.rename_owner("rowan", "renamed").unwrap(), 3);
        assert_eq!(graph.count_owner("rowan").unwrap(), 0);
        assert_eq!(graph.count_owner("renamed").unwrap(), 3);
        assert!(graph.get_node(&agent("renamed"), &a).unwrap().is_some());
        assert!(graph.get_node(&rowan, &a).unwrap().is_none());

        let export = graph.export_owner("renamed").unwrap();
        assert_eq!(export["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(export["edges"].as_array().unwrap().len(), 1);
        assert_eq!(graph.purge_owner("renamed").unwrap(), 3);
        assert_eq!(graph.count_owner("renamed").unwrap(), 0);
    }

    #[test]
    fn migrated_database_serves_concurrent_per_agent_connections() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("knowledge.db");
        legacy_database(&db_path);

        // Production opens one connection per agent onto the same file;
        // the second open must find the migration already applied.
        let graph_a = KnowledgeGraph::new(&db_path, 1000).unwrap();
        let graph_b = KnowledgeGraph::new(&db_path, 1000).unwrap();
        let rowan = agent("rowan");
        let sable = agent("sable");

        let rowan_id = graph_a
            .add_node(&rowan, NodeType::Pattern, "Rowan", "mine", &[], None)
            .unwrap();
        assert!(
            graph_b.get_node(&sable, &rowan_id).unwrap().is_none(),
            "attribution must hold across separate connections to one file"
        );
        assert!(graph_b.get_node(&rowan, &rowan_id).unwrap().is_some());
    }
}
