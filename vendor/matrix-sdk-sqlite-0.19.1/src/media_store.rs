// Copyright 2024 The Matrix.org Foundation C.I.C.
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

//! An SQLite-based backend for the [`MediaStore`].

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use deadpool::managed::PoolConfig;
use matrix_sdk_base::{
    cross_process_lock::CrossProcessLockGeneration,
    media::{
        MediaRequestParameters, UniqueKey,
        store::{
            IgnoreMediaRetentionPolicy, MediaRetentionPolicy, MediaService, MediaStore,
            MediaStoreInner,
        },
    },
    timer,
};
use matrix_sdk_store_encryption::StoreCipher;
use ruma::{MilliSecondsSinceUnixEpoch, MxcUri, time::SystemTime};
use rusqlite::{OptionalExtension, params_from_iter};
use tokio::{
    fs,
    sync::{Mutex, OwnedMutexGuard},
};
use tracing::{debug, instrument};

use crate::{
    OpenStoreError, RuntimeConfig, Secret, SqliteStoreConfig,
    connection::{self, Connection as SqliteAsyncConn, Pool as SqlitePool, SqliteConnections},
    error::{Error, Result},
    utils::{
        EncryptableStore, SqliteAsyncConnExt, SqliteKeyValueStoreAsyncConnExt,
        SqliteKeyValueStoreConnExt, SqliteTransactionExt, time_to_timestamp,
    },
};

mod keys {
    // Entries in Key-value store
    pub const MEDIA_RETENTION_POLICY: &str = "media_retention_policy";
    pub const LAST_MEDIA_CLEANUP_TIME: &str = "last_media_cleanup_time";

    // Tables
    pub const MEDIA: &str = "media";
}

/// The database name.
const DATABASE_NAME: &str = "matrix-sdk-media.sqlite3";

/// An SQLite-based media store.
#[derive(Clone)]
pub struct SqliteMediaStore {
    store_cipher: Option<Arc<StoreCipher>>,

    /// `Some` when active, `None` when closed.
    connections: Arc<Mutex<Option<SqliteConnections>>>,

    /// Retained so we can rebuild the pool on reopen.
    db_path: PathBuf,

    /// Retained so we can rebuild the pool on reopen.
    pool_config: PoolConfig,

    /// Retained so we can re-apply runtime config on reopen.
    runtime_config: RuntimeConfig,

    media_service: MediaService,
}

#[cfg(not(tarpaulin_include))]
impl fmt::Debug for SqliteMediaStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqliteMediaStore").finish_non_exhaustive()
    }
}

impl EncryptableStore for SqliteMediaStore {
    fn get_cypher(&self) -> Option<&StoreCipher> {
        self.store_cipher.as_deref()
    }
}

impl SqliteMediaStore {
    /// Open the SQLite-based media store at the given path using the
    /// given passphrase to encrypt private data.
    pub async fn open(
        path: impl AsRef<Path>,
        passphrase: Option<&str>,
    ) -> Result<Self, OpenStoreError> {
        Self::open_with_config(&SqliteStoreConfig::new(path).passphrase(passphrase)).await
    }

    /// Open the SQLite-based media store at the given path using the given
    /// key to encrypt private data.
    pub async fn open_with_key(
        path: impl AsRef<Path>,
        key: Option<&[u8]>,
    ) -> Result<Self, OpenStoreError> {
        Self::open_with_config(&SqliteStoreConfig::new(path).key(key)).await
    }

    /// Open the SQLite-based media store with the config open config.
    #[instrument(skip(config), fields(path = ?config.path))]
    pub async fn open_with_config(config: &SqliteStoreConfig) -> Result<Self, OpenStoreError> {
        debug!(?config);

        let _timer = timer!("open_with_config");

        fs::create_dir_all(&config.path).await.map_err(OpenStoreError::CreateDir)?;

        let db_path = config.path.join(DATABASE_NAME);
        let pool_config = config.pool_config();
        let runtime_config = config.runtime_config();

        let pool = config.build_pool_of_connections(DATABASE_NAME)?;

        let this =
            Self::open_with_pool(pool, db_path, pool_config, runtime_config, config.secret.clone())
                .await?;

        // Apply runtime config on the write connection.
        this.write().await?.apply_runtime_config(runtime_config).await?;

        Ok(this)
    }

