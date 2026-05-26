use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::Connection;
use std::sync::Arc;

pub struct LeaderElection {
    db: Arc<Mutex<Connection>>,
    node_id: String,
    lease_duration_secs: i64,
}

impl LeaderElection {
    pub fn new(
        db: Arc<Mutex<Connection>>,
        node_id: &str,
        lease_duration_secs: i64,
    ) -> Result<Self> {
        {
            let conn = db.lock();
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS leader_lease (
                    id TEXT PRIMARY KEY DEFAULT 'singleton',
                    holder TEXT NOT NULL,
                    acquired_at TEXT NOT NULL,
                    expires_at TEXT NOT NULL
                );",
            )?;
        }
        Ok(Self {
            db,
            node_id: node_id.to_string(),
            lease_duration_secs,
        })
    }

    pub fn try_acquire(&self) -> Result<bool> {
        let db = self.db.lock();
        let now = chrono::Utc::now();
        let now_str = now.format("%Y-%m-%d %H:%M:%S").to_string();
        let expires = (now + chrono::Duration::seconds(self.lease_duration_secs))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        let changed = db.execute(
            "INSERT INTO leader_lease (id, holder, acquired_at, expires_at)
             VALUES ('singleton', ?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET
               holder = ?1, acquired_at = ?2, expires_at = ?3
             WHERE holder = ?1 OR expires_at < ?2",
            rusqlite::params![self.node_id, now_str, expires],
        )?;
        Ok(changed > 0)
    }

    pub fn renew(&self) -> Result<bool> {
        let db = self.db.lock();
        let now = chrono::Utc::now();
        let now_str = now.format("%Y-%m-%d %H:%M:%S").to_string();
        let expires = (now + chrono::Duration::seconds(self.lease_duration_secs))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();

        let changed = db.execute(
            "UPDATE leader_lease SET acquired_at = ?1, expires_at = ?2 WHERE id = 'singleton' AND holder = ?3",
            rusqlite::params![now_str, expires, self.node_id],
        )?;
        Ok(changed > 0)
    }

    pub fn release(&self) -> Result<()> {
        let db = self.db.lock();
        db.execute(
            "DELETE FROM leader_lease WHERE id = 'singleton' AND holder = ?1",
            rusqlite::params![self.node_id],
        )?;
        Ok(())
    }

    pub fn current_leader(&self) -> Result<Option<String>> {
        let db = self.db.lock();
        let now_str = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let mut stmt = db.prepare(
            "SELECT holder FROM leader_lease WHERE id = 'singleton' AND expires_at >= ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![now_str], |row| row.get(0))?;
        match rows.next() {
            Some(Ok(holder)) => Ok(Some(holder)),
            _ => Ok(None),
        }
    }

    pub fn is_leader(&self) -> bool {
        self.current_leader()
            .ok()
            .flatten()
            .map(|h| h == self.node_id)
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Arc<Mutex<Connection>> {
        Arc::new(Mutex::new(Connection::open_in_memory().unwrap()))
    }

    #[test]
    fn acquire_and_check() {
        let db = test_db();
        let le = LeaderElection::new(db.clone(), "zeroclaw_node_1", 60).unwrap();
        assert!(le.try_acquire().unwrap());
        assert!(le.is_leader());
        assert_eq!(
            le.current_leader().unwrap(),
            Some("zeroclaw_node_1".to_string())
        );
    }

    #[test]
    fn second_node_cannot_take_active_lease() {
        let db = test_db();
        let le1 = LeaderElection::new(db.clone(), "zeroclaw_node_1", 60).unwrap();
        let le2 = LeaderElection::new(db.clone(), "zeroclaw_node_2", 60).unwrap();
        assert!(le1.try_acquire().unwrap());
        assert!(!le2.try_acquire().unwrap());
        assert!(le1.is_leader());
        assert!(!le2.is_leader());
    }

    #[test]
    fn release_allows_new_leader() {
        let db = test_db();
        let le1 = LeaderElection::new(db.clone(), "zeroclaw_node_1", 60).unwrap();
        let le2 = LeaderElection::new(db.clone(), "zeroclaw_node_2", 60).unwrap();
        le1.try_acquire().unwrap();
        le1.release().unwrap();
        assert!(le2.try_acquire().unwrap());
        assert!(le2.is_leader());
    }

    #[test]
    fn renew_extends_lease() {
        let db = test_db();
        let le = LeaderElection::new(db.clone(), "zeroclaw_node_1", 60).unwrap();
        le.try_acquire().unwrap();
        assert!(le.renew().unwrap());
    }
}
