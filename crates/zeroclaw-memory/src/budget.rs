use anyhow::Result;
use rusqlite::{Connection, params};
use zeroclaw_config::schema::{MemoryConfig, MemoryEvictOrder};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvictionReport {
    pub evicted_by_count: u64,
    pub evicted_by_bytes: u64,
    pub pinned_skipped: u64,
}

pub fn compact_category_to_budget(
    conn: &Connection,
    category: &str,
    cfg: &MemoryConfig,
) -> Result<EvictionReport> {
    let max_rows = match category {
        "core" => cfg.core_max_rows,
        "daily" => cfg.daily_max_rows,
        _ => 0,
    };
    let max_bytes = match category {
        "core" => cfg.core_max_bytes,
        _ => 0,
    };
    if max_rows == 0 && max_bytes == 0 {
        return Ok(EvictionReport::default());
    }

    let pinned_skipped = conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE category = ?1 AND pinned = 1",
        params![category],
        |row| row.get::<_, u64>(0),
    )?;

    let mut report = EvictionReport {
        pinned_skipped,
        ..EvictionReport::default()
    };

    if max_rows > 0 {
        let current = conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE category = ?1 AND superseded_by IS NULL",
            params![category],
            |row| row.get::<_, u64>(0),
        )?;
        if current > max_rows {
            let excess = current - max_rows;
            let order = match cfg.evict_order {
                MemoryEvictOrder::Value => "importance ASC, created_at ASC",
                MemoryEvictOrder::Oldest => "created_at ASC",
            };
            let sql = format!(
                "DELETE FROM memories WHERE id IN (
                    SELECT id FROM memories
                    WHERE category = ?1 AND superseded_by IS NULL AND pinned = 0
                    ORDER BY {order}
                    LIMIT ?2
                )"
            );
            let affected = conn.execute(&sql, params![category, excess])?;
            report.evicted_by_count = u64::try_from(affected).unwrap_or(0);
        }
    }

    if max_bytes > 0 {
        loop {
            let current_bytes = conn.query_row(
                "SELECT COALESCE(SUM(LENGTH(content)), 0)
                 FROM memories
                 WHERE category = ?1 AND superseded_by IS NULL",
                params![category],
                |row| row.get::<_, u64>(0),
            )?;
            if current_bytes <= max_bytes {
                break;
            }
            let order = match cfg.evict_order {
                MemoryEvictOrder::Value => "importance ASC, created_at ASC",
                MemoryEvictOrder::Oldest => "created_at ASC",
            };
            let sql = format!(
                "DELETE FROM memories WHERE id = (
                    SELECT id FROM memories
                    WHERE category = ?1 AND superseded_by IS NULL AND pinned = 0
                    ORDER BY {order}
                    LIMIT 1
                )"
            );
            let affected = conn.execute(&sql, params![category])?;
            if affected == 0 {
                break;
            }
            report.evicted_by_bytes += u64::try_from(affected).unwrap_or(0);
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::MemoryEvictOrder;

    fn seed(conn: &Connection, rows: &[(&str, &str, f64, bool)]) {
        conn.execute_batch(
            "CREATE TABLE memories (
                id TEXT PRIMARY KEY,
                category TEXT NOT NULL,
                content TEXT NOT NULL,
                importance REAL,
                created_at TEXT NOT NULL,
                pinned INTEGER NOT NULL DEFAULT 0,
                superseded_by TEXT
            );",
        )
        .unwrap();
        for (i, (id, content, imp, pinned)) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO memories \
                 (id, category, content, importance, created_at, pinned, superseded_by) \
                 VALUES (?1, 'core', ?2, ?3, ?4, ?5, NULL)",
                params![
                    id,
                    content,
                    imp,
                    format!("2026-01-01T00:00:{i:02}Z"),
                    *pinned as i64
                ],
            )
            .unwrap();
        }
    }

    fn core_count(conn: &Connection) -> u64 {
        conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE category = 'core' AND superseded_by IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn alive(conn: &Connection, id: &str) -> bool {
        conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE id = ?1",
            params![id],
            |r| r.get::<_, u64>(0),
        )
        .unwrap()
            == 1
    }

    #[test]
    fn budget_evicts_lowest_value_rows_and_protects_pinned() {
        let conn = Connection::open_in_memory().unwrap();
        seed(
            &conn,
            &[
                ("a", "low", 0.1, false),
                ("b", "mid", 0.2, false),
                ("c", "high", 0.3, false),
                ("d", "top", 0.4, false),
                ("p", "pinned-low", 0.05, true),
            ],
        );
        let cfg = MemoryConfig {
            core_max_rows: 2,
            evict_order: MemoryEvictOrder::Value,
            ..MemoryConfig::default()
        };

        let report = compact_category_to_budget(&conn, "core", &cfg).unwrap();

        assert_eq!(
            report.evicted_by_count, 3,
            "three lowest-value non-pinned evicted"
        );
        assert_eq!(report.pinned_skipped, 1);
        assert_eq!(core_count(&conn), 2, "compacted to the row budget");
        assert!(
            alive(&conn, "p"),
            "pinned row survives despite lowest value"
        );
        assert!(alive(&conn, "d"), "highest-value row retained");
        assert!(!alive(&conn, "a"), "lowest-value row evicted");
    }

    #[test]
    fn budget_unbounded_by_default_is_a_noop() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn, &[("a", "x", 0.1, false), ("b", "y", 0.2, false)]);
        let report = compact_category_to_budget(&conn, "core", &MemoryConfig::default()).unwrap();
        assert_eq!(
            report,
            EvictionReport::default(),
            "caps=0 means no eviction"
        );
        assert_eq!(core_count(&conn), 2);
    }
}
