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

    struct Row<'a> {
        id: &'a str,
        category: &'a str,
        content: &'a str,
        importance: f64,
        pinned: bool,
        superseded_by: Option<&'a str>,
    }

    fn create_table(conn: &Connection) {
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
    }

    fn insert_rows(conn: &Connection, rows: &[Row]) {
        for (i, row) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO memories \
                 (id, category, content, importance, created_at, pinned, superseded_by) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    row.id,
                    row.category,
                    row.content,
                    row.importance,
                    format!("2026-01-01T00:00:{i:02}Z"),
                    i64::from(row.pinned),
                    row.superseded_by,
                ],
            )
            .unwrap();
        }
    }

    fn seed(conn: &Connection, rows: &[(&str, &str, f64, bool)]) {
        create_table(conn);
        let rows: Vec<Row> = rows
            .iter()
            .map(|(id, content, importance, pinned)| Row {
                id,
                category: "core",
                content,
                importance: *importance,
                pinned: *pinned,
                superseded_by: None,
            })
            .collect();
        insert_rows(conn, &rows);
    }

    fn count_live(conn: &Connection, category: &str) -> u64 {
        conn.query_row(
            "SELECT COUNT(*) FROM memories WHERE category = ?1 AND superseded_by IS NULL",
            params![category],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn live_bytes(conn: &Connection, category: &str) -> u64 {
        conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(content)), 0) \
             FROM memories WHERE category = ?1 AND superseded_by IS NULL",
            params![category],
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
        assert_eq!(count_live(&conn, "core"), 2, "compacted to the row budget");
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
        assert_eq!(count_live(&conn, "core"), 2);
    }

    #[test]
    fn count_cap_at_exact_budget_is_a_noop() {
        let conn = Connection::open_in_memory().unwrap();
        seed(&conn, &[("a", "x", 0.1, false), ("b", "y", 0.2, false)]);
        let cfg = MemoryConfig {
            core_max_rows: 2,
            ..MemoryConfig::default()
        };

        let report = compact_category_to_budget(&conn, "core", &cfg).unwrap();

        assert_eq!(
            report.evicted_by_count, 0,
            "cap only trims rows strictly over budget"
        );
        assert_eq!(count_live(&conn, "core"), 2);
    }

    #[test]
    fn byte_cap_evicts_lowest_value_until_under_budget() {
        let conn = Connection::open_in_memory().unwrap();
        let body = "x".repeat(100);
        seed(
            &conn,
            &[
                ("a", body.as_str(), 0.1, false),
                ("b", body.as_str(), 0.2, false),
                ("c", body.as_str(), 0.3, false),
                ("d", body.as_str(), 0.4, false),
            ],
        );
        let cfg = MemoryConfig {
            core_max_bytes: 250,
            ..MemoryConfig::default()
        };

        let report = compact_category_to_budget(&conn, "core", &cfg).unwrap();

        assert_eq!(
            report.evicted_by_count, 0,
            "no row cap configured, nothing evicted by count"
        );
        assert_eq!(
            report.evicted_by_bytes, 2,
            "evicts one row at a time until under the byte budget"
        );
        assert!(live_bytes(&conn, "core") <= 250);
        assert!(!alive(&conn, "a"), "lowest-value row evicted first");
        assert!(!alive(&conn, "b"), "second-lowest-value row evicted next");
        assert!(alive(&conn, "c"));
        assert!(alive(&conn, "d"));
    }

    #[test]
    fn byte_cap_terminates_when_only_pinned_rows_remain() {
        let conn = Connection::open_in_memory().unwrap();
        let body = "x".repeat(100);
        seed(
            &conn,
            &[
                ("a", body.as_str(), 0.1, false),
                ("b", body.as_str(), 0.2, false),
                ("p", body.as_str(), 0.05, true),
            ],
        );
        let cfg = MemoryConfig {
            core_max_bytes: 50,
            ..MemoryConfig::default()
        };

        // The pinned row alone exceeds the byte budget; the eviction loop
        // must stop once no evictable row remains instead of spinning.
        let report = compact_category_to_budget(&conn, "core", &cfg).unwrap();

        assert_eq!(report.evicted_by_bytes, 2, "both non-pinned rows evicted");
        assert_eq!(report.pinned_skipped, 1);
        assert!(alive(&conn, "p"), "pinned row survives over-budget");
        assert_eq!(live_bytes(&conn, "core"), 100);
    }

    #[test]
    fn oldest_evict_order_ignores_importance() {
        let conn = Connection::open_in_memory().unwrap();
        seed(
            &conn,
            &[
                ("oldest", "first", 0.9, false),
                ("mid", "second", 0.2, false),
                ("newest", "third", 0.1, false),
            ],
        );
        let cfg = MemoryConfig {
            core_max_rows: 2,
            evict_order: MemoryEvictOrder::Oldest,
            ..MemoryConfig::default()
        };

        let report = compact_category_to_budget(&conn, "core", &cfg).unwrap();

        assert_eq!(report.evicted_by_count, 1);
        assert!(
            !alive(&conn, "oldest"),
            "oldest row evicted despite highest importance"
        );
        assert!(alive(&conn, "mid"));
        assert!(alive(&conn, "newest"));
    }

    #[test]
    fn superseded_rows_do_not_count_toward_the_row_cap() {
        let conn = Connection::open_in_memory().unwrap();
        create_table(&conn);
        insert_rows(
            &conn,
            &[
                Row {
                    id: "hidden",
                    category: "core",
                    content: "old fact",
                    importance: 0.1,
                    pinned: false,
                    superseded_by: Some("live_a"),
                },
                Row {
                    id: "live_a",
                    category: "core",
                    content: "new fact",
                    importance: 0.2,
                    pinned: false,
                    superseded_by: None,
                },
                Row {
                    id: "live_b",
                    category: "core",
                    content: "other fact",
                    importance: 0.3,
                    pinned: false,
                    superseded_by: None,
                },
            ],
        );
        let cfg = MemoryConfig {
            core_max_rows: 2,
            ..MemoryConfig::default()
        };

        let report = compact_category_to_budget(&conn, "core", &cfg).unwrap();

        assert_eq!(
            report.evicted_by_count, 0,
            "two live rows fit the cap; the superseded row is not counted"
        );
        assert!(
            alive(&conn, "hidden"),
            "superseded rows are never budget-evicted"
        );
        assert!(alive(&conn, "live_a"));
        assert!(alive(&conn, "live_b"));
    }

    #[test]
    fn daily_category_uses_daily_row_cap_only() {
        let conn = Connection::open_in_memory().unwrap();
        create_table(&conn);
        insert_rows(
            &conn,
            &[
                Row {
                    id: "d1",
                    category: "daily",
                    content: "low",
                    importance: 0.1,
                    pinned: false,
                    superseded_by: None,
                },
                Row {
                    id: "d2",
                    category: "daily",
                    content: "mid",
                    importance: 0.2,
                    pinned: false,
                    superseded_by: None,
                },
                Row {
                    id: "d3",
                    category: "daily",
                    content: "top",
                    importance: 0.3,
                    pinned: false,
                    superseded_by: None,
                },
            ],
        );
        // core_max_bytes must not leak into the daily category: byte caps
        // apply only to core.
        let cfg = MemoryConfig {
            daily_max_rows: 1,
            core_max_bytes: 1,
            ..MemoryConfig::default()
        };

        let report = compact_category_to_budget(&conn, "daily", &cfg).unwrap();

        assert_eq!(report.evicted_by_count, 2);
        assert_eq!(report.evicted_by_bytes, 0, "byte cap does not apply here");
        assert!(!alive(&conn, "d1"));
        assert!(!alive(&conn, "d2"));
        assert!(alive(&conn, "d3"), "highest-value daily row retained");
    }

    #[test]
    fn unrecognized_category_is_a_noop() {
        let conn = Connection::open_in_memory().unwrap();
        create_table(&conn);
        insert_rows(
            &conn,
            &[Row {
                id: "c1",
                category: "conversation",
                content: "chatter",
                importance: 0.1,
                pinned: false,
                superseded_by: None,
            }],
        );
        let cfg = MemoryConfig {
            core_max_rows: 1,
            core_max_bytes: 1,
            daily_max_rows: 1,
            ..MemoryConfig::default()
        };

        let report = compact_category_to_budget(&conn, "conversation", &cfg).unwrap();

        assert_eq!(
            report,
            EvictionReport::default(),
            "budget caps only cover core and daily"
        );
        assert!(alive(&conn, "c1"));
    }
}