    /// Open an SQLite-based media store using the given SQLite database
    /// pool. The given passphrase will be used to encrypt private data.
    async fn open_with_pool(
        pool: SqlitePool,
        db_path: PathBuf,
        pool_config: PoolConfig,
        runtime_config: RuntimeConfig,
        secret: Option<Secret>,
    ) -> Result<Self, OpenStoreError> {
        let conn = pool.get().await?;

        let version = conn.db_version().await?;
        run_migrations(&conn, version).await?;

        conn.wal_checkpoint().await;

        let store_cipher = match &secret {
            Some(s) => Some(Arc::new(conn.get_or_create_store_cipher(s.clone()).await?)),
            None => None,
        };

        let media_service = MediaService::new();
        let media_retention_policy = conn.get_serialized_kv(keys::MEDIA_RETENTION_POLICY).await?;
        let last_media_cleanup_time = conn.get_serialized_kv(keys::LAST_MEDIA_CLEANUP_TIME).await?;
        media_service.restore(media_retention_policy, last_media_cleanup_time);

        let connections = SqliteConnections {
            pool,
            // Use `conn` as our selected write connection.
            write_connection: Arc::new(Mutex::new(conn)),
        };

        Ok(Self {
            store_cipher,
            connections: Arc::new(Mutex::new(Some(connections))),
            db_path,
            pool_config,
            runtime_config,
            media_service,
        })
    }

    // Acquire a connection for executing read operations.
    #[instrument(skip_all)]
    async fn read(&self) -> Result<SqliteAsyncConn> {
        let pool = {
            let guard = self.connections.lock().await;
            let conns = guard.as_ref().ok_or(Error::StoreClosed)?;
            conns.pool.clone()
        };

        let connection = pool.get().await?;

        // Per https://www.sqlite.org/foreignkeys.html#fk_enable, foreign key
        // support must be enabled on a per-connection basis. Execute it every
        // time we try to get a connection, since we can't guarantee a previous
        // connection did enable it before.
        connection.execute_batch("PRAGMA foreign_keys = ON;").await?;

        Ok(connection)
    }

    // Acquire a connection for executing write operations.
    #[instrument(skip_all)]
    async fn write(&self) -> Result<OwnedMutexGuard<SqliteAsyncConn>> {
        let write_connection = {
            let guard = self.connections.lock().await;
            let conns = guard.as_ref().ok_or(Error::StoreClosed)?;
            conns.write_connection.clone()
        };

        let connection = write_connection.lock_owned().await;

        // Per https://www.sqlite.org/foreignkeys.html#fk_enable, foreign key
        // support must be enabled on a per-connection basis. Execute it every
        // time we try to get a connection, since we can't guarantee a previous
        // connection did enable it before.
        connection.execute_batch("PRAGMA foreign_keys = ON;").await?;

        Ok(connection)
    }

    pub async fn vacuum(&self) -> Result<()> {
        let write_connection = {
            let guard = self.connections.lock().await;
            let conns = guard.as_ref().ok_or(Error::StoreClosed)?;
            conns.write_connection.clone()
        };
        write_connection.lock().await.vacuum().await
    }

    async fn get_db_size(&self) -> Result<Option<usize>> {
        let pool = {
            let guard = self.connections.lock().await;
            let conns = guard.as_ref().ok_or(Error::StoreClosed)?;
            conns.pool.clone()
        };
        Ok(Some(pool.get().await?.get_db_size().await?))
    }

    pub async fn close(&self) -> Result<()> {
        connection::close_connections(&self.connections, "Media store").await;
        Ok(())
    }

    pub async fn reopen(&self) -> Result<()> {
        connection::reopen_connections(
            &self.connections,
            self.db_path.clone(),
            self.pool_config,
            self.runtime_config,
        )
        .await?;
        Ok(())
    }

    /// Returns the pool size status, for testing purposes.
    #[cfg(test)]
    async fn pool_max_size(&self) -> Option<usize> {
        let guard = self.connections.lock().await;
        guard.as_ref().map(|conns| conns.pool.status().max_size)
    }
}

