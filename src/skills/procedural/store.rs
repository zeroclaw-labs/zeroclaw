//! SkillStore — CRUD + FTS5 search for procedural skills.

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// A full skill record from the database.
#[derive(Debug, Clone)]
pub struct SkillRecord {
    pub id: String,
    pub name: String,
    pub category: Option<String>,
    pub description: String,
    pub content_md: String,
    pub version: i64,
    pub use_count: i64,
    pub success_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub created_by: String,
    pub device_id: String,
}

/// A reference file attached to a skill (L2 depth).
#[derive(Debug, Clone)]
pub struct SkillReference {
    pub skill_id: String,
    pub file_path: String,
    pub content: String,
}

/// Which section of a skill to patch.
#[derive(Debug, Clone)]
pub enum PatchTarget {
    /// Append to the Pitfalls section
    Pitfalls,
    /// Replace the Procedure section
    Procedure,
    /// Replace the full content
    Full,
}

pub struct SkillStore {
    conn: Arc<Mutex<Connection>>,
    device_id: String,
}

impl SkillStore {
    pub fn new(conn: Arc<Mutex<Connection>>, device_id: String) -> Self {
        Self { conn, device_id }
    }

    /// Run schema migration. Call once at startup.
    pub fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock();
        super::schema::migrate(&conn).context("skill schema migration failed")
    }

    /// Create a new skill from a generated SKILL.md document.
    ///
    /// Rejects empty / whitespace-only names (UNIQUE NOT NULL would panic on
    /// the second blank insert) and empty content (an empty skill is useless
    /// and produces empty FTS5 rows that pollute search rankings).
    pub fn create(
        &self,
        name: &str,
        category: Option<&str>,
        description: &str,
        content_md: &str,
        created_by: &str,
    ) -> Result<String> {
        let trimmed_name = name.trim();
        if trimmed_name.is_empty() {
            anyhow::bail!("skill name must not be empty");
        }
        if description.trim().is_empty() {
            anyhow::bail!("skill description must not be empty");
        }
        if content_md.trim().is_empty() {
            anyhow::bail!("skill content_md must not be empty");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO skills (id, name, category, description, content_md, created_by, device_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![id, trimmed_name, category, description, content_md, created_by, self.device_id],
        )
        .context("failed to insert skill")?;
        // Mirror into FTS5
        conn.execute(
            "INSERT INTO skills_fts (skill_id, name, description, content_md)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, trimmed_name, description, content_md],
        )
        .context("failed to mirror skill into FTS5")?;
        Ok(id)
    }

    /// Get a skill by name.
    pub fn get_by_name(&self, name: &str) -> Result<Option<SkillRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, category, description, content_md, version,
                    use_count, success_count, created_at, updated_at, created_by, device_id
             FROM skills WHERE name = ?1",
        )?;
        let mut rows = stmt.query_map(params![name], |row| {
            Ok(SkillRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                category: row.get(2)?,
                description: row.get(3)?,
                content_md: row.get(4)?,
                version: row.get(5)?,
                use_count: row.get(6)?,
                success_count: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                created_by: row.get(10)?,
                device_id: row.get(11)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    /// Get a skill by id.
    pub fn get(&self, id: &str) -> Result<Option<SkillRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, category, description, content_md, version,
                    use_count, success_count, created_at, updated_at, created_by, device_id
             FROM skills WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id], |row| {
            Ok(SkillRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                category: row.get(2)?,
                description: row.get(3)?,
                content_md: row.get(4)?,
                version: row.get(5)?,
                use_count: row.get(6)?,
                success_count: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                created_by: row.get(10)?,
                device_id: row.get(11)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    /// List all active skills (brief summaries for L0 injection).
    pub fn list_all(&self) -> Result<Vec<SkillRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, category, description, content_md, version,
                    use_count, success_count, created_at, updated_at, created_by, device_id
             FROM skills ORDER BY use_count DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SkillRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                category: row.get(2)?,
                description: row.get(3)?,
                content_md: row.get(4)?,
                version: row.get(5)?,
                use_count: row.get(6)?,
                success_count: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                created_by: row.get(10)?,
                device_id: row.get(11)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to list skills")
    }

    /// List skills filtered by category.
    pub fn list_by_category(&self, category: &str) -> Result<Vec<SkillRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id, name, category, description, content_md, version,
                    use_count, success_count, created_at, updated_at, created_by, device_id
             FROM skills WHERE category = ?1 ORDER BY use_count DESC",
        )?;
        let rows = stmt.query_map(params![category], |row| {
            Ok(SkillRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                category: row.get(2)?,
                description: row.get(3)?,
                content_md: row.get(4)?,
                version: row.get(5)?,
                use_count: row.get(6)?,
                success_count: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                created_by: row.get(10)?,
                device_id: row.get(11)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to list skills by category")
    }

    /// Full-text search across skill name, description, and content.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SkillRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT s.id, s.name, s.category, s.description, s.content_md, s.version,
                    s.use_count, s.success_count, s.created_at, s.updated_at, s.created_by, s.device_id
             FROM skills_fts f
             JOIN skills s ON s.id = f.skill_id
             WHERE skills_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![query, limit as i64], |row| {
            Ok(SkillRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                category: row.get(2)?,
                description: row.get(3)?,
                content_md: row.get(4)?,
                version: row.get(5)?,
                use_count: row.get(6)?,
                success_count: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                created_by: row.get(10)?,
                device_id: row.get(11)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("FTS5 skill search failed")
    }

    /// Patch a specific section of a skill's content.
    pub fn patch(&self, skill_id: &str, target: PatchTarget, patch_content: &str) -> Result<()> {
        let conn = self.conn.lock();
        let now = now_epoch();

        let new_content = match target {
            PatchTarget::Full => patch_content.to_string(),
            PatchTarget::Pitfalls => {
                let current: String = conn.query_row(
                    "SELECT content_md FROM skills WHERE id = ?1",
                    params![skill_id],
                    |row| row.get(0),
                )?;
                append_to_section(&current, "## Pitfalls", patch_content)
            }
            PatchTarget::Procedure => {
                let current: String = conn.query_row(
                    "SELECT content_md FROM skills WHERE id = ?1",
                    params![skill_id],
                    |row| row.get(0),
                )?;
                replace_section(&current, "## Procedure", patch_content)
            }
        };

        conn.execute(
            "UPDATE skills SET content_md = ?1, version = version + 1, updated_at = ?2 WHERE id = ?3",
            params![new_content, now, skill_id],
        )?;

        // Refresh FTS5 row for this skill
        conn.execute(
            "DELETE FROM skills_fts WHERE skill_id = ?1",
            params![skill_id],
        )?;
        let (name, description): (String, String) = conn.query_row(
            "SELECT name, description FROM skills WHERE id = ?1",
            params![skill_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        conn.execute(
            "INSERT INTO skills_fts (skill_id, name, description, content_md)
             VALUES (?1, ?2, ?3, ?4)",
            params![skill_id, name, description, new_content],
        )?;
        Ok(())
    }

    /// Record a usage event (success or failure) for a skill.
    pub fn record_usage(&self, skill_id: &str, succeeded: bool) -> Result<()> {
        let conn = self.conn.lock();
        let now = now_epoch();
        if succeeded {
            conn.execute(
                "UPDATE skills SET use_count = use_count + 1, success_count = success_count + 1, updated_at = ?1 WHERE id = ?2",
                params![now, skill_id],
            )?;
        } else {
            conn.execute(
                "UPDATE skills SET use_count = use_count + 1, updated_at = ?1 WHERE id = ?2",
                params![now, skill_id],
            )?;
        }
        Ok(())
    }

    /// Delete a skill by id.
    pub fn delete(&self, skill_id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM skills WHERE id = ?1", params![skill_id])?;
        conn.execute(
            "DELETE FROM skills_fts WHERE skill_id = ?1",
            params![skill_id],
        )?;
        Ok(())
    }

    /// Add a reference file to a skill (L2 content).
    pub fn add_reference(
        &self,
        skill_id: &str,
        file_path: &str,
        content: &str,
    ) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO skill_references (skill_id, file_path, content)
             VALUES (?1, ?2, ?3)",
            params![skill_id, file_path, content],
        )?;
        Ok(())
    }

    /// Get references for a skill.
    pub fn get_references(&self, skill_id: &str) -> Result<Vec<SkillReference>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT skill_id, file_path, content FROM skill_references WHERE skill_id = ?1",
        )?;
        let rows = stmt.query_map(params![skill_id], |row| {
            Ok(SkillReference {
                skill_id: row.get(0)?,
                file_path: row.get(1)?,
                content: row.get(2)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to get skill references")
    }

    /// Upsert a skill from a sync delta (version LWW).
    pub fn upsert_from_sync(
        &self,
        id: &str,
        name: &str,
        category: Option<&str>,
        description: &str,
        content_md: &str,
        version: i64,
        created_by: &str,
        device_id: &str,
    ) -> Result<()> {
        let conn = self.conn.lock();
        let now = now_epoch();
        let changed = conn.execute(
            "INSERT INTO skills (id, name, category, description, content_md, version, created_by, device_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                category = excluded.category,
                description = excluded.description,
                content_md = excluded.content_md,
                version = excluded.version,
                updated_at = excluded.updated_at
             WHERE excluded.version > skills.version",
            params![id, name, category, description, content_md, version, created_by, device_id, now],
        )?;
        if changed > 0 {
            conn.execute(
                "DELETE FROM skills_fts WHERE skill_id = ?1",
                params![id],
            )?;
            conn.execute(
                "INSERT INTO skills_fts (skill_id, name, description, content_md)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, name, description, content_md],
            )?;
        }
        Ok(())
    }

    /// Get skills with low usage for archival candidates (Dream Cycle).
    pub fn low_usage_candidates(&self, min_age_days: i64, max_use_count: i64) -> Result<Vec<SkillRecord>> {
        let conn = self.conn.lock();
        let cutoff = now_epoch() - (min_age_days * 86400);
        let mut stmt = conn.prepare(
            "SELECT id, name, category, description, content_md, version,
                    use_count, success_count, created_at, updated_at, created_by, device_id
             FROM skills
             WHERE created_at < ?1 AND use_count <= ?2
             ORDER BY use_count ASC, updated_at ASC",
        )?;
        let rows = stmt.query_map(params![cutoff, max_use_count], |row| {
            Ok(SkillRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                category: row.get(2)?,
                description: row.get(3)?,
                content_md: row.get(4)?,
                version: row.get(5)?,
                use_count: row.get(6)?,
                success_count: row.get(7)?,
                created_at: row.get(8)?,
                updated_at: row.get(9)?,
                created_by: row.get(10)?,
                device_id: row.get(11)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("failed to query low-usage skills")
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Append content after a markdown section header. If the section doesn't
/// exist, append it at the end of the document.
///
/// Uses `rfind` to locate the *last* occurrence of the header — if a skill
/// ever ends up with duplicate section headers (manual edits, merge hiccup),
/// new pitfalls still land in the most recent section rather than the first
/// stale one.
fn append_to_section(doc: &str, header: &str, content: &str) -> String {
    if let Some(pos) = doc.rfind(header) {
        // Find the next section header (## ...) after the current one
        let after_header = pos + header.len();
        if let Some(next_section) = doc[after_header..].find("\n## ") {
            let insert_pos = after_header + next_section;
            format!(
                "{}\n- {}\n{}",
                &doc[..insert_pos],
                content,
                &doc[insert_pos..]
            )
        } else {
            // No next section — append at end
            format!("{}\n- {}\n", doc, content)
        }
    } else {
        // Section doesn't exist — create it at the end
        format!("{}\n\n{}\n\n- {}\n", doc, header, content)
    }
}

/// Replace the content of a markdown section (everything between the header
/// and the next ## header).
fn replace_section(doc: &str, header: &str, new_content: &str) -> String {
    if let Some(pos) = doc.find(header) {
        let after_header = pos + header.len();
        if let Some(next_section) = doc[after_header..].find("\n## ") {
            let end_pos = after_header + next_section;
            format!("{}\n\n{}\n{}", &doc[..pos + header.len()], new_content, &doc[end_pos..])
        } else {
            format!("{}\n\n{}\n", &doc[..pos + header.len()], new_content)
        }
    } else {
        format!("{}\n\n{}\n\n{}\n", doc, header, new_content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> SkillStore {
        let conn = Connection::open_in_memory().unwrap();
        super::super::schema::migrate(&conn).unwrap();
        SkillStore::new(Arc::new(Mutex::new(conn)), "test-device".into())
    }

    #[test]
    fn create_and_get_skill() {
        let store = test_store();
        let id = store
            .create("rust-borrow-patterns", Some("coding"), "Rust ownership patterns", "# Rust Borrow Patterns\n\n## Procedure\n...", "agent")
            .unwrap();
        let skill = store.get(&id).unwrap().unwrap();
        assert_eq!(skill.name, "rust-borrow-patterns");
        assert_eq!(skill.category.as_deref(), Some("coding"));
        assert_eq!(skill.version, 1);
    }

    #[test]
    fn search_fts5() {
        let store = test_store();
        store
            .create("hwp-conversion", Some("document"), "HWP conversion pitfalls", "# HWP\n\nUse hwp5html for tables", "agent")
            .unwrap();
        store
            .create("rust-patterns", Some("coding"), "Rust patterns", "# Rust\n\nArc Mutex", "agent")
            .unwrap();
        let results = store.search("HWP", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "hwp-conversion");
    }

    #[test]
    fn record_usage_updates_counts() {
        let store = test_store();
        let id = store
            .create("test-skill", None, "test", "content", "agent")
            .unwrap();
        store.record_usage(&id, true).unwrap();
        store.record_usage(&id, false).unwrap();
        let skill = store.get(&id).unwrap().unwrap();
        assert_eq!(skill.use_count, 2);
        assert_eq!(skill.success_count, 1);
    }

    #[test]
    fn patch_pitfalls_appends() {
        let store = test_store();
        let id = store
            .create("test", None, "d", "# Skill\n\n## Procedure\nDo stuff\n\n## Pitfalls\n- Old pitfall", "agent")
            .unwrap();
        store
            .patch(&id, PatchTarget::Pitfalls, "New pitfall discovered")
            .unwrap();
        let skill = store.get(&id).unwrap().unwrap();
        assert!(skill.content_md.contains("New pitfall discovered"));
        assert!(skill.content_md.contains("Old pitfall"));
        assert_eq!(skill.version, 2);
    }

    #[test]
    fn append_to_section_creates_if_missing() {
        let doc = "# Title\n\nSome content";
        let result = append_to_section(doc, "## Pitfalls", "Watch out");
        assert!(result.contains("## Pitfalls"));
        assert!(result.contains("Watch out"));
    }

    #[test]
    fn create_rejects_empty_name() {
        let store = test_store();
        assert!(store.create("", Some("coding"), "desc", "# body", "agent").is_err());
        assert!(store.create("   ", Some("coding"), "desc", "# body", "agent").is_err());
    }

    #[test]
    fn create_rejects_empty_description_and_content() {
        let store = test_store();
        assert!(store.create("name", None, "", "# body", "agent").is_err());
        assert!(store.create("name", None, "desc", "", "agent").is_err());
        assert!(store.create("name", None, "desc", "   \n  ", "agent").is_err());
    }

    #[test]
    fn create_trims_whitespace_from_name() {
        let store = test_store();
        let id = store
            .create("  spaced-name  ", None, "d", "# body", "agent")
            .unwrap();
        let skill = store.get(&id).unwrap().unwrap();
        assert_eq!(skill.name, "spaced-name");
    }

    #[test]
    fn upsert_from_sync_version_lww() {
        let store = test_store();
        store.upsert_from_sync("s1", "skill1", None, "desc", "v1 content", 1, "agent", "dev-a").unwrap();
        store.upsert_from_sync("s1", "skill1", None, "desc", "v2 content", 2, "agent", "dev-b").unwrap();
        let skill = store.get("s1").unwrap().unwrap();
        assert_eq!(skill.content_md, "v2 content");

        // Lower version should not overwrite
        store.upsert_from_sync("s1", "skill1", None, "desc", "v1 old", 1, "agent", "dev-c").unwrap();
        let skill = store.get("s1").unwrap().unwrap();
        assert_eq!(skill.content_md, "v2 content");
    }
}
