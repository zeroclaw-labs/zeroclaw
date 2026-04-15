// Categories module (v3.0)
//
// 9 immutable Seed Categories + user-created Custom Categories.
// Custom categories are stored in `user_categories` table (created in S2 migration).

pub mod seed;

pub use seed::{SeedCategory, SeedCategoryInfo, SEED_CATEGORIES};

use anyhow::{bail, Result};
use rusqlite::params;
use serde::{Deserialize, Serialize};

/// A custom (user-created) category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomCategory {
    pub uuid: String,
    pub name: String,
    pub icon: Option<String>,
    /// If set, this custom category is a child of the given seed key.
    pub parent_seed_key: Option<String>,
    pub order_index: i32,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Unified category reference — either a Seed or Custom category.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CategoryRef {
    Seed { key: String },
    Custom { uuid: String },
}

impl CategoryRef {
    pub fn seed(key: &str) -> Self {
        Self::Seed {
            key: key.to_string(),
        }
    }

    pub fn custom(uuid: &str) -> Self {
        Self::Custom {
            uuid: uuid.to_string(),
        }
    }

    /// Check if this refers to a known seed category.
    pub fn is_seed(&self) -> bool {
        matches!(self, Self::Seed { .. })
    }
}

/// Maximum number of custom categories per user (spam prevention).
const MAX_CUSTOM_CATEGORIES: usize = 100;

/// Custom category CRUD operations on the SQLite `user_categories` table.
///
/// These are standalone functions that take a connection lock, keeping
/// SqliteMemory's public surface focused on memory operations.
pub struct CategoryStore;

impl CategoryStore {
    /// Create a new custom category. Returns the generated UUID.
    pub fn create(
        conn: &rusqlite::Connection,
        name: &str,
        icon: Option<&str>,
        parent_seed_key: Option<&str>,
    ) -> Result<String> {
        // Validate: name not empty
        if name.trim().is_empty() {
            bail!("Category name cannot be empty");
        }

        // Validate: parent_seed_key must be a known seed if provided
        if let Some(key) = parent_seed_key {
            if !SeedCategory::is_seed_key(key) {
                bail!("Unknown parent seed key: {key}");
            }
        }

        // Validate: name must not conflict with seed category names/keys
        let lower_name = name.trim().to_lowercase();
        for info in SEED_CATEGORIES {
            if info.key == lower_name || info.name_ko == name.trim() {
                bail!("Category name conflicts with seed category: {}", info.key);
            }
        }

        // Validate: max count
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM user_categories",
            [],
            |r| r.get(0),
        )?;
        if count as usize >= MAX_CUSTOM_CATEGORIES {
            bail!("Maximum of {MAX_CUSTOM_CATEGORIES} custom categories reached");
        }

