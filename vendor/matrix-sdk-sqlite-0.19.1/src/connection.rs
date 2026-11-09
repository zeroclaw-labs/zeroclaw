// Copyright 2025 The Matrix.org Foundation C.I.C.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! An implementation of `deadpool` for `rusqlite`.
//!
//! Initially, we were using `deadpool-sqlite`, that is also using `rusqlite` as
//! the SQLite interface. However, in the implementation of
//! [`deadpool::managed::Manager`], when recycling an object (i.e. an SQLite
//! connection), [a SQL query is run to detect whether the connection is still
//! alive][connection-test]. It creates performance issues:
//!
//! 1. It runs a prepared SQL query, which has a non-negligle cost. Imagine each
//!    connection is used to run on average one query; when recycled, a second
//!    query was constantly run. Even if it's a simple query, it requires to
//!    prepare a statement, to run and to query it.
//! 2. The SQL query was run in a blocking task. Indeed,
//!    `deadpool_runtime::spawn_blocking` is used (via
//!    `deadpool_sync::SyncWrapper::interact`), which includes [blocking the
//!    thread, acquiring a lock][spawn_blocking] etc. All this has more
//!    performance cost.
//!
//! Measures have shown it is a performance bottleneck for us, especially on
//! Android. Why specifically on Android and not other systems? This is still
//! unclear at the time of writing (2025-11-11), despites having spent several
//! days digging and trying to find an answer to this question.
//!
//! We have tried to use another approach to test the aliveness of the
//! connections without running queries. It has involved patching `rusqlite` to
//! add more bindings to SQLite, and patching `deadpool` itself, but without any
//! successful results.
//!
//! Finally, we have started questioning the reason of this test: why testing
//! whether the connection was still alive? After all, there is no reason a
//! connection should die in our case:
//!
//! - all connections are local,
//! - all interactions are behind [WAL], which is local only,
//! - even if for an unknown reason the connection died, using it next time
//!   would create an error… exactly what would happen when recycling the
//!   connection.
//!
//! Consequently, we have created a new implementation of `deadpool` for
//! `rusqlite` that doesn't test the aliveness of the connections when recycled.
//! We assume they are all alive.
//!
//! This implementation is, at the time of writing (2025-11-11):
//!
//! - 3.5 times faster on Android than `deadpool-sqlite`, removing the lock and
//!   thread contention entirely,
//! - 2 times faster on iOS.
//!
//! [connection-test]: https://github.com/deadpool-rs/deadpool/blob/d6f7d58756f0cc7bdd1f3d54d820c1332d67e4d5/crates/deadpool-sqlite/src/lib.rs#L80-L100
//! [spawn_blocking]: https://github.com/deadpool-rs/deadpool/blob/d6f7d58756f0cc7bdd1f3d54d820c1332d67e4d5/crates/deadpool-sync/src/lib.rs#L113-L131
//! [WAL]: https://www.sqlite.org/wal.html

use std::{convert::Infallible, path::PathBuf, sync::Arc, time::Duration};

pub use deadpool::managed::reexports::*;
use deadpool::managed::{self, Metrics, PoolConfig, RecycleError};
use deadpool_sync::SyncWrapper;
use tokio::sync::Mutex;
use tracing::{info, warn};

/// The default runtime used by `matrix-sdk-sqlite` for `deadpool`.
pub const RUNTIME: Runtime = Runtime::Tokio1;

deadpool::managed_reexports!(
    "matrix-sdk-sqlite",
    Manager,
    managed::Object<Manager>,
    rusqlite::Error,
    Infallible
);

/// Type representing a connection to SQLite from the [`Pool`].
pub type Connection = Object;

/// [`Manager`][managed::Manager] for creating and recycling SQLite
/// [`Connection`]s.
#[derive(Debug)]
pub struct Manager {
    pub(crate) database_path: PathBuf,
}

impl Manager {
    /// Creates a new [`Manager`] for a database.
    #[must_use]
    pub fn new(database_path: PathBuf) -> Self {
        Self { database_path }
    }
}

impl managed::Manager for Manager {
    type Type = SyncWrapper<rusqlite::Connection>;
    type Error = rusqlite::Error;

    async fn create(&self) -> Result<Self::Type, Self::Error> {
        let path = self.database_path.clone();
        SyncWrapper::new(RUNTIME, move || rusqlite::Connection::open(path)).await
    }

    async fn recycle(
        &self,
        conn: &mut Self::Type,
        _: &Metrics,
    ) -> managed::RecycleResult<Self::Error> {
        if conn.is_mutex_poisoned() {
            return Err(RecycleError::Message(
                "Mutex is poisoned. Connection is considered unusable.".into(),
            ));
        }

        Ok(())
    }
}

