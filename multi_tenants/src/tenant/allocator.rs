use anyhow::{bail, Result};

/// Find the first available port in [range[0], range[1]] not already used by any tenant.
/// Must be called inside a `db.write()` closure to avoid TOCTOU races.
pub fn allocate_port(conn: &rusqlite::Connection, range: [u16; 2]) -> Result<u16> {
    let mut stmt = conn.prepare("SELECT port FROM tenants")?;
    let used_ports: Vec<u16> = stmt
        .query_map([], |row| row.get::<_, i64>(0))?
        .filter_map(|r| r.ok())
        .map(|p| p as u16)
        .collect();

    for port in range[0]..=range[1] {
        if !used_ports.contains(&port) {
            return Ok(port);
        }
    }

    bail!("no available port in range {}..={}", range[0], range[1])
}

/// Find the first available UID in [range[0], range[1]] not already used by any tenant.
/// Must be called inside a `db.write()` closure to avoid TOCTOU races.
pub fn allocate_uid(conn: &rusqlite::Connection, range: [u32; 2]) -> Result<u32> {
    let mut stmt = conn.prepare("SELECT uid FROM tenants")?;
    let used_uids: Vec<u32> = stmt
        .query_map([], |row| row.get::<_, i64>(0))?
        .filter_map(|r| r.ok())
        .map(|u| u as u32)
        .collect();

    for uid in range[0]..=range[1] {
        if !used_uids.contains(&uid) {
            return Ok(uid);
        }
    }

    bail!("no available uid in range {}..={}", range[0], range[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::pool::DbPool;

    fn test_db() -> (DbPool, String) {
        let tmp_path = std::env::temp_dir()
            .join(format!("zctest-{}.db", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .to_string();
        let db = DbPool::open(&tmp_path, 1).unwrap();
        db.write(|conn| {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS tenants (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    slug TEXT UNIQUE NOT NULL,
                    status TEXT NOT NULL DEFAULT 'stopped',
                    plan TEXT NOT NULL DEFAULT 'free',
                    port INTEGER NOT NULL,
                    uid INTEGER NOT NULL,
                    container_id TEXT,
                    created_at TEXT DEFAULT (datetime('now'))
                );",
            )?;
            Ok(())
        })
        .unwrap();
        (db, tmp_path)
    }

    fn insert_tenant(db: &DbPool, id: &str, slug: &str, port: u16, uid: u32) {
        db.write(|conn| {
            conn.execute(
                "INSERT INTO tenants (id, name, slug, port, uid) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, slug, slug, port as i64, uid as i64],
            )?;
            Ok(())
        })
        .unwrap();
    }

    // --- Port tests ---

    #[test]
    fn test_allocate_port_returns_first_available() {
        let (db, path) = test_db();
        let port = db.write(|conn| allocate_port(conn, [9000, 9010])).unwrap();
        assert_eq!(port, 9000);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_allocate_port_skips_used() {
        let (db, path) = test_db();
        insert_tenant(&db, "t1", "tenant-a", 9000, 2000);
        insert_tenant(&db, "t2", "tenant-b", 9001, 2001);
        let port = db.write(|conn| allocate_port(conn, [9000, 9010])).unwrap();
        assert_eq!(port, 9002);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_allocate_port_exhausted_returns_error() {
        let (db, path) = test_db();
        // Fill all ports in tiny range
        for i in 0u16..=2 {
            insert_tenant(
                &db,
                &format!("t{}", i),
                &format!("tenant-{}", i),
                9000 + i,
                2000 + i as u32,
            );
        }
        let result = db.write(|conn| allocate_port(conn, [9000, 9002]));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no available port"));
        let _ = std::fs::remove_file(&path);
    }

    // --- UID tests ---

    #[test]
    fn test_allocate_uid_returns_first_available() {
        let (db, path) = test_db();
        let uid = db.write(|conn| allocate_uid(conn, [10000, 10010])).unwrap();
        assert_eq!(uid, 10000);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_allocate_uid_skips_used() {
        let (db, path) = test_db();
        insert_tenant(&db, "t1", "tenant-a", 9000, 10000);
        insert_tenant(&db, "t2", "tenant-b", 9001, 10001);
        let uid = db.write(|conn| allocate_uid(conn, [10000, 10010])).unwrap();
        assert_eq!(uid, 10002);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_allocate_uid_exhausted_returns_error() {
        let (db, path) = test_db();
        for i in 0u32..=2 {
            insert_tenant(
                &db,
                &format!("t{}", i),
                &format!("tenant-{}", i),
                9000 + i as u16,
                10000 + i,
            );
        }
        let result = db.write(|conn| allocate_uid(conn, [10000, 10002]));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("no available uid"));
        let _ = std::fs::remove_file(&path);
    }
}
