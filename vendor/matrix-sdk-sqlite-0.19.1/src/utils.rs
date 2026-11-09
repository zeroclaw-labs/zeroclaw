// Copyright 2022 The Matrix.org Foundation C.I.C.
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

use core::fmt;
use std::{
    borrow::{Borrow, Cow},
    cmp::min,
    iter,
    ops::Deref,
};

use async_trait::async_trait;
use base64::Engine as _;
use deadpool_sync::InteractError;
use itertools::Itertools;
use matrix_sdk_store_encryption::{EncryptableValue, StoreCipher};
use ruma::{OwnedEventId, OwnedRoomId, serde::Raw, time::SystemTime};
use rusqlite::{OptionalExtension, Params, Row, Statement, Transaction, limits::Limit};
use serde::{Serialize, de::DeserializeOwned};
use tracing::{error, trace, warn};
use vodozemac::base64_encode;
use zeroize::Zeroize;

use crate::{
    OpenStoreError, RuntimeConfig, Secret,
    connection::Connection as SqliteAsyncConn,
    error::{Error, Result},
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Key {
    Plain(Vec<u8>),
    Hashed([u8; 32]),
}

impl Deref for Key {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        match self {
            Key::Plain(slice) => slice,
            Key::Hashed(bytes) => bytes,
        }
    }
}

impl Borrow<[u8]> for Key {
    fn borrow(&self) -> &[u8] {
        self.deref()
    }
}

impl rusqlite::ToSql for Key {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.deref().to_sql()
    }
}

#[async_trait]
pub(crate) trait SqliteAsyncConnExt {
    async fn execute<P>(
        &self,
        sql: impl AsRef<str> + Send + 'static,
        params: P,
    ) -> rusqlite::Result<usize>
    where
        P: Params + Send + 'static;