/// Run migrations for the given version of the database.
async fn run_migrations(conn: &SqliteAsyncConn, version: u8) -> Result<()> {
    // Always enable foreign keys for the current connection.
    conn.execute_batch("PRAGMA foreign_keys = ON;").await?;

    if version < 1 {
        debug!("Creating database");
        // First turn on WAL mode, this can't be done in the transaction, it fails with
        // the error message: "cannot change into wal mode from within a transaction".
        conn.execute_batch("PRAGMA journal_mode = wal;").await?;
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!("../migrations/media_store/001_init.sql"))?;
            txn.set_db_version(1)
        })
        .await?;
    }

    if version < 2 {
        debug!("Upgrading database to version 2");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/media_store/002_lease_locks_with_generation.sql"
            ))?;
            txn.set_db_version(2)
        })
        .await?;
    }

    Ok(())
}

#[async_trait]
impl MediaStore for SqliteMediaStore {
    type Error = Error;

    #[instrument(skip(self))]
    async fn try_take_leased_lock(
        &self,
        lease_duration_ms: u32,
        key: &str,
        holder: &str,
    ) -> Result<Option<CrossProcessLockGeneration>> {
        let key = key.to_owned();
        let holder = holder.to_owned();

        let now: u64 = MilliSecondsSinceUnixEpoch::now().get().into();
        let expiration = now + lease_duration_ms as u64;

        // Learn about the `excluded` keyword in https://sqlite.org/lang_upsert.html.
        let generation = self
            .write()
            .await?
            .with_transaction(move |txn| {
                txn.query_one(
                    "INSERT INTO lease_locks (key, holder, expiration)
                    VALUES (?1, ?2, ?3)
                    ON CONFLICT (key)
                    DO
                        UPDATE SET
                            holder = excluded.holder,
                            expiration = excluded.expiration,
                            generation =
                                CASE holder
                                    WHEN excluded.holder THEN generation
                                    ELSE generation + 1
                                END
                        WHERE
                            holder = excluded.holder
                            OR expiration < ?4
                    RETURNING generation
                    ",
                    (key, holder, expiration, now),
                    |row| row.get(0),
                )
                .optional()
            })
            .await?;

        Ok(generation)
    }

    async fn add_media_content(
        &self,
        request: &MediaRequestParameters,
        content: Vec<u8>,
        ignore_policy: IgnoreMediaRetentionPolicy,
    ) -> Result<()> {
        let _timer = timer!("method");

        self.media_service.add_media_content(self, request, content, ignore_policy).await
    }

    #[instrument(skip_all)]
    async fn replace_media_key(
        &self,
        from: &MediaRequestParameters,
        to: &MediaRequestParameters,
    ) -> Result<(), Self::Error> {
        let _timer = timer!("method");

        let prev_uri = self.encode_key(keys::MEDIA, from.source.unique_key());
        let prev_format = self.encode_key(keys::MEDIA, from.format.unique_key());

        let new_uri = self.encode_key(keys::MEDIA, to.source.unique_key());
        let new_format = self.encode_key(keys::MEDIA, to.format.unique_key());

        let conn = self.write().await?;
        conn.execute(
            r#"UPDATE media SET uri = ?, format = ? WHERE uri = ? AND format = ?"#,
            (new_uri, new_format, prev_uri, prev_format),
        )
        .await?;

        Ok(())
    }

    #[instrument(skip_all)]
    async fn get_media_content(&self, request: &MediaRequestParameters) -> Result<Option<Vec<u8>>> {
        let _timer = timer!("method");

        self.media_service.get_media_content(self, request).await
    }

    #[instrument(skip_all)]
    async fn remove_media_content(&self, request: &MediaRequestParameters) -> Result<()> {
        let _timer = timer!("method");

        let uri = self.encode_key(keys::MEDIA, request.source.unique_key());
        let format = self.encode_key(keys::MEDIA, request.format.unique_key());

        let conn = self.write().await?;
        conn.execute("DELETE FROM media WHERE uri = ? AND format = ?", (uri, format)).await?;

        Ok(())
    }

    #[instrument(skip(self))]
    async fn remove_media_content_for_uri(&self, uri: &MxcUri) -> Result<()> {
        let _timer = timer!("method");

        let uri = self.encode_key(keys::MEDIA, uri);

        let conn = self.write().await?;
        conn.execute("DELETE FROM media WHERE uri = ?", (uri,)).await?;

        Ok(())
    }

    #[instrument(skip_all)]
    async fn set_media_retention_policy(
        &self,
        policy: MediaRetentionPolicy,
    ) -> Result<(), Self::Error> {
        let _timer = timer!("method");

        self.media_service.set_media_retention_policy(self, policy).await
    }

