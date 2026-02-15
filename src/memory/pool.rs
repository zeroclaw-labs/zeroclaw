//! High-performance SQLite connection pool for ZeroClaw
//!
//! This module provides a deadpool-based connection pool for SQLite,
//! replacing the single-connection Mutex approach for better concurrency.

use deadpool::managed::{self, Metrics, Pool, RecycleResult};
use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{debug, warn};

/// Connection pool configuration
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum number of connections in the pool
    pub max_size: usize,
    /// Minimum number of connections to maintain
    pub min_idle: usize,
    /// Connection timeout in seconds
    pub connection_timeout_secs: u64,
    /// Max lifetime of a connection in seconds
    pub max_lifetime_secs: u64,
    /// Enable WAL mode for better concurrency
    pub enable_wal: bool,
    /// Busy timeout in milliseconds
    pub busy_timeout_ms: u64,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_size: num_cpus::get() * 2,
            min_idle: 2,
            connection_timeout_secs: 30,
            max_lifetime_secs: 3600,
            enable_wal: true,
            busy_timeout_ms: 5000,
        }
    }
}

/// Manager for SQLite connections in the pool
pub struct SqliteConnectionManager {
    db_path: PathBuf,
    config: PoolConfig,
    init_sql: Arc<Vec<String>>,
}

impl SqliteConnectionManager {
    pub fn new(db_path: PathBuf, config: PoolConfig, init_sql: Vec<String>) -> Self {
        Self {
            db_path,
            config,
            init_sql: Arc::new(init_sql),
        }
    }
}

impl managed::Manager for SqliteConnectionManager {
    type Type = Connection;
    type Error = rusqlite::Error;

    fn create(&self) -> impl std::future::Future<Output = Result<Connection, rusqlite::Error>> + Send {
        let db_path = self.db_path.clone();
        let busy_timeout_ms = self.config.busy_timeout_ms;
        let enable_wal = self.config.enable_wal;
        let init_sql = self.init_sql.clone();

        async move {
            let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_FULL_MUTEX;

            let conn = Connection::open_with_flags(&db_path, flags)?;

            // Configure connection
            conn.busy_timeout(std::time::Duration::from_millis(busy_timeout_ms))?;

            if enable_wal {
                conn.pragma_update(None, "journal_mode", "WAL")?;
                conn.pragma_update(None, "synchronous", "NORMAL")?;
            }

            // Execute initialization SQL
            for sql in init_sql.iter() {
                conn.execute_batch(sql)?;
            }

            debug!("Created new SQLite connection for pool");
            Ok(conn)
        }
    }

    fn recycle(&self, conn: &mut Connection, _: &Metrics) -> impl std::future::Future<Output = RecycleResult<rusqlite::Error>> + Send {
        // Check if connection is still valid
        match conn.execute_batch("SELECT 1") {
            Ok(()) => std::future::ready(Ok(())),
            Err(e) => {
                warn!("Recycling invalid SQLite connection: {}", e);
                std::future::ready(Err(e.into()))
            }
        }
    }
}

/// Pooled SQLite connection wrapper
pub type PooledConnection = managed::Object<SqliteConnectionManager>;

/// SQLite connection pool
#[derive(Clone)]
pub struct SqlitePool {
    inner: Pool<SqliteConnectionManager>,
}

impl SqlitePool {
    /// Create a new connection pool
    pub fn new(
        db_path: PathBuf,
        config: PoolConfig,
        init_sql: Vec<String>,
    ) -> anyhow::Result<Self> {
        let manager = SqliteConnectionManager::new(db_path, config.clone(), init_sql);

        let pool = Pool::builder(manager)
            .max_size(config.max_size)
            .wait_timeout(Some(std::time::Duration::from_secs(
                config.connection_timeout_secs,
            )))
            .runtime(deadpool::Runtime::Tokio1)
            .build()?;

        Ok(Self { inner: pool })
    }

    /// Get a connection from the pool
    pub async fn get(&self) -> anyhow::Result<PooledConnection> {
        let conn = self.inner.get().await?;
        Ok(conn)
    }

    /// Get pool statistics
    pub fn stats(&self) -> PoolStats {
        let status = self.inner.status();
        PoolStats {
            size: status.size,
            available: status.available,
            max_size: status.size,
        }
    }

    /// Execute a closure with a pooled connection
    pub async fn with_connection<F, R>(&self, f: F) -> anyhow::Result<R>
    where
        F: FnOnce(&Connection) -> anyhow::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let conn = self.get().await?;
        // Run the closure in a blocking task since SQLite operations are blocking
        let result = tokio::task::spawn_blocking(move || f(&*conn)).await??;
        Ok(result)
    }
}

/// Pool statistics
#[derive(Debug, Clone)]
pub struct PoolStats {
    pub size: usize,
    pub available: usize,
    pub max_size: usize,
}

impl PoolStats {
    pub fn utilization_pct(&self) -> f64 {
        if self.max_size == 0 {
            0.0
        } else {
            (self.size - self.available) as f64 / self.max_size as f64 * 100.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_pool_creation() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");

        let config = PoolConfig {
            max_size: 4,
            min_idle: 1,
            ..Default::default()
        };

        let pool = SqlitePool::new(db_path, config, vec![]).unwrap();
        let stats = pool.stats();
        assert_eq!(stats.max_size, 4);
    }

    #[tokio::test]
    async fn test_concurrent_connections() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");

        let config = PoolConfig {
            max_size: 4,
            ..Default::default()
        };

        let pool = SqlitePool::new(db_path, config, vec![]).unwrap();

        // Spawn multiple concurrent tasks
        let mut handles = vec![];
        for i in 0..10 {
            let pool = pool.clone();
            let handle = tokio::spawn(async move {
                let _conn = pool.get().await.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                i
            });
            handles.push(handle);
        }

        let results: Vec<i32> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|r| r.unwrap())
            .collect();

        assert_eq!(results.len(), 10);
    }

    #[tokio::test]
    async fn test_with_connection() {
        let tmp = TempDir::new().unwrap();
        let db_path = tmp.path().join("test.db");

        let pool = SqlitePool::new(db_path, PoolConfig::default(), vec![]).unwrap();

        let result: i64 = pool
            .with_connection(|conn| {
                conn.execute("CREATE TABLE test (id INTEGER PRIMARY KEY)", [])?;
                let count: i64 = conn.query_row("SELECT COUNT(*) FROM test", [], |row| row.get(0))?;
                Ok(count)
            })
            .await
            .unwrap();

        assert_eq!(result, 0);
    }
}