        // Get next order_index
        let max_order: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(order_index), 0) FROM user_categories",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let uuid = uuid::Uuid::new_v4().to_string();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        conn.execute(
            "INSERT INTO user_categories (uuid, name, icon, parent_seed_key, order_index, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                uuid,
                name.trim(),
                icon,
                parent_seed_key,
                max_order + 1,
                now,
                now,
            ],
        )?;

        Ok(uuid)
    }

    /// List all custom categories, ordered by parent_seed_key then order_index.
    pub fn list(conn: &rusqlite::Connection) -> Result<Vec<CustomCategory>> {
        let mut stmt = conn.prepare(
            "SELECT uuid, name, icon, parent_seed_key, order_index, created_at, updated_at
             FROM user_categories
             ORDER BY parent_seed_key NULLS FIRST, order_index",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(CustomCategory {
                uuid: row.get(0)?,
                name: row.get(1)?,
                icon: row.get(2)?,
                parent_seed_key: row.get(3)?,
                order_index: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Delete a custom category by UUID.
    /// Returns true if the category was found and deleted.
    pub fn delete(conn: &rusqlite::Connection, uuid: &str) -> Result<bool> {
        let affected = conn.execute(
            "DELETE FROM user_categories WHERE uuid = ?1",
            params![uuid],
        )?;
        Ok(affected > 0)
    }

    /// Update a custom category's order_index.
    pub fn reorder(
        conn: &rusqlite::Connection,
        uuid: &str,
        new_order: i32,
    ) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let affected = conn.execute(
            "UPDATE user_categories SET order_index = ?1, updated_at = ?2 WHERE uuid = ?3",
            params![new_order, now, uuid],
        )?;
        if affected == 0 {
            bail!("Custom category not found: {uuid}");
        }
        Ok(())
    }

    /// Rename a custom category.
    pub fn rename(
        conn: &rusqlite::Connection,
        uuid: &str,
        new_name: &str,
    ) -> Result<()> {
        if new_name.trim().is_empty() {
            bail!("Category name cannot be empty");
        }
        // Check seed conflicts
        let lower = new_name.trim().to_lowercase();
        for info in SEED_CATEGORIES {
            if info.key == lower || info.name_ko == new_name.trim() {
                bail!("Name conflicts with seed category: {}", info.key);
            }
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let affected = conn.execute(
            "UPDATE user_categories SET name = ?1, updated_at = ?2 WHERE uuid = ?3",
            params![new_name.trim(), now, uuid],
        )?;
        if affected == 0 {
            bail!("Custom category not found: {uuid}");
        }
        Ok(())
    }

    /// Get a custom category by UUID.
    pub fn get(conn: &rusqlite::Connection, uuid: &str) -> Result<Option<CustomCategory>> {
        let mut stmt = conn.prepare(
            "SELECT uuid, name, icon, parent_seed_key, order_index, created_at, updated_at
             FROM user_categories WHERE uuid = ?1",
        )?;
        let result = stmt.query_row(params![uuid], |row| {
            Ok(CustomCategory {
                uuid: row.get(0)?,
                name: row.get(1)?,
                icon: row.get(2)?,
                parent_seed_key: row.get(3)?,
                order_index: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            })
        });
        match result {
            Ok(cat) => Ok(Some(cat)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::SqliteMemory;
    use tempfile::TempDir;

    fn setup() -> (TempDir, SqliteMemory) {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        (tmp, mem)
    }

    #[test]
    fn create_and_list_custom_category() {
        let (_tmp, mem) = setup();
        let conn = mem.conn_for_test();
        let uuid = CategoryStore::create(&conn, "건강관리", Some("\u{1F3CB}"), Some("daily")).unwrap();
        assert!(!uuid.is_empty());

        let cats = CategoryStore::list(&conn).unwrap();
        assert_eq!(cats.len(), 1);
        assert_eq!(cats[0].name, "건강관리");
        assert_eq!(cats[0].parent_seed_key.as_deref(), Some("daily"));
    }

    #[test]
    fn create_top_level_custom_category() {
        let (_tmp, mem) = setup();
        let conn = mem.conn_for_test();
        let uuid = CategoryStore::create(&conn, "부동산", None, None).unwrap();
        let cat = CategoryStore::get(&conn, &uuid).unwrap().unwrap();
        assert!(cat.parent_seed_key.is_none());
    }

    #[test]
    fn reject_empty_name() {
        let (_tmp, mem) = setup();
        let conn = mem.conn_for_test();
        assert!(CategoryStore::create(&conn, "", None, None).is_err());
        assert!(CategoryStore::create(&conn, "   ", None, None).is_err());
    }

    #[test]
    fn reject_seed_name_conflict() {
        let (_tmp, mem) = setup();
        let conn = mem.conn_for_test();
        // Exact seed key
        assert!(CategoryStore::create(&conn, "daily", None, None).is_err());
        // Korean seed name
        assert!(CategoryStore::create(&conn, "일상업무", None, None).is_err());
    }

    #[test]
    fn reject_unknown_parent_seed() {
        let (_tmp, mem) = setup();
        let conn = mem.conn_for_test();
        assert!(CategoryStore::create(&conn, "test", None, Some("nonexistent")).is_err());
    }

    #[test]
    fn delete_custom_category() {
        let (_tmp, mem) = setup();
        let conn = mem.conn_for_test();
        let uuid = CategoryStore::create(&conn, "temp", None, None).unwrap();
        assert!(CategoryStore::delete(&conn, &uuid).unwrap());
        assert!(CategoryStore::list(&conn).unwrap().is_empty());
    }

    #[test]
    fn delete_nonexistent_returns_false() {
        let (_tmp, mem) = setup();
        let conn = mem.conn_for_test();
        assert!(!CategoryStore::delete(&conn, "fake-uuid").unwrap());
    }

    #[test]
    fn rename_custom_category() {
        let (_tmp, mem) = setup();
        let conn = mem.conn_for_test();
        let uuid = CategoryStore::create(&conn, "old name", None, None).unwrap();
        CategoryStore::rename(&conn, &uuid, "new name").unwrap();
        let cat = CategoryStore::get(&conn, &uuid).unwrap().unwrap();
        assert_eq!(cat.name, "new name");
    }

    #[test]
    fn rename_rejects_seed_conflict() {
        let (_tmp, mem) = setup();
        let conn = mem.conn_for_test();
        let uuid = CategoryStore::create(&conn, "test", None, None).unwrap();
        assert!(CategoryStore::rename(&conn, &uuid, "phone").is_err());
    }

    #[test]
    fn reorder_custom_category() {
        let (_tmp, mem) = setup();
        let conn = mem.conn_for_test();
        let uuid = CategoryStore::create(&conn, "cat1", None, None).unwrap();
        CategoryStore::reorder(&conn, &uuid, 99).unwrap();
        let cat = CategoryStore::get(&conn, &uuid).unwrap().unwrap();
        assert_eq!(cat.order_index, 99);
    }

    #[test]
    fn order_index_auto_increments() {
        let (_tmp, mem) = setup();
        let conn = mem.conn_for_test();
        CategoryStore::create(&conn, "cat1", None, None).unwrap();
        CategoryStore::create(&conn, "cat2", None, None).unwrap();
        let cats = CategoryStore::list(&conn).unwrap();
        assert!(cats[0].order_index < cats[1].order_index);
    }

    #[test]
    fn category_ref_seed_and_custom() {
        let seed = CategoryRef::seed("daily");
        assert!(seed.is_seed());

        let custom = CategoryRef::custom("some-uuid");
        assert!(!custom.is_seed());
    }
}