    #[instrument(skip_all)]
    fn media_retention_policy(&self) -> MediaRetentionPolicy {
        let _timer = timer!("method");

        self.media_service.media_retention_policy()
    }

    #[instrument(skip_all)]
    async fn set_ignore_media_retention_policy(
        &self,
        request: &MediaRequestParameters,
        ignore_policy: IgnoreMediaRetentionPolicy,
    ) -> Result<(), Self::Error> {
        let _timer = timer!("method");

        self.media_service.set_ignore_media_retention_policy(self, request, ignore_policy).await
    }

    #[instrument(skip_all)]
    async fn clean(&self) -> Result<(), Self::Error> {
        let _timer = timer!("method");

        self.media_service.clean(self).await
    }

    async fn close(&self) -> Result<(), Self::Error> {
        SqliteMediaStore::close(self).await
    }

    async fn reopen(&self) -> Result<(), Self::Error> {
        SqliteMediaStore::reopen(self).await
    }

    async fn optimize(&self) -> Result<(), Self::Error> {
        Ok(self.vacuum().await?)
    }

    async fn get_size(&self) -> Result<Option<usize>, Self::Error> {
        self.get_db_size().await
    }
}

#[cfg_attr(target_family = "wasm", async_trait(?Send))]
#[cfg_attr(not(target_family = "wasm"), async_trait)]
impl MediaStoreInner for SqliteMediaStore {
    type Error = Error;

    async fn media_retention_policy_inner(
        &self,
    ) -> Result<Option<MediaRetentionPolicy>, Self::Error> {
        let conn = self.read().await?;
        conn.get_serialized_kv(keys::MEDIA_RETENTION_POLICY).await
    }

    async fn set_media_retention_policy_inner(
        &self,
        policy: MediaRetentionPolicy,
    ) -> Result<(), Self::Error> {
        let conn = self.write().await?;
        conn.set_serialized_kv(keys::MEDIA_RETENTION_POLICY, policy).await?;
        Ok(())
    }

    async fn add_media_content_inner(
        &self,
        request: &MediaRequestParameters,
        data: Vec<u8>,
        last_access: SystemTime,
        policy: MediaRetentionPolicy,
        ignore_policy: IgnoreMediaRetentionPolicy,
    ) -> Result<(), Self::Error> {
        let ignore_policy = ignore_policy.is_yes();
        let data = self.encode_value(data)?;

        if !ignore_policy && policy.exceeds_max_file_size(data.len() as u64) {
            return Ok(());
        }

        let uri = self.encode_key(keys::MEDIA, request.source.unique_key());
        let format = self.encode_key(keys::MEDIA, request.format.unique_key());
        let timestamp = time_to_timestamp(last_access);

        let conn = self.write().await?;
        conn.execute(
            "INSERT OR REPLACE INTO media (uri, format, data, last_access, ignore_policy) VALUES (?, ?, ?, ?, ?)",
            (uri, format, data, timestamp, ignore_policy),
        )
        .await?;

        Ok(())
    }

    async fn set_ignore_media_retention_policy_inner(
        &self,
        request: &MediaRequestParameters,
        ignore_policy: IgnoreMediaRetentionPolicy,
    ) -> Result<(), Self::Error> {
        let uri = self.encode_key(keys::MEDIA, request.source.unique_key());
        let format = self.encode_key(keys::MEDIA, request.format.unique_key());
        let ignore_policy = ignore_policy.is_yes();

        let conn = self.write().await?;
        conn.execute(
            r#"UPDATE media SET ignore_policy = ? WHERE uri = ? AND format = ?"#,
            (ignore_policy, uri, format),
        )
        .await?;

        Ok(())
    }

    async fn get_media_content_inner(
        &self,
        request: &MediaRequestParameters,
        current_time: SystemTime,
    ) -> Result<Option<Vec<u8>>, Self::Error> {
        let uri = self.encode_key(keys::MEDIA, request.source.unique_key());
        let format = self.encode_key(keys::MEDIA, request.format.unique_key());
        let timestamp = time_to_timestamp(current_time);

        let conn = self.write().await?;
        let data = conn
            .with_transaction::<_, rusqlite::Error, _>(move |txn| {
                // Update the last access.
                // We need to do this first so the transaction is in write mode right away.
                // See: https://sqlite.org/lang_transaction.html#read_transactions_versus_write_transactions
                txn.execute(
                    "UPDATE media SET last_access = ? WHERE uri = ? AND format = ?",
                    (timestamp, &uri, &format),
                )?;

                txn.query_row::<Vec<u8>, _, _>(
                    "SELECT data FROM media WHERE uri = ? AND format = ?",
                    (&uri, &format),
                    |row| row.get(0),
                )
                .optional()
            })
            .await?;

        data.map(|v| self.decode_value(&v).map(Into::into)).transpose()
    }