    async fn execute_batch(&self, sql: impl AsRef<str> + Send + 'static) -> rusqlite::Result<()>;

    async fn prepare<T, F>(
        &self,
        sql: impl AsRef<str> + Send + 'static,
        f: F,
    ) -> rusqlite::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Statement<'_>) -> rusqlite::Result<T> + Send + 'static;

    async fn query_row<T, P, F>(
        &self,
        sql: impl AsRef<str> + Send + 'static,
        params: P,
        f: F,
    ) -> rusqlite::Result<T>
    where
        T: Send + 'static,
        P: Params + Send + 'static,
        F: FnOnce(&Row<'_>) -> rusqlite::Result<T> + Send + 'static;

    async fn query_one<T, P, F>(
        &self,
        sql: impl AsRef<str> + Send + 'static,
        params: P,
        f: F,
    ) -> rusqlite::Result<T>
    where
        T: Send + 'static,
        P: Params + Send + 'static,
        F: FnOnce(&Row<'_>) -> rusqlite::Result<T> + Send + 'static;

    async fn query_many<T, P, F>(
        &self,
        sql: impl AsRef<str> + Send + 'static,
        params: P,
        f: F,
    ) -> rusqlite::Result<Vec<T>>
    where
        T: Send + 'static,
        P: Params + Send + 'static,
        F: FnMut(&Row<'_>) -> rusqlite::Result<T> + Send + 'static;

    async fn with_transaction<T, E, F>(&self, f: F) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<rusqlite::Error> + Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<T, E> + Send + 'static;

    /// Chunk a large query over some keys.
    ///
    /// Imagine there is a _dynamic_ query that runs potentially large number of
    /// parameters, so much that the maximum number of parameters can be hit.
    /// Then, this helper is for you. It will execute the query on chunks of
    /// parameters.
    async fn chunk_large_query_over<Query, Res>(
        &self,
        mut keys_to_chunk: Vec<Key>,
        result_capacity: Option<usize>,
        do_query: Query,
    ) -> Result<Vec<Res>>
    where
        Res: Send + 'static,
        Query: Fn(&Transaction<'_>, ChunkFromLargeQuery<Key>) -> Result<Vec<Res>> + Send + 'static;

    /// Apply the [`RuntimeConfig`].
    ///
    /// It will call the `Self::optimize`, `Self::cache_size` or
    /// `Self::journal_size_limit` methods automatically based on the
    /// `RuntimeConfig` values.
    ///
    /// It is possible to call these methods individually though. This
    /// `apply_runtime_config` method allows to automate this process.
    async fn apply_runtime_config(&self, runtime_config: RuntimeConfig) -> Result<()> {
        let RuntimeConfig { optimize, cache_size, journal_size_limit } = runtime_config;

        if optimize {
            self.optimize().await?;
        }

        self.cache_size(cache_size).await?;
        self.journal_size_limit(journal_size_limit).await?;

        Ok(())
    }

    /// Optimize the database.
    ///
    /// The SQLite documentation recommends to run this regularly and after any
    /// schema change. The easiest is to do it consistently when the store is
    /// constructed, after eventual migrations.
    ///
    /// See [`PRAGMA optimize`] to learn more.
    ///
    /// [`PRAGMA cache_size`]: https://www.sqlite.org/pragma.html#pragma_optimize
    async fn optimize(&self) -> Result<()> {
        self.execute_batch("PRAGMA optimize = 0x10002;").await?;
        Ok(())
    }

    /// Define the maximum size in **bytes** the SQLite cache can use.
    ///
    /// See [`PRAGMA cache_size`] to learn more.
    ///
    /// [`PRAGMA cache_size`]: https://www.sqlite.org/pragma.html#pragma_cache_size
    async fn cache_size(&self, cache_size: u32) -> Result<()> {
        // `N` in `PRAGMA cache_size = -N` is expressed in kibibytes.
        // `cache_size` is expressed in bytes. Let's convert.
        let n = cache_size / 1024;

        self.execute_batch(format!("PRAGMA cache_size = -{n};")).await?;
        Ok(())
    }

    /// Limit the size of the WAL file, in **bytes**.
    ///
    /// By default, while the DB connections of the databases are open, [the
    /// size of the WAL file can keep increasing][size_wal_file] depending on
    /// the size needed for the transactions. A critical case is `VACUUM`
    /// which basically writes the content of the DB file to the WAL file
    /// before writing it back to the DB file, so we end up taking twice the
    /// size of the database.
    ///
    /// By setting this limit, the WAL file is truncated after its content is
    /// written to the database, if it is bigger than the limit.
    ///
    /// See [`PRAGMA journal_size_limit`] to learn more. The value `limit`
    /// corresponds to `N` in `PRAGMA journal_size_limit = N`.
    ///
    /// [size_wal_file]: https://www.sqlite.org/wal.html#avoiding_excessively_large_wal_files
    /// [`PRAGMA journal_size_limit`]: https://www.sqlite.org/pragma.html#pragma_journal_size_limit
    async fn journal_size_limit(&self, limit: u32) -> Result<()> {
        self.execute_batch(format!("PRAGMA journal_size_limit = {limit};")).await?;
        Ok(())
    }

    /// Defragment the database and free space on the filesystem.
    ///
    /// Only returns an error in tests, otherwise the error is only logged.
    async fn vacuum(&self) -> Result<()> {
        // Truncate the WAL file before vacuuming so it has room to grow.
        self.wal_checkpoint().await;
        if let Err(error) = self.execute_batch("VACUUM").await {
            // Since this is an optimisation step, do not propagate the error
            // but log it.
            #[cfg(not(any(test, debug_assertions)))]
            tracing::warn!("Failed to vacuum database: {error}");

            // We want to know if there is an error with this step during tests.
            #[cfg(any(test, debug_assertions))]
            return Err(error.into());
        } else {
            trace!("VACUUM complete");
            // Once vacuumed, truncate the WAL file again to purge the copied DB contents.
            self.wal_checkpoint().await;
        }

        Ok(())
    }

    /// Adds a manual [WAL checkpoint] to copy back the contents of the WAL
    /// files into the actual database, resetting the write-ahead log.
    ///
    /// [WAL checkpoint]: https://sqlite.org/c3ref/wal_checkpoint.html
    async fn wal_checkpoint(&self) {
        match self.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").await {
            Ok(_) => trace!("WAL checkpoint completed"),
            Err(error) => error!(?error, "WAL checkpoint error"),
        }
    }

    async fn get_db_size(&self) -> Result<usize> {
        let page_size =
            self.query_row("PRAGMA page_size;", (), |row| row.get::<_, usize>(0)).await?;
        let total_pages =
            self.query_row("PRAGMA page_count;", (), |row| row.get::<_, usize>(0)).await?;

        Ok(total_pages * page_size)
    }
}

#[async_trait]
impl SqliteAsyncConnExt for SqliteAsyncConn {
    async fn execute<P>(
        &self,
        sql: impl AsRef<str> + Send + 'static,
        params: P,
    ) -> rusqlite::Result<usize>
    where
        P: Params + Send + 'static,
    {
        self.interact(move |conn| conn.execute(sql.as_ref(), params))
            .await
            .map_err(map_interact_err)?
    }

    async fn execute_batch(&self, sql: impl AsRef<str> + Send + 'static) -> rusqlite::Result<()> {
        self.interact(move |conn| conn.execute_batch(sql.as_ref()))
            .await
            .map_err(map_interact_err)?
    }

    async fn prepare<T, F>(
        &self,
        sql: impl AsRef<str> + Send + 'static,
        f: F,
    ) -> rusqlite::Result<T>
    where
        T: Send + 'static,
        F: FnOnce(Statement<'_>) -> rusqlite::Result<T> + Send + 'static,
    {
        self.interact(move |conn| f(conn.prepare(sql.as_ref())?)).await.map_err(map_interact_err)?
    }

    async fn query_row<T, P, F>(
        &self,
        sql: impl AsRef<str> + Send + 'static,
        params: P,
        f: F,
    ) -> rusqlite::Result<T>
    where
        T: Send + 'static,
        P: Params + Send + 'static,
        F: FnOnce(&Row<'_>) -> rusqlite::Result<T> + Send + 'static,
    {
        self.interact(move |conn| conn.query_row(sql.as_ref(), params, f))
            .await
            .map_err(map_interact_err)?
    }

    async fn query_one<T, P, F>(
        &self,
        sql: impl AsRef<str> + Send + 'static,
        params: P,
        f: F,
    ) -> rusqlite::Result<T>
    where
        T: Send + 'static,
        P: Params + Send + 'static,
        F: FnOnce(&Row<'_>) -> rusqlite::Result<T> + Send + 'static,
    {
        self.interact(move |conn| conn.query_one(sql.as_ref(), params, f))
            .await
            .map_err(map_interact_err)?
    }

    async fn query_many<T, P, F>(
        &self,
        sql: impl AsRef<str> + Send + 'static,
        params: P,
        f: F,
    ) -> rusqlite::Result<Vec<T>>
    where
        T: Send + 'static,
        P: Params + Send + 'static,
        F: FnMut(&Row<'_>) -> rusqlite::Result<T> + Send + 'static,
    {
        self.interact(move |conn| {
            let mut stmt = conn.prepare(sql.as_ref())?;
            stmt.query_and_then(params, f)?.collect()
        })
        .await
        .map_err(map_interact_err)?
    }

    async fn with_transaction<T, E, F>(&self, f: F) -> Result<T, E>
    where
        T: Send + 'static,
        E: From<rusqlite::Error> + Send + 'static,
        F: FnOnce(&Transaction<'_>) -> Result<T, E> + Send + 'static,
    {
        self.interact(move |conn| {
            let txn = conn.transaction()?;
            let result = f(&txn)?;
            txn.commit()?;
            Ok(result)
        })
        .await
        .map_err(map_interact_err)
        .map_err(E::from)?
    }

    async fn chunk_large_query_over<Query, Res>(
        &self,
        keys_to_chunk: Vec<Key>,
        result_capacity: Option<usize>,
        do_query: Query,
    ) -> Result<Vec<Res>>
    where
        Res: Send + 'static,
        Query: Fn(&Transaction<'_>, ChunkFromLargeQuery<Key>) -> Result<Vec<Res>> + Send + 'static,
    {
        self.with_transaction(move |txn| {
            txn.chunk_large_query_over(keys_to_chunk, result_capacity, do_query)
        })
        .await
    }
}

/// Map an [`InteractError`] into a [`rusqlite::Error`].
///
/// An [`InteractError::Panic`] will panic. An [`InteractError::Cancelled`] will
/// generate a [`rusqlite::Error::SqliteFailure`] with the
/// [`rusqlite::ffi::SQLITE_ABORT`] code.
fn map_interact_err(error: InteractError) -> rusqlite::Error {
    match error {
        InteractError::Panic(p) => panic!("{p:?}"),
        InteractError::Cancelled => rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ABORT),
            None,
        ),
    }
}

pub(crate) trait SqliteTransactionExt {
    /// See [`SqliteAsyncConnExt::chunk_large_query_over`].
    fn chunk_large_query_over<Key, Query, Res>(
        &self,
        keys_to_chunk: Vec<Key>,
        result_capacity: Option<usize>,
        do_query: Query,
    ) -> Result<Vec<Res>>
    where
        Res: Send + 'static,
        Query: Fn(&Transaction<'_>, ChunkFromLargeQuery<Key>) -> Result<Vec<Res>> + Send + 'static;
}

/// Represent the new chunk prepared by
/// [`SqliteAsyncConnExt::chunk_large_query_over`] or
/// [`SqliteTransactionExt::chunk_large_query_over`].
#[repr(transparent)]
pub(crate) struct ChunkFromLargeQuery<Key>(Vec<Key>);

impl<Key> IntoIterator for ChunkFromLargeQuery<Key> {
    type Item = Key;
    type IntoIter = std::vec::IntoIter<Key>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<Key> ChunkFromLargeQuery<Key> {
    /// Return the correct number of host parameters (the `?` variable in an SQL
    /// query) equals to the size of the chunk.
    pub fn host_parameters(&self) -> impl fmt::Display + use<Key> {
        host_parameters(self.0.len())
    }

    /// Iterate over the keys in the chunk, by reference.
    ///
    /// To get an owned iterator, see the `IntoIterator` implementation.
    pub fn iter(&self) -> std::slice::Iter<'_, Key> {
        self.0.iter()
    }
}

impl SqliteTransactionExt for Transaction<'_> {
    fn chunk_large_query_over<Key, Query, Res>(
        &self,
        mut keys_to_chunk: Vec<Key>,
        result_capacity: Option<usize>,
        do_query: Query,
    ) -> Result<Vec<Res>>
    where
        Res: Send + 'static,
        Query: Fn(&Transaction<'_>, ChunkFromLargeQuery<Key>) -> Result<Vec<Res>> + Send + 'static,
    {
        // Divide by 2 to allow space for more static parameters (not part of
        // `keys_to_chunk`).
        let maximum_chunk_size = self.limit(Limit::SQLITE_LIMIT_VARIABLE_NUMBER)? / 2;
        let maximum_chunk_size: usize = maximum_chunk_size
            .try_into()
            .map_err(|_| Error::SqliteMaximumVariableNumber(maximum_chunk_size))?;

        if keys_to_chunk.len() < maximum_chunk_size {
            // Chunking isn't necessary.
            let chunk = keys_to_chunk;

            Ok(do_query(self, ChunkFromLargeQuery(chunk))?)
        } else {
            // Chunking _is_ necessary.

            // Define the accumulator.
            let capacity = result_capacity.unwrap_or_default();
            let mut all_results = Vec::with_capacity(capacity);

            while !keys_to_chunk.is_empty() {
                // Chunk and run the query.
                let tail = keys_to_chunk.split_off(min(keys_to_chunk.len(), maximum_chunk_size));
                let chunk = keys_to_chunk;
                keys_to_chunk = tail;

                all_results.extend(do_query(self, ChunkFromLargeQuery(chunk))?);
            }

            Ok(all_results)
        }
    }
}

/// Extension trait for a [`rusqlite::Connection`] that contains a key-value
/// table named `kv`.
///
/// The table should be created like this:
///
/// ```sql
/// CREATE TABLE "kv" (
///     "key" TEXT PRIMARY KEY NOT NULL,
///     "value" BLOB NOT NULL
/// );
/// ```
pub(crate) trait SqliteKeyValueStoreConnExt {
    /// Store the given value for the given key.
    fn set_kv(&self, key: &str, value: &[u8]) -> rusqlite::Result<()>;

    /// Store the given value for the given key by serializing it.
    fn set_serialized_kv<T: Serialize + Send>(&self, key: &str, value: T) -> Result<()> {
        let serialized_value = rmp_serde::to_vec_named(&value)?;
        self.set_kv(key, &serialized_value)?;

        Ok(())
    }

    /// Removes the current key and value if exists.
    fn clear_kv(&self, key: &str) -> rusqlite::Result<()>;

    /// Set the version of the database.
    fn set_db_version(&self, version: u8) -> rusqlite::Result<()> {
        self.set_kv("version", &[version])
    }
}

impl SqliteKeyValueStoreConnExt for rusqlite::Connection {
    fn set_kv(&self, key: &str, value: &[u8]) -> rusqlite::Result<()> {
        self.execute(
            "INSERT INTO kv VALUES (?1, ?2) ON CONFLICT (key) DO UPDATE SET value = ?2",
            (key, value),
        )?;
        Ok(())
    }

    fn clear_kv(&self, key: &str) -> rusqlite::Result<()> {
        self.execute("DELETE FROM kv WHERE key = ?1", (key,))?;
        Ok(())
    }
}

/// Extension trait for an [`SqliteAsyncConn`] that contains a key-value
/// table named `kv`.
///
/// The table should be created like this:
///
/// ```sql
/// CREATE TABLE "kv" (
///     "key" TEXT PRIMARY KEY NOT NULL,
///     "value" BLOB NOT NULL
/// );
/// ```
#[async_trait]
pub(crate) trait SqliteKeyValueStoreAsyncConnExt: SqliteAsyncConnExt {
    /// Whether the `kv` table exists in this database.
    async fn kv_table_exists(&self) -> rusqlite::Result<bool> {
        self.query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'kv')",
            (),
            |row| row.get(0),
        )
        .await
    }

    /// Get the stored value for the given key.
    async fn get_kv(&self, key: &str) -> rusqlite::Result<Option<Vec<u8>>> {
        let key = key.to_owned();
        self.query_row("SELECT value FROM kv WHERE key = ?", (key,), |row| row.get(0))
            .await
            .optional()
    }

    /// Get the stored serialized value for the given key.
    async fn get_serialized_kv<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let Some(bytes) = self.get_kv(key).await? else {
            return Ok(None);
        };

        Ok(Some(rmp_serde::from_slice(&bytes)?))
    }

    /// Store the given value for the given key.
    async fn set_kv(&self, key: &str, value: Vec<u8>) -> rusqlite::Result<()>;

    /// Store the given value for the given key by serializing it.
    async fn set_serialized_kv<T: Serialize + Send + 'static>(
        &self,
        key: &str,
        value: T,
    ) -> Result<()>;

    /// Clears the given value for the given key.
    async fn clear_kv(&self, key: &str) -> rusqlite::Result<()>;

    /// Get the version of the database.
    async fn db_version(&self) -> Result<u8, OpenStoreError> {
        let kv_exists = self.kv_table_exists().await.map_err(OpenStoreError::LoadVersion)?;

        if kv_exists {
            match self.get_kv("version").await.map_err(OpenStoreError::LoadVersion)?.as_deref() {
                Some([v]) => Ok(*v),
                Some(_) => Err(OpenStoreError::InvalidVersion),
                None => Err(OpenStoreError::MissingVersion),
            }
        } else {
            Ok(0)
        }
    }

    /// Get the [`StoreCipher`] of the database or create it.
    async fn get_or_create_store_cipher(
        &self,
        secret: Secret,
    ) -> Result<StoreCipher, OpenStoreError> {
        const STORAGE_KEY: &str = "cipher";

        let encrypted_cipher =
            self.get_kv(STORAGE_KEY).await.map_err(OpenStoreError::LoadCipher)?;

        let cipher = if let Some(encrypted) = encrypted_cipher {
            match &secret {
                Secret::PassPhrase(passphrase) => StoreCipher::import(passphrase, &encrypted)?,
                Secret::Key(key) => StoreCipher::import_with_key(key.as_slice(), &encrypted)?,
                Secret::HighEntropyPassPhrase { key, base64_variant } => {
                    // Element X apps used the passphrase-based secret variant even though the
                    // underlying secret was a randomly generated key.
                    //
                    // The `HighEntropyPassPhrase` variant was introduced to migrate these cipher
                    // exports from a passphrase-based setup to a key-based setup.
                    //
                    // We first attempt to decrypt the cipher using the provided high-entropy
                    // passphrase as a key. If this results in a KDF mismatch, it indicates that
                    // the export was originally encrypted with the high-entropy passphrase being
                    // used as a passphrase instead.
                    //
                    // In that case, we re-encrypt the cipher using the key-based setup. On the next
                    // import attempt, `import_with_key()` can then decrypt it successfully.
                    match StoreCipher::import_with_key(key.as_slice(), &encrypted) {
                        Ok(cipher) => cipher,
                        Err(matrix_sdk_store_encryption::Error::KdfMismatch) => {
                            // EX generated a byte array for a key but converted it into a string by
                            // base64 encoding it to use it as a passphrase. So let's do that as
                            // well.
                            //
                            // Funnily enough, iOS used padded base64, while Android used unpadded.
                            let mut base64_passphrase = match base64_variant {
                                crate::Base64Variant::Unpadded => base64_encode(key),
                                crate::Base64Variant::Padded => {
                                    base64::prelude::BASE64_STANDARD.encode(key)
                                }
                            };

                            let cipher = StoreCipher::import(&base64_passphrase, &encrypted);
                            base64_passphrase.zeroize();

                            let cipher = cipher?;
                            let export = cipher.export_with_key(key.as_slice())?;

                            self.set_kv(STORAGE_KEY, export)
                                .await
                                .map_err(OpenStoreError::SaveCipher)?;

                            cipher
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
            }
        } else {
            let cipher = StoreCipher::new()?;

            let export = match &secret {
                Secret::PassPhrase(passphrase) => {
                    #[cfg(not(test))]
                    {
                        cipher.export(passphrase)
                    }
                    #[cfg(test)]
                    {
                        cipher._insecure_export_fast_for_testing(passphrase)
                    }
                }
                Secret::Key(key) => cipher.export_with_key(key.as_slice()),
                Secret::HighEntropyPassPhrase { key, .. } => cipher.export_with_key(key.as_slice()),
            }?;

            self.set_kv(STORAGE_KEY, export).await.map_err(OpenStoreError::SaveCipher)?;

            cipher
        };

        Ok(cipher)
    }
}

#[async_trait]
impl SqliteKeyValueStoreAsyncConnExt for SqliteAsyncConn {
    async fn set_kv(&self, key: &str, value: Vec<u8>) -> rusqlite::Result<()> {
        let key = key.to_owned();
        self.interact(move |conn| conn.set_kv(&key, &value)).await.unwrap()?;

        Ok(())
    }

    async fn set_serialized_kv<T: Serialize + Send + 'static>(
        &self,
        key: &str,
        value: T,
    ) -> Result<()> {
        let key = key.to_owned();
        self.interact(move |conn| conn.set_serialized_kv(&key, value)).await.unwrap()?;

        Ok(())
    }

    async fn clear_kv(&self, key: &str) -> rusqlite::Result<()> {
        let key = key.to_owned();
        self.interact(move |conn| conn.clear_kv(&key)).await.unwrap()?;

        Ok(())
    }
}

/// Repeat `?` n times, where n is defined by `count`. `?` are comma-separated.
pub(crate) fn host_parameters(count: usize) -> impl fmt::Display {
    assert_ne!(count, 0, "Can't generate zero host parameters");

    iter::repeat_n("?", count).format(",")
}

/// Convert the given `SystemTime` to a timestamp, as the number of seconds
/// since Unix Epoch.
///
/// Returns an `i64` as it is the numeric type used by SQLite.
pub(crate) fn time_to_timestamp(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|d| d.as_secs().try_into().ok())
        // It is unlikely to happen unless the time on the system is seriously wrong, but we always
        // need a value.
        .unwrap_or(0)
}

/// Trait for a store that can encrypt its values, based on the presence of a
/// cipher or not.
///
/// A single method must be implemented: `get_cypher`, which returns an optional
/// cipher.
///
/// All the other methods come for free, based on the implementation of
/// `get_cypher`.
pub(crate) trait EncryptableStore {
    fn get_cypher(&self) -> Option<&StoreCipher>;

    /// If the store is using encryption, this will hash the given key. This is
    /// useful when we need to do queries against a given key, but we don't
    /// need to store the key in plain text (i.e. it's not both a key and a
    /// value).
    fn encode_key(&self, table_name: &str, key: impl AsRef<[u8]>) -> Key {
        let bytes = key.as_ref();
        if let Some(store_cipher) = self.get_cypher() {
            Key::Hashed(store_cipher.hash_key(table_name, bytes))
        } else {
            Key::Plain(bytes.to_owned())
        }
    }

    fn encode_value<V>(&self, value: V) -> Result<Vec<u8>>
    where
        V: EncryptableValue + Into<Vec<u8>>,
    {
        if let Some(key) = self.get_cypher() {
            let encrypted = key.encrypt_value_data(value)?;
            Ok(rmp_serde::to_vec_named(&encrypted)?)
        } else {
            Ok(value.into())
        }
    }

    fn decode_value<'a>(&self, value: &'a [u8]) -> Result<Cow<'a, [u8]>> {
        if let Some(key) = self.get_cypher() {
            let encrypted = rmp_serde::from_slice(value)?;
            let decrypted = key.decrypt_value_data(encrypted)?;
            Ok(Cow::Owned(decrypted))
        } else {
            Ok(Cow::Borrowed(value))
        }
    }

    fn serialize_value(&self, value: &impl Serialize) -> Result<Vec<u8>> {
        let serialized = rmp_serde::to_vec_named(value)?;
        self.encode_value(serialized)
    }

    fn deserialize_value<T: DeserializeOwned>(&self, value: &[u8]) -> Result<T> {
        let decoded = self.decode_value(value)?;
        Ok(rmp_serde::from_slice(&decoded)?)
    }

    fn serialize_json(&self, value: &impl Serialize) -> Result<Vec<u8>> {
        let serialized = serde_json::to_vec(value)?;
        self.encode_value(serialized)
    }

    fn deserialize_json<T: DeserializeOwned>(&self, data: &[u8]) -> Result<T> {
        let decoded = self.decode_value(data)?;

        let json_deserializer = &mut serde_json::Deserializer::from_slice(&decoded);

        serde_path_to_error::deserialize(json_deserializer).map_err(|err| {
            let raw_json: Option<Raw<serde_json::Value>> = serde_json::from_slice(&decoded).ok();

            let target_type = std::any::type_name::<T>();
            let serde_path = err.path().to_string();

            error!(
                sentry = true,
                %err,
                "Failed to deserialize {target_type} in a store: {serde_path}",
            );

            if let Some(raw) = raw_json {
                if let Some(room_id) = raw.get_field::<OwnedRoomId>("room_id").ok().flatten() {
                    warn!("Found a room id in the source data to deserialize: {room_id}");
                }
                if let Some(event_id) = raw.get_field::<OwnedEventId>("event_id").ok().flatten() {
                    warn!("Found an event id in the source data to deserialize: {event_id}");
                }
            }

            err.into_inner().into()
        })
    }
}

#[cfg(test)]
mod unit_tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn test_can_generate_host_parameters() {
        assert_eq!(host_parameters(1).to_string(), "?");
        assert_eq!(host_parameters(2).to_string(), "?,?");
        assert_eq!(host_parameters(5).to_string(), "?,?,?,?,?");
    }

    #[test]
    #[should_panic(expected = "Can't generate zero host parameters")]
    fn test_generating_zero_host_parameters_panics() {
        host_parameters(0);
    }

    #[test]
    fn test_time_to_timestamp() {
        assert_eq!(time_to_timestamp(SystemTime::UNIX_EPOCH), 0);
        assert_eq!(time_to_timestamp(SystemTime::UNIX_EPOCH + Duration::from_secs(60)), 60);

        // Fallback value on overflow.
        assert_eq!(time_to_timestamp(SystemTime::UNIX_EPOCH - Duration::from_secs(60)), 0);
    }
}