/// Gracefully close the store-owned write connection.
///
/// Callers are expected to remove the enclosing [`SqliteConnections`] from the
/// store before calling this helper so no new write acquisitions can happen
/// through the store API.
///
/// 1. Waits for any in-flight write to complete by acquiring the lock.
/// 2. Runs a WAL checkpoint (TRUNCATE) to flush pending data to the main
///    database file and release WAL locks.
/// 3. Drops the store-owned `Arc` on a blocking thread.
///
/// This is still best effort: if another cloned `Arc` or an
/// `OwnedMutexGuard<Connection>` is still alive elsewhere, the underlying
/// SQLite connection may remain alive until that handle is dropped.
pub async fn close_connection(write_connection: Arc<Mutex<Connection>>) {
    // Acquire the lock to wait for any in-flight write to complete.
    let guard = write_connection.lock().await;

    // Flush WAL and release locks while we still own the connection.
    let _ = guard
        .interact(|raw| {
            raw.execute_batch("PRAGMA locking_mode = NORMAL; PRAGMA wal_checkpoint(TRUNCATE);")
                .ok();
        })
        .await;

    drop(guard);

    // Drop the store-owned Arc on a blocking thread.
    let _ = tokio::task::spawn_blocking(move || drop(write_connection)).await;
}

/// Live database connections held by each SQLite store.
///
/// Wrapped in `Option<SqliteConnections>` guarded by a `Mutex` inside each
/// store; `Some` means the store is active, `None` means it is closed.
pub(crate) struct SqliteConnections {
    /// The pool of read connections.
    pub pool: Pool,
    /// The dedicated write connection.
    ///
    /// This lives behind `Arc<Mutex<_>>` so stores can clone the `Arc` and
    /// obtain an `OwnedMutexGuard<Connection>` without holding the outer
    /// `connections` mutex across await points.
    pub write_connection: Arc<Mutex<Connection>>,
}

/// Close a store by taking its connections out.
///
/// After this returns, any new call to `read()` or `write()` through the
/// store will fail with [`crate::error::Error::StoreClosed`] until
/// [`reopen_connections`] is called.
///
/// Idempotent: if the store is already closed this is a no-op.
pub(crate) async fn close_connections(connections: &Mutex<Option<SqliteConnections>>, label: &str) {
    let mut guard = connections.lock().await;
    let Some(conns) = guard.take() else {
        // Already closed — idempotent.
        return;
    };

    let SqliteConnections { pool, write_connection } = conns;

    // Close the pool. Idle read connections are dropped immediately;
    // in-flight reads complete and their connections are discarded (not
    // recycled) on release. New pool.get() calls return PoolError::Closed.
    pool.close();

    let status = pool.status();
    info!(
        size = status.size,
        max_size = status.max_size,
        available = status.available,
        "{label} pause: pool closed"
    );

    // Close the write connection: wait for any in-flight write to finish,
    // run a WAL checkpoint, then drop on a blocking thread.
    close_connection(write_connection).await;

    let status = pool.status();
    info!(
        size = status.size,
        max_size = status.max_size,
        available = status.available,
        "{label} pause: write connection released"
    );

    // Wait for any in-flight read connections to drain.
    // The write connection has already been released above, so
    // pool.status().size == 0 now correctly means every connection is gone
    // and no SQLite file locks are held.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while pool.status().size > 0 {
        if tokio::time::Instant::now() >= deadline {
            let status = pool.status();
            warn!(
                size = status.size,
                max_size = status.max_size,
                available = status.available,
                "Timed out waiting for SQLite pool connections to drain"
            );
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Resume a store by rebuilding its connections.
///
/// Idempotent: if the store is already active this is a no-op.
pub(crate) async fn reopen_connections(
    connections: &Mutex<Option<SqliteConnections>>,
    db_path: PathBuf,
    pool_config: PoolConfig,
    runtime_config: crate::RuntimeConfig,
) -> crate::error::Result<()> {
    use crate::utils::SqliteAsyncConnExt as _;

    let mut guard = connections.lock().await;
    if guard.is_some() {
        // Not closed — idempotent.
        return Ok(());
    }

    // Rebuild the pool (connections are created lazily on first get()).
    let pool =
        Pool::builder(Manager::new(db_path)).config(pool_config).runtime(RUNTIME).build().map_err(
            |e| crate::error::Error::InvalidData {
                details: format!("Failed to rebuild connection pool: {e}"),
            },
        )?;

    let write_conn = pool.get().await?;
    // Re-apply runtime config (WAL mode, busy timeout, etc.)
    write_conn.apply_runtime_config(runtime_config).await?;

    *guard = Some(SqliteConnections { pool, write_connection: Arc::new(Mutex::new(write_conn)) });

    Ok(())
}