    async fn clean_inner(
        &self,
        policy: MediaRetentionPolicy,
        current_time: SystemTime,
    ) -> Result<(), Self::Error> {
        if !policy.has_limitations() {
            // We can safely skip all the checks.
            return Ok(());
        }

        let conn = self.write().await?;
        let removed = conn
            .with_transaction::<_, Error, _>(move |txn| {
                let mut removed = false;

                // First, check media content that exceed the max filesize.
                if let Some(max_file_size) = policy.computed_max_file_size() {
                    let count = txn.execute(
                        "DELETE FROM media WHERE ignore_policy IS FALSE AND length(data) > ?",
                        (max_file_size,),
                    )?;

                    if count > 0 {
                        removed = true;
                    }
                }

                // Then, clean up expired media content.
                if let Some(last_access_expiry) = policy.last_access_expiry {
                    let current_timestamp = time_to_timestamp(current_time);
                    let expiry_secs = last_access_expiry.as_secs();
                    let count = txn.execute(
                        "DELETE FROM media WHERE ignore_policy IS FALSE AND (? - last_access) >= ?",
                        (current_timestamp, expiry_secs),
                    )?;

                    if count > 0 {
                        removed = true;
                    }
                }

                // Finally, if the cache size is too big, remove old items until it fits.
                if let Some(max_cache_size) = policy.max_cache_size {
                    // i64 is the integer type used by SQLite, use it here to avoid usize overflow
                    // during the conversion of the result.
                    let cache_size = txn
                        .query_row(
                            "SELECT sum(length(data)) FROM media WHERE ignore_policy IS FALSE",
                            (),
                            |row| {
                                // `sum()` returns `NULL` if there are no rows.
                                row.get::<_, Option<u64>>(0)
                            },
                        )?
                        .unwrap_or_default();

                    // If the cache size is overflowing or bigger than max cache size, clean up.
                    if cache_size > max_cache_size {
                        // Get the sizes of the media contents ordered by last access.
                        let mut cached_stmt = txn.prepare_cached(
                            "SELECT rowid, length(data) FROM media \
                             WHERE ignore_policy IS FALSE ORDER BY last_access DESC",
                        )?;
                        let content_sizes = cached_stmt
                            .query(())?
                            .mapped(|row| Ok((row.get::<_, i64>(0)?, row.get::<_, u64>(1)?)));

                        let mut accumulated_items_size = 0u64;
                        let mut limit_reached = false;
                        let mut rows_to_remove = Vec::new();

                        for result in content_sizes {
                            let (row_id, size) = match result {
                                Ok(content_size) => content_size,
                                Err(error) => {
                                    return Err(error.into());
                                }
                            };

                            if limit_reached {
                                rows_to_remove.push(row_id);
                                continue;
                            }

                            match accumulated_items_size.checked_add(size) {
                                Some(acc) if acc > max_cache_size => {
                                    // We can stop accumulating.
                                    limit_reached = true;
                                    rows_to_remove.push(row_id);
                                }
                                Some(acc) => accumulated_items_size = acc,
                                None => {
                                    // The accumulated size is overflowing but the setting cannot be
                                    // bigger than usize::MAX, we can stop accumulating.
                                    limit_reached = true;
                                    rows_to_remove.push(row_id);
                                }
                            }
                        }

                        if !rows_to_remove.is_empty() {
                            removed = true;
                        }

                        txn.chunk_large_query_over(rows_to_remove, None, |txn, row_ids| {
                            let sql_params = row_ids.host_parameters();
                            let query = format!("DELETE FROM media WHERE rowid IN ({sql_params})");
                            txn.prepare(&query)?.execute(params_from_iter(row_ids))?;
                            Ok(Vec::<()>::new())
                        })?;
                    }
                }

                txn.set_serialized_kv(keys::LAST_MEDIA_CLEANUP_TIME, current_time)?;

                Ok(removed)
            })
            .await?;

        // If we removed media, defragment the database and free space on the
        // filesystem.
        if removed {
            conn.vacuum().await?;
        }

        Ok(())
    }

