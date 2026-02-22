pub mod pool;

use pool::DbPool;

const MIGRATIONS: &[(&str, &str)] = &[
    ("001_initial", include_str!("migrations/001_initial.sql")),
    (
        "002_resource_snapshots",
        include_str!("migrations/002_resource_snapshots.sql"),
    ),
    (
        "003_pairing_code",
        include_str!("migrations/003_pairing_code.sql"),
    ),
];

pub fn run_migrations(pool: &DbPool) -> anyhow::Result<()> {
    pool.write(|conn| {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS _migrations (
                name TEXT PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
            )",
        )?;

        for (name, sql) in MIGRATIONS {
            let applied: bool = conn.query_row(
                "SELECT COUNT(*) > 0 FROM _migrations WHERE name = ?1",
                [name],
                |row| row.get(0),
            )?;

            if !applied {
                conn.execute_batch(sql)?;
                conn.execute("INSERT INTO _migrations (name) VALUES (?1)", [name])?;
                tracing::info!("applied migration: {}", name);
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path() -> std::path::PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("zcplatform-test-{}.db", uuid::Uuid::new_v4()));
        path
    }

    #[test]
    fn test_migrations_apply_cleanly() {
        let path = temp_db_path();
        let pool = DbPool::open(path.to_str().unwrap(), 1).unwrap();
        run_migrations(&pool).unwrap();

        // Verify tables exist (use reader — file-backed DB shares state)
        pool.read(|conn| {
            let tables: Vec<String> = conn
                .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?
                .query_map([], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect();

            assert!(tables.contains(&"users".to_string()));
            assert!(tables.contains(&"tenants".to_string()));
            assert!(tables.contains(&"tenant_configs".to_string()));
            assert!(tables.contains(&"channels".to_string()));
            assert!(tables.contains(&"members".to_string()));
            assert!(tables.contains(&"audit_log".to_string()));
            assert!(tables.contains(&"usage_metrics".to_string()));
            assert!(tables.contains(&"vault_keys".to_string()));
            assert!(tables.contains(&"otp_tokens".to_string()));
            assert!(tables.contains(&"resource_snapshots".to_string()));
            Ok(())
        })
        .unwrap();

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_migrations_idempotent() {
        let path = temp_db_path();
        let pool = DbPool::open(path.to_str().unwrap(), 1).unwrap();
        run_migrations(&pool).unwrap();
        run_migrations(&pool).unwrap(); // Should not error
        let _ = std::fs::remove_file(&path);
    }
}