    async fn last_media_cleanup_time_inner(&self) -> Result<Option<SystemTime>, Self::Error> {
        let conn = self.read().await?;
        conn.get_serialized_kv(keys::LAST_MEDIA_CLEANUP_TIME).await
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{
            LazyLock,
            atomic::{AtomicU32, Ordering::SeqCst},
        },
        time::Duration,
    };

    use matrix_sdk_base::{
        media::{
            MediaFormat, MediaRequestParameters, MediaThumbnailSettings,
            store::{IgnoreMediaRetentionPolicy, MediaStore, MediaStoreError},
        },
        media_store_inner_integration_tests, media_store_integration_tests,
        media_store_integration_tests_time,
    };
    use matrix_sdk_test::async_test;
    use ruma::{events::room::MediaSource, media::Method, mxc_uri, uint};
    use tempfile::{TempDir, tempdir};

    use super::SqliteMediaStore;
    use crate::{SqliteStoreConfig, utils::SqliteAsyncConnExt};

    static TMP_DIR: LazyLock<TempDir> = LazyLock::new(|| tempdir().unwrap());
    static NUM: AtomicU32 = AtomicU32::new(0);

    fn new_media_store_workspace() -> PathBuf {
        let name = NUM.fetch_add(1, SeqCst).to_string();
        TMP_DIR.path().join(name)
    }

    async fn get_media_store() -> Result<SqliteMediaStore, MediaStoreError> {
        let tmpdir_path = new_media_store_workspace();

        tracing::info!("using media store @ {}", tmpdir_path.to_str().unwrap());

        Ok(SqliteMediaStore::open(tmpdir_path.to_str().unwrap(), None).await.unwrap())
    }

    media_store_integration_tests!();
    media_store_integration_tests_time!();
    media_store_inner_integration_tests!();

    async fn get_media_store_content_sorted_by_last_access(
        media_store: &SqliteMediaStore,
    ) -> Vec<Vec<u8>> {
        let sqlite_db = media_store.read().await.expect("accessing sqlite db failed");
        sqlite_db
            .prepare("SELECT data FROM media ORDER BY last_access DESC", |mut stmt| {
                stmt.query(())?.mapped(|row| row.get(0)).collect()
            })
            .await
            .expect("querying media cache content by last access failed")
    }

    #[async_test]
    async fn test_pool_size() {
        let tmpdir_path = new_media_store_workspace();
        let store_open_config = SqliteStoreConfig::new(tmpdir_path).pool_max_size(42);

        let store = SqliteMediaStore::open_with_config(&store_open_config).await.unwrap();

        assert_eq!(store.pool_max_size().await.unwrap(), 42);
    }

    #[async_test]
    async fn test_last_access() {
        let media_store = get_media_store().await.expect("creating media cache failed");
        let uri = mxc_uri!("mxc://localhost/media");
        let file_request = MediaRequestParameters {
            source: MediaSource::Plain(uri.to_owned()),
            format: MediaFormat::File,
        };
        let thumbnail_request = MediaRequestParameters {
            source: MediaSource::Plain(uri.to_owned()),
            format: MediaFormat::Thumbnail(MediaThumbnailSettings::with_method(
                Method::Crop,
                uint!(100),
                uint!(100),
            )),
        };

        let content: Vec<u8> = "hello world".into();
        let thumbnail_content: Vec<u8> = "hello…".into();

        // Add the media.
        media_store
            .add_media_content(&file_request, content.clone(), IgnoreMediaRetentionPolicy::No)
            .await
            .expect("adding file failed");

        // Since the precision of the timestamp is in seconds, wait so the timestamps
        // differ.
        tokio::time::sleep(Duration::from_secs(3)).await;

        media_store
            .add_media_content(
                &thumbnail_request,
                thumbnail_content.clone(),
                IgnoreMediaRetentionPolicy::No,
            )
            .await
            .expect("adding thumbnail failed");

        // File's last access is older than thumbnail.
        let contents = get_media_store_content_sorted_by_last_access(&media_store).await;

        assert_eq!(contents.len(), 2, "media cache contents length is wrong");
        assert_eq!(contents[0], thumbnail_content, "thumbnail is not last access");
        assert_eq!(contents[1], content, "file is not second-to-last access");

        // Since the precision of the timestamp is in seconds, wait so the timestamps
        // differ.
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Access the file so its last access is more recent.
        let _ = media_store
            .get_media_content(&file_request)
            .await
            .expect("getting file failed")
            .expect("file is missing");

        // File's last access is more recent than thumbnail.
        let contents = get_media_store_content_sorted_by_last_access(&media_store).await;

        assert_eq!(contents.len(), 2, "media cache contents length is wrong");
        assert_eq!(contents[0], content, "file is not last access");
        assert_eq!(contents[1], thumbnail_content, "thumbnail is not second-to-last access");
    }
}

#[cfg(test)]
mod close_reopen_tests {
    use std::sync::{
        LazyLock,
        atomic::{AtomicU32, Ordering::SeqCst},
    };

    use matrix_sdk_base::media::{
        MediaFormat, MediaRequestParameters,
        store::{IgnoreMediaRetentionPolicy, MediaStore},
    };
    use matrix_sdk_test::async_test;
    use ruma::{events::room::MediaSource, mxc_uri};
    use tempfile::{TempDir, tempdir};

    use super::SqliteMediaStore;

    static TMP_DIR: LazyLock<TempDir> = LazyLock::new(|| tempdir().unwrap());
    static NUM: AtomicU32 = AtomicU32::new(0);

    async fn new_store() -> SqliteMediaStore {
        let name = NUM.fetch_add(1, SeqCst).to_string();
        let tmpdir_path = TMP_DIR.path().join(name);
        SqliteMediaStore::open(tmpdir_path, None).await.unwrap()
    }

    fn test_request() -> MediaRequestParameters {
        MediaRequestParameters {
            source: MediaSource::Plain(mxc_uri!("mxc://localhost/test_media").to_owned()),
            format: MediaFormat::File,
        }
    }

    #[async_test]
    async fn test_close_completes_without_timeout() {
        let store = new_store().await;

        // Close should complete quickly without hitting any timeout.
        let start = std::time::Instant::now();
        store.close().await.unwrap();
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "close() took {elapsed:?}, expected < 2s (no timeout)"
        );

        // Connections should be None after close.
        let guard = store.connections.lock().await;
        assert!(guard.is_none(), "connections should be None after close");
    }

    #[async_test]
    async fn test_reopen_restores_connections() {
        let store = new_store().await;

        store.close().await.unwrap();

        {
            let guard = store.connections.lock().await;
            assert!(guard.is_none());
        }

        store.reopen().await.unwrap();

        {
            let guard = store.connections.lock().await;
            assert!(guard.is_some(), "connections should be Some after reopen");
        }
    }

    #[async_test]
    async fn test_close_is_idempotent() {
        let store = new_store().await;

        store.close().await.unwrap();
        // Second close should be a no-op.
        store.close().await.unwrap();

        let guard = store.connections.lock().await;
        assert!(guard.is_none());
    }

    #[async_test]
    async fn test_reopen_is_idempotent() {
        let store = new_store().await;

        // Reopen on an active store should be a no-op.
        store.reopen().await.unwrap();

        let guard = store.connections.lock().await;
        assert!(guard.is_some());
    }

    #[async_test]
    async fn test_read_fails_when_closed() {
        let store = new_store().await;
        store.close().await.unwrap();

        let err = store.get_media_content(&test_request()).await;
        assert!(err.is_err(), "read should fail when closed");

        let err_msg = err.unwrap_err().to_string();
        assert!(err_msg.contains("closed"), "error should mention 'closed', got: {err_msg}");
    }

    #[async_test]
    async fn test_write_fails_when_closed() {
        let store = new_store().await;
        store.close().await.unwrap();

        let err = store
            .add_media_content(&test_request(), b"data".to_vec(), IgnoreMediaRetentionPolicy::No)
            .await;
        assert!(err.is_err(), "write should fail when closed");

        let err_msg = err.unwrap_err().to_string();
        assert!(err_msg.contains("closed"), "error should mention 'closed', got: {err_msg}");
    }

    #[async_test]
    async fn test_data_persists_across_close_reopen() {
        let store = new_store().await;

        // Write some media content.
        store
            .add_media_content(
                &test_request(),
                b"hello world".to_vec(),
                IgnoreMediaRetentionPolicy::Yes,
            )
            .await
            .unwrap();

        // Verify it's there.
        let content = store.get_media_content(&test_request()).await.unwrap();
        assert_eq!(content.as_deref(), Some(b"hello world".as_slice()));

        // Close and reopen.
        store.close().await.unwrap();
        store.reopen().await.unwrap();

        // Content should still be there after reopen.
        let content = store.get_media_content(&test_request()).await.unwrap();
        assert_eq!(
            content.as_deref(),
            Some(b"hello world".as_slice()),
            "media content should persist across close/reopen"
        );
    }

    #[async_test]
    async fn test_multiple_close_reopen_cycles() {
        let store = new_store().await;

        for _ in 0..5 {
            store.close().await.unwrap();
            store.reopen().await.unwrap();

            // After each cycle, the store should be fully operational.
            let result = store.get_media_content(&test_request()).await;
            assert!(result.is_ok(), "store should work after close/reopen cycle");
        }
    }

    #[async_test]
    async fn test_pool_is_fully_drained_after_close() {
        let store = new_store().await;

        // Do a few reads to exercise the pool.
        let _ = store.get_media_content(&test_request()).await;
        let _ = store.get_media_content(&test_request()).await;

        store.close().await.unwrap();

        // After close, the connections field should be None (pool and write
        // connection have been fully torn down).
        let guard = store.connections.lock().await;
        assert!(guard.is_none(), "all connections should be released after close");
    }

    #[async_test]
    async fn test_operations_work_immediately_after_reopen() {
        let store = new_store().await;

        store.close().await.unwrap();
        store.reopen().await.unwrap();

        // Read should work immediately after reopen.
        let result = store.get_media_content(&test_request()).await;
        assert!(result.is_ok(), "read should succeed immediately after reopen");

        // Write should work immediately after reopen.
        let result = store
            .add_media_content(
                &test_request(),
                b"after_reopen".to_vec(),
                IgnoreMediaRetentionPolicy::No,
            )
            .await;
        assert!(result.is_ok(), "write should succeed immediately after reopen");
    }

    #[async_test]
    async fn test_close_waits_for_held_read_connection_to_drain() {
        let store = new_store().await;

        // Acquire a read connection and hold it, simulating an in-flight read.
        let held_conn = store.read().await.unwrap();

        // Spawn close in a background task — it will close the pool and then
        // poll-wait for pool.status().size == 0 in the drain loop.
        let store_clone = store.clone();
        let close_handle = tokio::spawn(async move {
            store_clone.close().await.unwrap();
        });

        // Give close() a moment to close the pool and enter the drain loop.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // The close task should still be running because we hold a connection.
        assert!(!close_handle.is_finished(), "close should be waiting for the held connection");

        // Release the held connection — this lets pool.status().size drop to 0.
        drop(held_conn);

        // Now close should complete promptly (well within the 5s timeout).
        let timeout = tokio::time::timeout(std::time::Duration::from_secs(3), close_handle).await;
        assert!(timeout.is_ok(), "close should complete after the held connection is released");
        timeout.unwrap().unwrap();

        // Verify the store is fully closed.
        let guard = store.connections.lock().await;
        assert!(guard.is_none(), "connections should be None after close");
    }
}

#[cfg(test)]
mod encrypted_tests {
    use std::sync::{
        LazyLock,
        atomic::{AtomicU32, Ordering::SeqCst},
    };

    use matrix_sdk_base::{
        media::store::MediaStoreError, media_store_inner_integration_tests,
        media_store_integration_tests, media_store_integration_tests_time,
    };
    use tempfile::{TempDir, tempdir};

    use super::SqliteMediaStore;

    static TMP_DIR: LazyLock<TempDir> = LazyLock::new(|| tempdir().unwrap());
    static NUM: AtomicU32 = AtomicU32::new(0);

    async fn get_media_store() -> Result<SqliteMediaStore, MediaStoreError> {
        let name = NUM.fetch_add(1, SeqCst).to_string();
        let tmpdir_path = TMP_DIR.path().join(name);

        tracing::info!("using media store @ {}", tmpdir_path.to_str().unwrap());

        Ok(SqliteMediaStore::open(tmpdir_path.to_str().unwrap(), Some("default_test_password"))
            .await
            .unwrap())
    }

    media_store_integration_tests!();
    media_store_integration_tests_time!();
    media_store_inner_integration_tests!();
}
