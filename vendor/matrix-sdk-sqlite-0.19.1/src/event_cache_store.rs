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

//! An SQLite-based backend for the [`EventCacheStore`].

use std::{
    collections::HashMap,
    fmt,
    iter::once,
    ops::{Deref, Not},
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use deadpool::managed::PoolConfig;
use matrix_sdk_base::{
    cross_process_lock::CrossProcessLockGeneration,
    deserialized_responses::TimelineEvent,
    event_cache::{
        Event, Gap,
        store::{EventCacheStore, extract_event_relation},
        thread::ThreadInfo,
    },
    linked_chunk::{
        ChunkContent, ChunkIdentifier, ChunkIdentifierGenerator, ChunkMetadata, LinkedChunkId,
        Position, RawChunk, Update,
    },
    timer,
};
use matrix_sdk_store_encryption::StoreCipher;
use ruma::{
    EventId, MilliSecondsSinceUnixEpoch, OwnedEventId, RoomId, events::relation::RelationType,
};
use rusqlite::{
    OptionalExtension, ToSql, Transaction, TransactionBehavior, params, params_from_iter,
};
use tokio::{
    fs,
    sync::{Mutex, OwnedMutexGuard},
};
use tracing::{debug, error, instrument, trace};

use crate::{
    OpenStoreError, RuntimeConfig, Secret, SqliteStoreConfig,
    connection::{self, Connection as SqliteAsyncConn, Pool as SqlitePool, SqliteConnections},
    error::{Error, Result},
    utils::{
        EncryptableStore, Key, SqliteAsyncConnExt, SqliteKeyValueStoreAsyncConnExt,
        SqliteKeyValueStoreConnExt, SqliteTransactionExt, host_parameters,
    },
};

mod keys {
    // Tables
    pub const LINKED_CHUNKS: &str = "linked_chunks";
    pub const EVENTS: &str = "events";
}

/// The database name.
const DATABASE_NAME: &str = "matrix-sdk-event-cache.sqlite3";

/// The string used to identify a chunk of type events, in the `type` field in
/// the database.
const CHUNK_TYPE_EVENT_TYPE_STRING: &str = "E";
/// The string used to identify a chunk of type gap, in the `type` field in the
/// database.
const CHUNK_TYPE_GAP_TYPE_STRING: &str = "G";

/// Type to support (de)encryption of keys and values for the
/// [`SqliteEventCacheStore`].
///
/// See the [`SqliteEventCacheStore::encryption`] field.
struct Encryption {
    cipher: Option<StoreCipher>,
}

impl Encryption {
    fn encode_event(&self, event: &TimelineEvent) -> Result<EncodedEvent> {
        let serialized = serde_json::to_vec(event)?;

        // Extract the relationship info here.
        let raw_event = event.raw();
        let (relates_to, rel_type) = extract_event_relation(raw_event).unzip();

        // The content may be encrypted.
        let content = self.encode_value(serialized)?;

        Ok(EncodedEvent {
            content,
            rel_type,
            relates_to: relates_to
                .map(|relates_to| self.encode_event_id(keys::EVENTS, &relates_to)),
        })
    }

    fn decode_event(&self, raw_encoded_event: &[u8]) -> Result<Event> {
        Ok(serde_json::from_slice(&self.decode_value(raw_encoded_event)?)?)
    }

    /// Encode the event ID as a _key_: it cannot be decoded, but this is
    /// stable.
    fn encode_event_id(&self, table_name: &str, event_id: &EventId) -> Key {
        self.encode_key(table_name, event_id)
    }

    /// Encode the room ID as a _key_: it cannot be decoded, but this is
    /// stable.
    fn encode_room_id(&self, table_name: &str, room_id: &RoomId) -> Key {
        self.encode_key(table_name, room_id)
    }

    /// Encode a [`LinkedChunkId`].
    fn encode_linked_chunk(&self, table_name: &str, linked_chunk_id: &LinkedChunkId<'_>) -> Key {
        self.encode_key(table_name, linked_chunk_id.storage_key())
    }

    /// Encode a thread ID (which is an [`EventId`]).
    fn encode_thread_id(&self, thread_id: &EventId) -> Result<Vec<u8>> {
        self.encode_value(String::from(thread_id.as_str()))
    }

    /// Decode a thread ID (which is an [`EventId`]).
    #[cfg(test)]
    fn decode_thread_id(&self, encoded_thread_id: &[u8]) -> Result<OwnedEventId> {
        let as_slice = self.decode_value(encoded_thread_id)?;
        let as_str = str::from_utf8(as_slice.as_ref())?;

        Ok(EventId::parse(as_str)?)
    }

    /// Encode a [`ThreadInfo`]).
    fn encode_thread_info(&self, thread_info: &ThreadInfo) -> Result<Vec<u8>> {
        self.encode_value(serde_json::to_vec(thread_info)?)
    }

    /// Decode a [`ThreadInfo`].
    fn decode_thread_info(&self, encoded_thread_info: &[u8]) -> Result<ThreadInfo> {
        Ok(serde_json::from_slice(&self.decode_value(encoded_thread_info)?)?)
    }
}

impl EncryptableStore for Encryption {
    fn get_cypher(&self) -> Option<&StoreCipher> {
        self.cipher.as_ref()
    }
}

/// An SQLite-based event cache store.
#[derive(Clone)]
pub struct SqliteEventCacheStore {
    /// Type to encrypt keys and values.
    encryption: Arc<Encryption>,

    /// `Some` when active, `None` when closed.
    connections: Arc<Mutex<Option<SqliteConnections>>>,

    /// Retained so we can rebuild the pool on reopen.
    db_path: PathBuf,

    /// Retained so we can rebuild the pool on reopen.
    pool_config: PoolConfig,

    /// Retained so we can re-apply runtime config on reopen.
    runtime_config: RuntimeConfig,
}

#[cfg(not(tarpaulin_include))]
impl fmt::Debug for SqliteEventCacheStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqliteEventCacheStore").finish_non_exhaustive()
    }
}

impl SqliteEventCacheStore {
    /// Open the SQLite-based event cache store at the given path using the
    /// given passphrase to encrypt private data.
    pub async fn open(
        path: impl AsRef<Path>,
        passphrase: Option<&str>,
    ) -> Result<Self, OpenStoreError> {
        Self::open_with_config(&SqliteStoreConfig::new(path).passphrase(passphrase)).await
    }

    /// Open the SQLite-based event cache store at the given path using the
    /// given key to encrypt private data.
    pub async fn open_with_key(
        path: impl AsRef<Path>,
        key: Option<&[u8]>,
    ) -> Result<Self, OpenStoreError> {
        Self::open_with_config(&SqliteStoreConfig::new(path).key(key)).await
    }

    /// Open the SQLite-based event cache store with the config open config.
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

    /// Open an SQLite-based event cache store using the given SQLite database
    /// pool. The given secret will be used to encrypt private data.
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

        let cipher = match secret {
            Some(s) => Some(conn.get_or_create_store_cipher(s).await?),
            None => None,
        };

        let connections = SqliteConnections {
            pool,
            // Use `conn` as our selected write connection.
            write_connection: Arc::new(Mutex::new(conn)),
        };

        Ok(Self {
            encryption: Arc::new(Encryption { cipher }),
            connections: Arc::new(Mutex::new(Some(connections))),
            db_path,
            pool_config,
            runtime_config,
        })
    }

    /// Acquire a connection for executing read operations.
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

    /// Acquire a connection for executing write operations.
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

    fn map_row_to_chunk(
        row: &rusqlite::Row<'_>,
    ) -> Result<(u64, Option<u64>, Option<u64>, String), rusqlite::Error> {
        Ok((
            row.get::<_, u64>(0)?,
            row.get::<_, Option<u64>>(1)?,
            row.get::<_, Option<u64>>(2)?,
            row.get::<_, String>(3)?,
        ))
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
        connection::close_connections(&self.connections, "Event cache store").await;
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

struct EncodedEvent {
    content: Vec<u8>,
    rel_type: Option<String>,
    relates_to: Option<Key>,
}

trait TransactionExtForLinkedChunks {
    fn rebuild_chunk(
        &self,
        encryption: &Encryption,
        linked_chunk_id: &Key,
        previous: Option<u64>,
        index: u64,
        next: Option<u64>,
        chunk_type: &str,
    ) -> Result<RawChunk<Event, Gap>>;

    fn load_gap_content(
        &self,
        encryption: &Encryption,
        linked_chunk_id: &Key,
        chunk_id: ChunkIdentifier,
    ) -> Result<Gap>;

    fn load_events_content(
        &self,
        encryption: &Encryption,
        linked_chunk_id: &Key,
        chunk_id: ChunkIdentifier,
    ) -> Result<Vec<Event>>;
}

impl TransactionExtForLinkedChunks for Transaction<'_> {
    fn rebuild_chunk(
        &self,
        encryption: &Encryption,
        linked_chunk_id: &Key,
        previous: Option<u64>,
        id: u64,
        next: Option<u64>,
        chunk_type: &str,
    ) -> Result<RawChunk<Event, Gap>> {
        let previous = previous.map(ChunkIdentifier::new);
        let next = next.map(ChunkIdentifier::new);
        let id = ChunkIdentifier::new(id);

        match chunk_type {
            CHUNK_TYPE_GAP_TYPE_STRING => {
                // It's a gap!
                let gap = self.load_gap_content(encryption, linked_chunk_id, id)?;
                Ok(RawChunk { content: ChunkContent::Gap(gap), previous, identifier: id, next })
            }

            CHUNK_TYPE_EVENT_TYPE_STRING => {
                // It's events!
                let events = self.load_events_content(encryption, linked_chunk_id, id)?;
                Ok(RawChunk {
                    content: ChunkContent::Items(events),
                    previous,
                    identifier: id,
                    next,
                })
            }

            other => {
                // It's an error!
                Err(Error::InvalidData {
                    details: format!("a linked chunk has an unknown type {other}"),
                })
            }
        }
    }

    fn load_gap_content(
        &self,
        encryption: &Encryption,
        linked_chunk_id: &Key,
        chunk_id: ChunkIdentifier,
    ) -> Result<Gap> {
        // There's at most one row for it in the database, so a call to `query_one` is
        // sufficient.
        let encoded_prev_token: Vec<u8> = self.query_one(
            "SELECT prev_token FROM gap_chunks WHERE chunk_id = ? AND linked_chunk_id = ?",
            (chunk_id.index(), &linked_chunk_id),
            |row| row.get(0),
        )?;
        let prev_token_bytes = encryption.decode_value(&encoded_prev_token)?;
        let prev_token = String::from_utf8(prev_token_bytes.into_owned())?;
        Ok(Gap { token: prev_token })
    }

    fn load_events_content(
        &self,
        encryption: &Encryption,
        linked_chunk_id: &Key,
        chunk_id: ChunkIdentifier,
    ) -> Result<Vec<Event>> {
        // Retrieve all the events from the database.
        let mut events = Vec::new();

        for event_data in self
            .prepare(
                "SELECT events.content \
                    FROM event_chunks ec, events \
                    WHERE events.event_id = ec.event_id AND ec.chunk_id = ? AND ec.linked_chunk_id = ? \
                    ORDER BY ec.position ASC",
            )?
            .query_map((chunk_id.index(), &linked_chunk_id), |row| row.get::<_, Vec<u8>>(0))?
        {
            events.push(encryption.decode_event(&event_data?)?);
        }

        Ok(events)
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
            txn.execute_batch(include_str!("../migrations/event_cache_store/001_init.sql"))?;
            txn.set_db_version(1)
        })
        .await?;
    }

    if version < 2 {
        debug!("Upgrading database to version 2");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!("../migrations/event_cache_store/002_lease_locks.sql"))?;
            txn.set_db_version(2)
        })
        .await?;
    }

    if version < 3 {
        debug!("Upgrading database to version 3");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!("../migrations/event_cache_store/003_events.sql"))?;
            txn.set_db_version(3)
        })
        .await?;
    }

    if version < 4 {
        debug!("Upgrading database to version 4");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/event_cache_store/004_ignore_policy.sql"
            ))?;
            txn.set_db_version(4)
        })
        .await?;
    }

    if version < 5 {
        debug!("Upgrading database to version 5");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/event_cache_store/005_events_index_on_event_id.sql"
            ))?;
            txn.set_db_version(5)
        })
        .await?;
    }

    if version < 6 {
        debug!("Upgrading database to version 6");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!("../migrations/event_cache_store/006_events.sql"))?;
            txn.set_db_version(6)
        })
        .await?;
    }

    if version < 7 {
        debug!("Upgrading database to version 7");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/event_cache_store/007_event_chunks.sql"
            ))?;
            txn.set_db_version(7)
        })
        .await?;
    }

    if version < 8 {
        debug!("Upgrading database to version 8");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/event_cache_store/008_linked_chunk_id.sql"
            ))?;
            txn.set_db_version(8)
        })
        .await?;
    }

    if version < 9 {
        debug!("Upgrading database to version 9");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/event_cache_store/009_related_event_index.sql"
            ))?;
            txn.set_db_version(9)
        })
        .await?;
    }

    if version < 10 {
        debug!("Upgrading database to version 10");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!("../migrations/event_cache_store/010_drop_media.sql"))?;
            txn.set_db_version(10)
        })
        .await?;

        if version >= 1 {
            // Defragment the DB and optimize its size on the filesystem now that we removed
            // the media cache.
            conn.vacuum().await?;
        }
    }

    if version < 11 {
        debug!("Upgrading database to version 11");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/event_cache_store/011_empty_event_cache.sql"
            ))?;
            txn.set_db_version(11)
        })
        .await?;
    }

    if version < 12 {
        debug!("Upgrading database to version 12");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/event_cache_store/012_store_event_type.sql"
            ))?;
            txn.set_db_version(12)
        })
        .await?;
    }

    if version < 13 {
        debug!("Upgrading database to version 13");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/event_cache_store/013_lease_locks_with_generation.sql"
            ))?;
            txn.set_db_version(13)
        })
        .await?;
    }

    if version < 14 {
        debug!("Upgrading database to version 14");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/event_cache_store/014_event_chunks_event_id_index.sql"
            ))?;
            txn.set_db_version(14)
        })
        .await?;
    }

    if version < 15 {
        debug!("Upgrading database to version 15");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/event_cache_store/015_event_ids_are_encoded.sql"
            ))?;
            txn.set_db_version(15)
        })
        .await?;
    }

    if version < 16 {
        debug!("Upgrading database to version 16");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!("../migrations/event_cache_store/016_threads.sql"))?;
            txn.set_db_version(16)
        })
        .await?;
    }

    if version < 17 {
        debug!("Upgrading database to version 17");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/event_cache_store/017_threads_with_thread_infos.sql"
            ))?;
            txn.set_db_version(17)
        })
        .await?;
    }

    if version < 18 {
        debug!("Upgrading database to version 18");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!("../migrations/event_cache_store/018_event_chunks_unique_linked_chunk_id_event_id.sql"))?;
            txn.set_db_version(18)
        })
        .await?;
    }

    Ok(())
}

#[async_trait]
impl EventCacheStore for SqliteEventCacheStore {
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
                    "INSERT INTO lease_locks (key, holder, expiration) \
                    VALUES (?1, ?2, ?3) \
                    ON CONFLICT (key) \
                    DO \
                        UPDATE SET \
                            holder = excluded.holder, \
                            expiration = excluded.expiration, \
                            generation = \
                                CASE holder \
                                    WHEN excluded.holder THEN generation \
                                    ELSE generation + 1 \
                                END \
                        WHERE \
                            holder = excluded.holder \
                            OR expiration < ?4 \
                    RETURNING generation",
                    (key, holder, expiration, now),
                    |row| row.get(0),
                )
                .optional()
            })
            .await?;

        Ok(generation)
    }

    #[instrument(skip(self, updates))]
    async fn handle_linked_chunk_updates(
        &self,
        linked_chunk_id: LinkedChunkId<'_>,
        updates: Vec<Update<Event, Gap>>,
    ) -> Result<(), Self::Error> {
        let _timer = timer!("method");

        let hashed_linked_chunk_id =
            self.encryption.encode_linked_chunk(keys::LINKED_CHUNKS, &linked_chunk_id);
        let hashed_room_id =
            self.encryption.encode_room_id(keys::EVENTS, linked_chunk_id.room_id());
        let encryption = self.encryption.clone();

        // Use a single transaction throughout this function, so that either all updates
        // work, or none is taken into account.
        with_immediate_transaction(self, move |txn| {
            for update in updates {
                match update {
                    Update::NewItemsChunk { previous, new, next } => {
                        let previous = previous.as_ref().map(ChunkIdentifier::index);
                        let new = new.index();
                        let next = next.as_ref().map(ChunkIdentifier::index);

                        trace!("new events chunk (prev={previous:?}, i={new}, next={next:?})");

                        insert_chunk(
                            txn,
                            &hashed_linked_chunk_id,
                            previous,
                            new,
                            next,
                            CHUNK_TYPE_EVENT_TYPE_STRING,
                        )?;
                    }

                    Update::NewGapChunk { previous, new, next, gap } => {
                        let hashed_prev_token = encryption.encode_value(gap.token)?;

                        let previous = previous.as_ref().map(ChunkIdentifier::index);
                        let new = new.index();
                        let next = next.as_ref().map(ChunkIdentifier::index);

                        trace!("new gap chunk (prev={previous:?}, i={new}, next={next:?})");

                        // Insert the chunk as a gap.
                        insert_chunk(
                            txn,
                            &hashed_linked_chunk_id,
                            previous,
                            new,
                            next,
                            CHUNK_TYPE_GAP_TYPE_STRING,
                        )?;

                        // Insert the gap's value.
                        txn.execute(
                            r#"
                            INSERT INTO gap_chunks(chunk_id, linked_chunk_id, prev_token)
                            VALUES (?, ?, ?)
                        "#,
                            (new, &hashed_linked_chunk_id, hashed_prev_token),
                        )?;
                    }

                    Update::RemoveChunk(chunk_identifier) => {
                        let chunk_id = chunk_identifier.index();

                        trace!("removing chunk @ {chunk_id}");

                        // Find chunk to delete.
                        let (previous, next): (Option<usize>, Option<usize>) = txn.query_one(
                            "SELECT previous, next FROM linked_chunks WHERE id = ? AND linked_chunk_id = ?",
                            (chunk_id, &hashed_linked_chunk_id),
                            |row| Ok((row.get(0)?, row.get(1)?))
                        )?;

                        // Replace its previous' next to its own next.
                        if let Some(previous) = previous {
                            txn.execute("UPDATE linked_chunks SET next = ? WHERE id = ? AND linked_chunk_id = ?", (next, previous, &hashed_linked_chunk_id))?;
                        }

                        // Replace its next' previous to its own previous.
                        if let Some(next) = next {
                            txn.execute("UPDATE linked_chunks SET previous = ? WHERE id = ? AND linked_chunk_id = ?", (previous, next, &hashed_linked_chunk_id))?;
                        }

                        // Now delete it, and let cascading delete corresponding entries in the
                        // other data tables.
                        txn.execute("DELETE FROM linked_chunks WHERE id = ? AND linked_chunk_id = ?", (chunk_id, &hashed_linked_chunk_id))?;
                    }

                    Update::PushItems { at, items } => {
                        if items.is_empty() {
                            // Should never happens, but better be safe.
                            continue;
                        }

                        let chunk_id = at.chunk_identifier().index();

                        trace!("pushing {} items @ {chunk_id}", items.len());

                        let mut chunk_statement = txn.prepare(
                            "INSERT INTO event_chunks(chunk_id, linked_chunk_id, event_id, position) VALUES (?, ?, ?, ?)"
                        )?;

                        // Note: we use `OR REPLACE` here, because the event might have been
                        // already inserted in the database. This is the case when an event is
                        // deduplicated and moved to another position; or because it was inserted
                        // outside the context of a linked chunk (e.g. pinned event).
                        let mut content_statement = txn.prepare(
                            "INSERT OR REPLACE INTO events(room_id, event_id, event_type, session_id, content, relates_to, rel_type) VALUES (?, ?, ?, ?, ?, ?, ?)"
                        )?;

                        let invalid_event = |event: TimelineEvent| {
                            let Some(event_id) = event.event_id() else {
                                error!("Trying to push an event with no ID");
                                return None;
                            };

                            let Some(event_type) = event.kind.event_type() else {
                                error!(%event_id, "Trying to save an event with no event type");
                                return None;
                            };

                            Some((event_id.to_owned(), event_type, event))
                        };

                        for (i, (event_id, event_type, event)) in items.into_iter().filter_map(invalid_event).enumerate() {
                            let hashed_event_id = encryption.encode_event_id(
                                // For the event ID, we need a stable hash between the `events`
                                // and `event_chunks` tables. That's why we use `keys::EVENTS`
                                // even if `hashed_event_id` is sometimes only used in
                                // `events_chunks`.
                                keys::EVENTS,
                                &event_id,
                            );

                            // Table `event_chunks`.
                            {
                                let index = at.index() + i;

                                chunk_statement.execute((chunk_id, &hashed_linked_chunk_id, &hashed_event_id, index))?;
                            }

                            // Table `events`.
                            {
                                let hashed_session_id = event.kind.session_id().map(|s| encryption.encode_key(keys::EVENTS, s));
                                let hashed_event_type = encryption.encode_key(keys::EVENTS, event_type);
                                let encoded_event = encryption.encode_event(&event)?;

                                content_statement.execute((
                                    &hashed_room_id,
                                    &hashed_event_id,
                                    hashed_event_type,
                                    hashed_session_id,
                                    encoded_event.content,
                                    encoded_event.relates_to,
                                    encoded_event.rel_type
                                ))?;
                            }
                        }
                    }

                    Update::ReplaceItem { at, item: event } => {
                        let chunk_id = at.chunk_identifier().index();
                        let index = at.index();

                        trace!("replacing item @ {chunk_id}:{index}");

                        // The event ID should be the same, but just in case it changed…
                        let Some(event_id) = event.event_id().map(|event_id| event_id.to_owned()) else {
                            error!("Trying to replace an event with a new one that has no ID");
                            continue;
                        };

                        let Some(event_type) = event.kind.event_type() else {
                            error!(%event_id, "Trying to save an event with no event type");
                            continue;
                        };

                        let hashed_event_id = encryption.encode_event_id(keys::EVENTS, &event_id);

                        // Before updating the event in its chunk, we must ensure the event exists:
                        // either we insert it, or we update it. Note that it's possible to replace
                        // an event by itself (with different encryption info for example, or from
                        // UTD to decrypted, stuff like that).

                        // Table `events`.
                        {
                            let hashed_session_id = event.kind.session_id().map(|s| encryption.encode_key(keys::EVENTS, s));
                            let hashed_event_type = encryption.encode_key(keys::EVENTS, event_type);
                            let encoded_event = encryption.encode_event(&event)?;

                            txn.execute(
                                "INSERT OR REPLACE INTO events(room_id, event_id, event_type, session_id, content, relates_to, rel_type) VALUES (?, ?, ?, ?, ?, ?, ?)",
                                (
                                    &hashed_room_id,
                                    &hashed_event_id,
                                    hashed_event_type,
                                    hashed_session_id,
                                    encoded_event.content,
                                    encoded_event.relates_to,
                                    encoded_event.rel_type
                                ),
                            )?;
                        }

                        // Table `event_chunks`.
                        {
                            // Replace the event at position `index` in chunk `chunk_id` by updating the event ID.
                            txn.execute(
                                r#"UPDATE event_chunks SET event_id = ? WHERE linked_chunk_id = ? AND chunk_id = ? AND position = ?"#,
                                (&hashed_event_id, &hashed_linked_chunk_id, chunk_id, index)
                            )?;
                        }
                    }

                    Update::RemoveItem { at } => {
                        let chunk_id = at.chunk_identifier().index();
                        let index = at.index();

                        trace!("removing item @ {chunk_id}:{index}");

                        // Remove the entry in the chunk table.
                        txn.execute("DELETE FROM event_chunks WHERE linked_chunk_id = ? AND chunk_id = ? AND position = ?", (&hashed_linked_chunk_id, chunk_id, index))?;

                        // Decrement the index of each item after the one we are
                        // going to remove.
                        //
                        // Imagine we have the following events:
                        //
                        // | event_id | linked_chunk_id | chunk_id | position |
                        // |----------|-----------------|----------|----------|
                        // | $ev0     | !r0             | 42       | 0        |
                        // | $ev1     | !r0             | 42       | 1        |
                        // | $ev2     | !r0             | 42       | 2        |
                        // | $ev3     | !r0             | 42       | 3        |
                        // | $ev4     | !r0             | 42       | 4        |
                        //
                        // `$ev2` has been removed, then we end up in this
                        // state:
                        //
                        // | event_id | linked_chunk_id    | chunk_id | position |
                        // |----------|--------------------|----------|----------|
                        // | $ev0     | !r0                | 42       | 0        |
                        // | $ev1     | !r0                | 42       | 1        |
                        // |          |                    |          |          | <- no more `$ev2`
                        // | $ev3     | !r0                | 42       | 3        |
                        // | $ev4     | !r0                | 42       | 4        |
                        //
                        // We need to shift the `position` of `$ev3` and `$ev4`
                        // to `position - 1`, like so:
                        //
                        // | event_id | linked_chunk_id | chunk_id | position |
                        // |----------|-----------------|----------|----------|
                        // | $ev0     | !r0             | 42       | 0        |
                        // | $ev1     | !r0             | 42       | 1        |
                        // | $ev3     | !r0             | 42       | 2        |
                        // | $ev4     | !r0             | 42       | 3        |
                        //
                        // Usually, it boils down to run the following query:
                        //
                        // ```sql
                        // UPDATE event_chunks
                        // SET position = position - 1
                        // WHERE position > 2 AND …
                        // ```
                        //
                        // Okay. But `UPDATE` runs on rows in no particular
                        // order. It means that it can update `$ev4` before
                        // `$ev3` for example. What happens in this particular
                        // case? The `position` of `$ev4` becomes `3`, however
                        // `$ev3` already has `position = 3`. Because there
                        // is a `UNIQUE` constraint on `(linked_chunk_id, chunk_id,
                        // position)`, it will result in a constraint violation.
                        //
                        // There is **no way** to control the execution order of
                        // `UPDATE` in SQLite. To persuade yourself, try:
                        //
                        // ```sql
                        // UPDATE event_chunks
                        // SET position = position - 1
                        // FROM (
                        //     SELECT event_id
                        //     FROM event_chunks
                        //     WHERE position > 2 AND …
                        //     ORDER BY position ASC
                        // ) as ordered
                        // WHERE event_chunks.event_id = ordered.event_id
                        // ```
                        //
                        // It will fail the same way.
                        //
                        // Thus, we have 2 solutions:
                        //
                        // 1. Remove the `UNIQUE` constraint,
                        // 2. Be creative.
                        //
                        // The `UNIQUE` constraint is a safe belt. Normally, we
                        // have `event_cache::Deduplicator` that is responsible
                        // to ensure there is no duplicated event. However,
                        // relying on this is “fragile” in the sense it can
                        // contain bugs. Relying on the `UNIQUE` constraint from
                        // SQLite is more robust. It's “braces and belt” as we
                        // say here.
                        //
                        // So. We need to be creative.
                        //
                        // Many solutions exist. Amongst the most popular, we
                        // see _dropping and re-creating the index_, which is
                        // no-go for us, it's too expensive. I (@hywan) have
                        // adopted the following one:
                        //
                        // - Do `position = position - 1` but in the negative
                        //   space, so `position = -(position - 1)`. A position
                        //   cannot be negative; we are sure it is unique!
                        // - Once all candidate rows are updated, do `position =
                        //   -position` to move back to the positive space.
                        //
                        // 'told you it's gonna be creative.
                        //
                        // This solution is a hack, **but** it is a small
                        // number of operations, and we can keep the `UNIQUE`
                        // constraint in place.
                        txn.execute(
                            r#"
                                UPDATE event_chunks
                                SET position = -(position - 1)
                                WHERE linked_chunk_id = ? AND chunk_id = ? AND position > ?
                            "#,
                            (&hashed_linked_chunk_id, chunk_id, index)
                        )?;
                        txn.execute(
                            r#"
                                UPDATE event_chunks
                                SET position = -position
                                WHERE position < 0 AND linked_chunk_id = ? AND chunk_id = ?
                            "#,
                            (&hashed_linked_chunk_id, chunk_id)
                        )?;

                        // We don't remove the events from `events` purposely
                        // because they can be used by another `LinkedChunkId`.
                    }

                    Update::DetachLastItems { at } => {
                        let chunk_id = at.chunk_identifier().index();
                        let index = at.index();

                        trace!("truncating items >= {chunk_id}:{index}");

                        // Remove these entries.
                        txn.execute("DELETE FROM event_chunks WHERE linked_chunk_id = ? AND chunk_id = ? AND position >= ?", (&hashed_linked_chunk_id, chunk_id, index))?;

                        // We don't remove the events from `events` purposely
                        // because they can be used by another `LinkedChunkId`.
                    }

                    Update::Clear => {
                        trace!("clearing items");

                        // Remove chunks, and let cascading do its job.
                        txn.execute(
                            "DELETE FROM linked_chunks WHERE linked_chunk_id = ?",
                            (&hashed_linked_chunk_id,),
                        )?;

                        // We don't remove the events from `events` purposely
                        // because they can be used by another `LinkedChunkId`.
                    }

                    Update::StartReattachItems | Update::EndReattachItems => {
                        // Nothing.
                    }
                }
            }

            Ok(())
        })
        .await?;

        Ok(())
    }

    #[instrument(skip(self))]
    async fn load_all_chunks(
        &self,
        linked_chunk_id: LinkedChunkId<'_>,
    ) -> Result<Vec<RawChunk<Event, Gap>>, Self::Error> {
        let _timer = timer!("method");

        let hashed_linked_chunk_id =
            self.encryption.encode_linked_chunk(keys::LINKED_CHUNKS, &linked_chunk_id);
        let encryption = self.encryption.clone();

        let result = self
            .read()
            .await?
            .with_transaction(move |txn| -> Result<_> {
                let mut items = Vec::new();

                // Use `ORDER BY id` to get a deterministic ordering for testing purposes.
                for data in txn
                    .prepare(
                        "SELECT id, previous, next, type FROM linked_chunks WHERE linked_chunk_id = ? ORDER BY id",
                    )?
                    .query_map((&hashed_linked_chunk_id,), Self::map_row_to_chunk)?
                {
                    let (id, previous, next, chunk_type) = data?;
                    let new = txn.rebuild_chunk(
                        &encryption,
                        &hashed_linked_chunk_id,
                        previous,
                        id,
                        next,
                        chunk_type.as_str(),
                    )?;
                    items.push(new);
                }

                Ok(items)
            })
            .await?;

        Ok(result)
    }

    #[instrument(skip(self))]
    async fn load_all_chunks_metadata(
        &self,
        linked_chunk_id: LinkedChunkId<'_>,
    ) -> Result<Vec<ChunkMetadata>, Self::Error> {
        let _timer = timer!("method");

        let hashed_linked_chunk_id =
            self.encryption.encode_linked_chunk(keys::LINKED_CHUNKS, &linked_chunk_id);

        self.read()
            .await?
            .with_transaction(move |txn| -> Result<_> {
                // We want to collect the metadata about each chunk (id, next, previous), and
                // for event chunks, the number of events in it. For gaps, the
                // number of events is 0, by convention.
                //
                // We've tried different strategies over time:
                // - use a `LEFT JOIN` + `COUNT`, which was extremely inefficient because it
                //   caused a full table traversal for each chunk, including for gaps which
                //   don't have any events. This happened in
                //   https://github.com/matrix-org/matrix-rust-sdk/pull/5225.
                // - use a `CASE` statement on the chunk's type: if it's an event chunk, run an
                //   additional `SELECT` query. It was an immense improvement, but still caused
                //   one select query per event chunk. This happened in
                //   https://github.com/matrix-org/matrix-rust-sdk/pull/5411.
                //
                // The current solution is to run two queries:
                // - one to get each chunk and its number of events, by doing a single `SELECT`
                //   query over the `event_chunks` table, grouping by chunk ids. This gives us a
                //   list of `(chunk_id, num_events)` pairs, which can be transformed into a
                //   hashmap.
                // - one to get each chunk's metadata (id, previous, next, type) from the
                //   database with a `SELECT`, and then use the hashmap to get the number of
                //   events.
                //
                // This strategy minimizes the number of queries to the database, and keeps them
                // super simple, while doing a bit more processing here, which is much faster.

                let num_events_by_chunk_ids = txn
                    .prepare(
                        "SELECT ec.chunk_id, COUNT(ec.event_id) \
                        FROM event_chunks as ec \
                        WHERE ec.linked_chunk_id = ? \
                        GROUP BY ec.chunk_id",
                    )?
                    .query_map((&hashed_linked_chunk_id,), |row| {
                        Ok((row.get::<_, u64>(0)?, row.get::<_, usize>(1)?))
                    })?
                    .collect::<Result<HashMap<_, _>, _>>()?;

                txn.prepare(
                    "SELECT \
                        lc.id, \
                        lc.previous, \
                        lc.next, \
                        lc.type \
                    FROM linked_chunks as lc \
                    WHERE lc.linked_chunk_id = ? \
                    ORDER BY lc.id",
                )?
                .query_map((&hashed_linked_chunk_id,), |row| {
                    Ok((
                        row.get::<_, u64>(0)?,
                        row.get::<_, Option<u64>>(1)?,
                        row.get::<_, Option<u64>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })?
                .map(|data| -> Result<_> {
                    let (id, previous, next, chunk_type) = data?;

                    // Note: since a gap has 0 events, an alternative could be to *not* retrieve
                    // the chunk type, and just let the hashmap lookup fail for gaps. However,
                    // benchmarking shows that this is slightly slower than matching the chunk
                    // type (around 1%, so in the realm of noise), so we keep the explicit
                    // check instead.
                    let num_items = if chunk_type == CHUNK_TYPE_GAP_TYPE_STRING {
                        0
                    } else {
                        num_events_by_chunk_ids.get(&id).copied().unwrap_or(0)
                    };

                    Ok(ChunkMetadata {
                        identifier: ChunkIdentifier::new(id),
                        previous: previous.map(ChunkIdentifier::new),
                        next: next.map(ChunkIdentifier::new),
                        num_items,
                    })
                })
                .collect::<Result<Vec<_>, _>>()
            })
            .await
    }

    #[instrument(skip(self))]
    async fn load_last_chunk(
        &self,
        linked_chunk_id: LinkedChunkId<'_>,
    ) -> Result<(Option<RawChunk<Event, Gap>>, ChunkIdentifierGenerator), Self::Error> {
        let _timer = timer!("method");

        let hashed_linked_chunk_id =
            self.encryption.encode_linked_chunk(keys::LINKED_CHUNKS, &linked_chunk_id);
        let encryption = self.encryption.clone();

        self
            .read()
            .await?
            .with_transaction(move |txn| -> Result<_> {
                // Find the latest chunk identifier to generate a `ChunkIdentifierGenerator`, and count the number of chunks.
                let (observed_max_identifier, number_of_chunks) = txn
                    .prepare(
                        "SELECT MAX(id), COUNT(*) FROM linked_chunks WHERE linked_chunk_id = ?"
                    )?
                    .query_one(
                        (&hashed_linked_chunk_id,),
                        |row| {
                            Ok((
                                // Read the `MAX(id)` as an `Option<u64>` instead
                                // of `u64` in case the `SELECT` returns nothing.
                                // Indeed, if it returns no line, the `MAX(id)` is
                                // set to `Null`.
                                row.get::<_, Option<u64>>(0)?,
                                row.get::<_, u64>(1)?,
                            ))
                        }
                    )?;

                let chunk_identifier_generator = match observed_max_identifier {
                    Some(max_observed_identifier) => {
                        ChunkIdentifierGenerator::new_from_previous_chunk_identifier(
                            ChunkIdentifier::new(max_observed_identifier)
                        )
                    },
                    None => ChunkIdentifierGenerator::new_from_scratch(),
                };

                // Find the last chunk.
                let Some((chunk_identifier, previous_chunk, chunk_type)) = txn
                    .prepare(
                        "SELECT id, previous, type FROM linked_chunks WHERE linked_chunk_id = ? AND next IS NULL"
                    )?
                    .query_one(
                        (&hashed_linked_chunk_id,),
                        |row| {
                            Ok((
                                row.get::<_, u64>(0)?,
                                row.get::<_, Option<u64>>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        }
                    )
                    .optional()?
                else {
                    // Chunk is not found and there are zero chunks for this room, this is consistent, all
                    // good.
                    if number_of_chunks == 0 {
                        return Ok((None, chunk_identifier_generator));
                    }
                    // Chunk is not found **but** there are chunks for this room, this is inconsistent. The
                    // linked chunk is malformed.
                    //
                    // Returning `Ok((None, _))` would be invalid here: we must return an error.
                    else {
                        return Err(Error::InvalidData {
                            details:
                                "last chunk is not found but chunks exist: the linked chunk contains a cycle"
                                    .to_owned()
                            }
                        )
                    }
                };

                // Build the chunk.
                let last_chunk = txn.rebuild_chunk(
                    &encryption,
                    &hashed_linked_chunk_id,
                    previous_chunk,
                    chunk_identifier,
                    None,
                    &chunk_type
                )?;

                Ok((Some(last_chunk), chunk_identifier_generator))
            })
            .await
    }

    #[instrument(skip(self))]
    async fn load_previous_chunk(
        &self,
        linked_chunk_id: LinkedChunkId<'_>,
        before_chunk_identifier: ChunkIdentifier,
    ) -> Result<Option<RawChunk<Event, Gap>>, Self::Error> {
        let _timer = timer!("method");

        let hashed_linked_chunk_id =
            self.encryption.encode_linked_chunk(keys::LINKED_CHUNKS, &linked_chunk_id);
        let encryption = self.encryption.clone();

        self
            .read()
            .await?
            .with_transaction(move |txn| -> Result<_> {
                // Find the chunk before the chunk identified by `before_chunk_identifier`.
                let Some((chunk_identifier, previous_chunk, next_chunk, chunk_type)) = txn
                    .prepare(
                        "SELECT id, previous, next, type FROM linked_chunks WHERE linked_chunk_id = ? AND next = ?"
                    )?
                    .query_one(
                        (&hashed_linked_chunk_id, before_chunk_identifier.index()),
                        |row| {
                            Ok((
                                row.get::<_, u64>(0)?,
                                row.get::<_, Option<u64>>(1)?,
                                row.get::<_, Option<u64>>(2)?,
                                row.get::<_, String>(3)?,
                            ))
                        }
                    )
                    .optional()?
                else {
                    // Chunk is not found.
                    return Ok(None);
                };

                // Build the chunk.
                let last_chunk = txn.rebuild_chunk(
                    &encryption,
                    &hashed_linked_chunk_id,
                    previous_chunk,
                    chunk_identifier,
                    next_chunk,
                    &chunk_type
                )?;

                Ok(Some(last_chunk))
            })
            .await
    }

    async fn load_thread_info(
        &self,
        room_id: &RoomId,
        thread_id: &EventId,
    ) -> Result<ThreadInfo, Self::Error> {
        let linked_chunk_id = LinkedChunkId::Thread(room_id, thread_id);
        let hashed_linked_chunk_id =
            self.encryption.encode_linked_chunk(keys::LINKED_CHUNKS, &linked_chunk_id);
        let encryption = self.encryption.clone();

        // First off, try by selecting the thread info. It's the most common case. If it
        // doesn't exist, create an empty one.
        //
        // We do that with 2 transactions.
        let maybe_thread_info = self
            .read()
            .await?
            .with_transaction(move |txn| {
                let maybe_encoded_thread_info = txn
                    .query_one(
                        "SELECT info FROM threads WHERE linked_chunk_id = ?",
                        (hashed_linked_chunk_id,),
                        |row| row.get::<_, Vec<u8>>(0),
                    )
                    .optional()?;

                maybe_encoded_thread_info
                    .map(|encoded_thread_info| {
                        encryption.decode_thread_info(encoded_thread_info.as_slice())
                    })
                    .transpose()
            })
            .await?;

        if let Some(thread_info) = maybe_thread_info {
            return Ok(thread_info);
        }

        // The thread doesn't exist, let's create it.

        let thread_info = ThreadInfo::new();
        let hashed_linked_chunk_id =
            self.encryption.encode_linked_chunk(keys::LINKED_CHUNKS, &linked_chunk_id);
        let hashed_room_id = self.encryption.encode_room_id(keys::EVENTS, room_id);
        let hashed_thread_id = self.encryption.encode_thread_id(thread_id)?;
        let encoded_thread_id = self.encryption.encode_thread_info(&thread_info)?;

        self.write()
            .await?
            .with_transaction(move |txn| {
                txn.execute(
                    "INSERT INTO threads VALUES (?, ?, ?, ?)",
                    (hashed_linked_chunk_id, hashed_room_id, hashed_thread_id, encoded_thread_id),
                )?;

                Ok::<(), Self::Error>(())
            })
            .await?;

        Ok(thread_info)
    }

    async fn update_thread_info(
        &self,
        room_id: &RoomId,
        thread_id: &EventId,
        thread_info: &ThreadInfo,
    ) -> Result<(), Self::Error> {
        let hashed_linked_chunk_id = self
            .encryption
            .encode_linked_chunk(keys::LINKED_CHUNKS, &LinkedChunkId::Thread(room_id, thread_id));
        let encoded_thread_info = self.encryption.encode_thread_info(thread_info)?;

        self.write()
            .await?
            .with_transaction(move |txn| {
                txn.execute(
                    "UPDATE threads SET info = ? WHERE linked_chunk_id = ?",
                    (encoded_thread_info, hashed_linked_chunk_id),
                )?;

                Ok(())
            })
            .await
    }

    #[instrument(skip(self))]
    async fn clear_all_events(&self, room_id: Option<&RoomId>) -> Result<(), Self::Error> {
        let _timer = timer!("method");

        match room_id {
            // Clear all events.
            None => {
                self.write()
                    .await?
                    .with_transaction(move |txn| {
                        // Remove all the chunks, and let cascading do its job.
                        txn.execute("DELETE FROM linked_chunks", ())?;

                        // Also clear all the events' contents, and let cascading do its job.
                        txn.execute("DELETE FROM events", ())?;

                        Ok(())
                    })
                    .await
            }

            // Clear events for specific room.
            Some(room_id) => {
                let encryption = self.encryption.clone();
                let room_id = room_id.to_owned();

                self.write()
                    .await?
                    .with_transaction(move |txn| {
                        // Delete linked chunks for the room and pinned-events caches.
                        {
                            let mut delete =
                                txn.prepare("DELETE FROM linked_chunks WHERE linked_chunk_id = ?")?;

                            for linked_chunk_id in
                                [LinkedChunkId::Room(&room_id), LinkedChunkId::PinnedEvents(&room_id)]
                            {
                                let linked_chunk_id = encryption.encode_linked_chunk(keys::LINKED_CHUNKS, &linked_chunk_id);

                                // Remove all the chunks about the current `LinkedChunkId`, and let cascading
                                // do its job.
                                delete.execute((&linked_chunk_id,))?;
                            }
                        }

                        let encoded_room_id = encryption.encode_room_id(keys::EVENTS, &room_id);

                        // Delete linked chunks for the thread caches.
                        {
                            txn.execute(
                                "DELETE FROM linked_chunks WHERE linked_chunk_id IN (SELECT linked_chunk_id FROM threads WHERE room_id = ?)",
                                (&encoded_room_id,),
                            )?;
                        }

                        // Also clear all the events' contents.
                        txn.execute(
                            "DELETE FROM events WHERE room_id = ?",
                            (encoded_room_id,),
                        )?;

                        Ok(())
                    })
                    .await
            }
        }
    }

    #[instrument(skip(self, event_ids))]
    async fn filter_duplicated_events(
        &self,
        linked_chunk_id: LinkedChunkId<'_>,
        event_ids: Vec<OwnedEventId>,
    ) -> Result<Vec<(OwnedEventId, Position)>, Self::Error> {
        let _timer = timer!("method");

        // If there's no events for which we want to check duplicates, we can return
        // early. It's not only an optimization to do so: it's required, otherwise the
        // `host_parameters` call below will panic.
        if event_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Select all events that exist in the store, i.e. the duplicates.
        let hashed_linked_chunk_id =
            self.encryption.encode_linked_chunk(keys::LINKED_CHUNKS, &linked_chunk_id);
        let event_ids_and_hashed_event_ids = event_ids
            .into_iter()
            .map(|event_id| {
                let hashed_event_id = self.encryption.encode_event_id(
                    // For the event ID, we need a stable hash between the
                    // `events` and `event_chunks` tables. That's why we use
                    // `keys::EVENTS` even if `hashed_event_id` is only used in
                    // `events_chunks`.
                    keys::EVENTS,
                    &event_id,
                );

                (event_id, hashed_event_id)
            })
            .collect::<Vec<_>>();

        self.read()
            .await?
            .with_transaction(move |txn| -> Result<_> {
                txn.chunk_large_query_over(
                    event_ids_and_hashed_event_ids,
                    None,
                    move |txn, event_ids_and_hashed_event_ids| {
                        let query = format!(
                            "SELECT event_id, chunk_id, position \
                            FROM event_chunks \
                            WHERE linked_chunk_id = ? AND event_id IN ({}) \
                            ORDER BY chunk_id ASC, position ASC",
                            event_ids_and_hashed_event_ids.host_parameters(),
                        );

                        let parameters = params_from_iter(
                            // parameter for `linked_chunk_id = ?`
                            once(
                                hashed_linked_chunk_id
                                    .to_sql()
                                    // SAFETY: it cannot fail since `Key::to_sql` never fails
                                    .unwrap(),
                            )
                            // parameters for `event_id IN (…)`
                            .chain(
                                event_ids_and_hashed_event_ids.iter().map(
                                    |(_event_id, hashed_event_id)| {
                                        hashed_event_id
                                            .to_sql()
                                            // SAFETY: it cannot fail since `Vec::<u8>::to_sql`
                                            // never fails
                                            .unwrap()
                                    },
                                ),
                            ),
                        );

                        let mut duplicated_events = Vec::new();

                        for duplicated_event in
                            txn.prepare(&query)?.query_map(parameters, |row| {
                                Ok((
                                    row.get::<_, Vec<u8>>(0)?,
                                    row.get::<_, u64>(1)?,
                                    row.get::<_, usize>(2)?,
                                ))
                            })?
                        {
                            let (duplicated_hashed_event_id, chunk_identifier, index) =
                                duplicated_event?;

                            // The event ID is encoded in the database. We can't decode it. However,
                            // we can find the original event ID with the `event_ids` parameter of
                            // this method by comparing the encoded event ID!
                            let Some(duplicated_event_id) = event_ids_and_hashed_event_ids
                                .iter()
                                .find_map(|(event_id, hashed_event_id)| {
                                    (hashed_event_id.deref() == duplicated_hashed_event_id)
                                        .then_some(event_id.clone())
                                })
                            else {
                                error!(
                                    "Unreachable: found a duplicated event that was not requested"
                                );
                                continue;
                            };

                            duplicated_events.push((
                                duplicated_event_id,
                                Position::new(ChunkIdentifier::new(chunk_identifier), index),
                            ));
                        }

                        Ok(duplicated_events)
                    },
                )
            })
            .await
    }

    #[instrument(skip(self, event_id))]
    async fn find_event(
        &self,
        room_id: &RoomId,
        event_id: &EventId,
    ) -> Result<Option<Event>, Self::Error> {
        let _timer = timer!("method");

        let encryption = self.encryption.clone();

        let hashed_room_id = self.encryption.encode_room_id(keys::EVENTS, room_id);
        let hashed_event_id = self.encryption.encode_event_id(keys::EVENTS, event_id);

        self.read()
            .await?
            .with_transaction(move |txn| -> Result<_> {
                let Some(event) = txn
                    .prepare("SELECT content FROM events WHERE event_id = ? AND room_id = ?")?
                    .query_one((hashed_event_id, hashed_room_id), |row| row.get::<_, Vec<u8>>(0))
                    .optional()?
                else {
                    // Event is not found.
                    return Ok(None);
                };

                Ok(Some(encryption.decode_event(&event)?))
            })
            .await
    }

    #[instrument(skip(self, event_id, filters))]
    async fn find_event_relations(
        &self,
        room_id: &RoomId,
        event_id: &EventId,
        filters: Option<&[RelationType]>,
    ) -> Result<Vec<(Event, Option<Position>)>, Self::Error> {
        let _timer = timer!("method");

        let hashed_room_id = self.encryption.encode_room_id(keys::EVENTS, room_id);
        let hashed_linked_chunk_id =
            self.encryption.encode_linked_chunk(keys::LINKED_CHUNKS, &LinkedChunkId::Room(room_id));
        let hashed_event_id = self.encryption.encode_event_id(keys::EVENTS, event_id);

        let filters = filters.map(ToOwned::to_owned);
        let encryption = self.encryption.clone();

        self.read()
            .await?
            .with_transaction(move |txn| -> Result<_> {
                find_event_relations_transaction(
                    &encryption,
                    hashed_room_id,
                    hashed_linked_chunk_id,
                    hashed_event_id,
                    filters,
                    txn,
                )
            })
            .await
    }

    #[instrument(skip(self))]
    async fn get_room_events(
        &self,
        room_id: &RoomId,
        event_type: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<Vec<Event>, Self::Error> {
        let _timer = timer!("method");

        let encryption = self.encryption.clone();

        let hashed_room_id = self.encryption.encode_room_id(keys::EVENTS, room_id);
        let hashed_event_type = event_type.map(|e| self.encryption.encode_key(keys::EVENTS, e));
        let hashed_session_id = session_id.map(|s| self.encryption.encode_key(keys::EVENTS, s));

        self.read()
            .await?
            .with_transaction(move |txn| -> Result<_> {
                // I'm not sure why clippy claims that the clones aren't required. The compiler
                // tells us that the lifetimes aren't long enough if we remove them. Doesn't matter
                // much so let's silence things.
                #[allow(clippy::redundant_clone)]
                let (query, keys) = match (hashed_event_type, hashed_session_id) {
                    (None, None) => {
                        ("SELECT content FROM events WHERE room_id = ?", params![hashed_room_id])
                    }
                    (None, Some(session_id)) => (
                        "SELECT content FROM events WHERE room_id = ?1 AND session_id = ?2",
                        params![hashed_room_id, session_id.to_owned()],
                    ),
                    (Some(event_type), None) => (
                        "SELECT content FROM events WHERE room_id = ? AND event_type = ?",
                        params![hashed_room_id, event_type.to_owned()]
                    ),
                    (Some(event_type), Some(session_id)) => (
                        "SELECT content FROM events WHERE room_id = ?1 AND event_type = ?2 AND session_id = ?3",
                        params![hashed_room_id, event_type.to_owned(), session_id.to_owned()],
                    ),
                };

                let mut statement = txn.prepare(query)?;

                statement
                    .query_map(keys, |row| row.get::<_, Vec<u8>>(0))?
                    .map(|maybe_encoded_event| {
                        encryption.decode_event(&maybe_encoded_event?)
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .await
    }

    #[instrument(skip(self, event))]
    async fn save_event(&self, room_id: &RoomId, event: Event) -> Result<(), Self::Error> {
        let _timer = timer!("method");

        let Some(event_id) = event.event_id() else {
            error!("Trying to save an event with no ID");
            return Ok(());
        };

        let Some(event_type) = event.kind.event_type() else {
            error!(%event_id, "Trying to save an event with no event type");
            return Ok(());
        };

        let hashed_event_type = self.encryption.encode_key(keys::EVENTS, event_type);
        let hashed_session_id =
            event.kind.session_id().map(|s| self.encryption.encode_key(keys::EVENTS, s));

        let hashed_room_id = self.encryption.encode_room_id(keys::EVENTS, room_id);
        let hashed_event_id = self.encryption.encode_event_id(keys::EVENTS, event_id);
        let encoded_event = self.encryption.encode_event(&event)?;

        self.write()
            .await?
            .with_transaction(move |txn| -> Result<_> {
                txn.execute(
                    "INSERT OR REPLACE INTO events(room_id, event_id, event_type, session_id, content, relates_to, rel_type) VALUES (?, ?, ?, ?, ?, ?, ?)",
                    (
                        &hashed_room_id,
                        hashed_event_id,
                        hashed_event_type,
                        hashed_session_id,
                        encoded_event.content,
                        encoded_event.relates_to,
                        encoded_event.rel_type
                    )
                )?;

                Ok(())
            })
            .await
    }

    async fn close(&self) -> Result<(), Self::Error> {
        SqliteEventCacheStore::close(self).await
    }

    async fn reopen(&self) -> Result<(), Self::Error> {
        SqliteEventCacheStore::reopen(self).await
    }

    async fn optimize(&self) -> Result<(), Self::Error> {
        Ok(self.vacuum().await?)
    }

    async fn get_size(&self) -> Result<Option<usize>, Self::Error> {
        self.get_db_size().await
    }
}

fn find_event_relations_transaction(
    encryption: &Encryption,
    hashed_room_id: Key,
    hashed_linked_chunk_id: Key,
    hashed_event_id: Key,
    filters: Option<Vec<RelationType>>,
    txn: &Transaction<'_>,
) -> Result<Vec<(Event, Option<Position>)>> {
    let get_rows = |row: &rusqlite::Row<'_>| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, Option<u64>>(1)?,
            row.get::<_, Option<usize>>(2)?,
        ))
    };

    // Collect related events.
    let collect_results = |transaction| {
        let mut related = Vec::new();

        for result in transaction {
            let (event, chunk_id, index): (Vec<u8>, Option<u64>, _) = result?;
            let event = encryption.decode_event(&event)?;

            // Only build the position if both the chunk_id and position were present; in
            // theory, they should either be present at the same time, or not at all.
            let pos = chunk_id
                .zip(index)
                .map(|(chunk_id, index)| Position::new(ChunkIdentifier::new(chunk_id), index));

            related.push((event, pos));
        }

        Ok(related)
    };

    if let Some(filters) = filters
        && filters.is_empty().not()
    {
        let query = format!(
            "SELECT events.content, event_chunks.chunk_id, event_chunks.position \
            FROM events \
            LEFT JOIN event_chunks ON events.event_id = event_chunks.event_id AND event_chunks.linked_chunk_id = ? \
            WHERE events.relates_to = ? AND events.room_id = ? AND events.rel_type IN ({})",
            host_parameters(filters.len())
        );

        // First the filters need to be stringified; because `.to_sql()` will borrow
        // from them, they also need to be stringified onto the stack, so as to
        // get a stable address (to avoid returning a temporary reference in the
        // map closure below).
        let filter_strings: Vec<_> = filters.iter().map(|f| f.to_string()).collect();
        let filters_params: Vec<_> = filter_strings
            .iter()
            .map(|f| f.to_sql().expect("converting a string to SQL should work"))
            .collect();

        let parameters = params_from_iter(
            [
                hashed_linked_chunk_id.to_sql().expect(
                    "We should be able to convert a hashed linked chunk ID to a SQLite value",
                ),
                hashed_event_id
                    .to_sql()
                    .expect("We should be able to convert an event ID to a SQLite value"),
                hashed_room_id
                    .to_sql()
                    .expect("We should be able to convert a room ID to a SQLite value"),
            ]
            .into_iter()
            .chain(filters_params),
        );

        let mut transaction = txn.prepare(&query)?;
        let transaction = transaction.query_map(parameters, get_rows)?;

        collect_results(transaction)
    } else {
        let query = "SELECT events.content, event_chunks.chunk_id, event_chunks.position \
            FROM events \
            LEFT JOIN event_chunks ON events.event_id = event_chunks.event_id AND event_chunks.linked_chunk_id = ? \
            WHERE events.relates_to = ? AND events.room_id = ?";
        let parameters = (hashed_linked_chunk_id, hashed_event_id, hashed_room_id);

        let mut transaction = txn.prepare(query)?;
        let transaction = transaction.query_map(parameters, get_rows)?;

        collect_results(transaction)
    }
}

/// Like `deadpool::managed::Object::with_transaction`, but starts the
/// transaction in immediate (write) mode from the beginning, precluding errors
/// of the kind SQLITE_BUSY from happening, for transactions that may involve
/// both reads and writes, and start with a write.
async fn with_immediate_transaction<
    T: Send + 'static,
    F: FnOnce(&Transaction<'_>) -> Result<T, Error> + Send + 'static,
>(
    this: &SqliteEventCacheStore,
    f: F,
) -> Result<T, Error> {
    this.write()
        .await?
        .interact(move |conn| -> Result<T, Error> {
            // Start the transaction in IMMEDIATE mode since all updates may cause writes,
            // to avoid read transactions upgrading to write mode and causing
            // SQLITE_BUSY errors. See also: https://www.sqlite.org/lang_transaction.html#deferred_immediate_and_exclusive_transactions
            conn.set_transaction_behavior(TransactionBehavior::Immediate);

            let code = || -> Result<T, Error> {
                let txn = conn.transaction()?;
                let res = f(&txn)?;
                txn.commit()?;
                Ok(res)
            };

            let res = code();

            // Reset the transaction behavior to use Deferred, after this transaction has
            // been run, whether it was successful or not.
            conn.set_transaction_behavior(TransactionBehavior::Deferred);

            res
        })
        .await
        // SAFETY: same logic as in [`deadpool::managed::Object::with_transaction`].`
        .unwrap()
}

fn insert_chunk(
    txn: &Transaction<'_>,
    linked_chunk_id: &Key,
    previous: Option<u64>,
    new: u64,
    next: Option<u64>,
    type_str: &str,
) -> rusqlite::Result<()> {
    // First, insert the new chunk.
    txn.execute(
        r#"
            INSERT INTO linked_chunks(id, linked_chunk_id, previous, next, type)
            VALUES (?, ?, ?, ?, ?)
        "#,
        (new, linked_chunk_id, previous, next, type_str),
    )?;

    // If this chunk has a previous one, update its `next` field.
    if let Some(previous) = previous {
        let updated = txn.execute(
            r#"
                UPDATE linked_chunks
                SET next = ?
                WHERE id = ? AND linked_chunk_id = ?
            "#,
            (new, previous, linked_chunk_id),
        )?;
        if updated < 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        if updated > 1 {
            return Err(rusqlite::Error::QueryReturnedMoreThanOneRow);
        }
    }

    // If this chunk has a next one, update its `previous` field.
    if let Some(next) = next {
        let updated = txn.execute(
            r#"
                UPDATE linked_chunks
                SET previous = ?
                WHERE id = ? AND linked_chunk_id = ?
            "#,
            (new, next, linked_chunk_id),
        )?;
        if updated < 1 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        if updated > 1 {
            return Err(rusqlite::Error::QueryReturnedMoreThanOneRow);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{
            LazyLock,
            atomic::{AtomicU32, Ordering::SeqCst},
        },
    };

    use assert_matches::assert_matches;
    use matrix_sdk_base::{
        event_cache::store::{
            EventCacheStore, EventCacheStoreError, IntoEventCacheStore,
            integration_tests::EventCacheStoreIntegrationTests,
        },
        event_cache_store_integration_tests, event_cache_store_integration_tests_time,
        linked_chunk::{ChunkIdentifier, LinkedChunkId, Update},
    };
    use matrix_sdk_test::{DEFAULT_TEST_ROOM_ID, async_test};
    use ruma::{OwnedEventId, event_id};
    use tempfile::{TempDir, tempdir};

    use super::{SqliteEventCacheStore, keys};
    use crate::{SqliteStoreConfig, utils::SqliteAsyncConnExt};

    static TMP_DIR: LazyLock<TempDir> = LazyLock::new(|| tempdir().unwrap());
    static NUM: AtomicU32 = AtomicU32::new(0);

    fn new_event_cache_store_workspace() -> PathBuf {
        let name = NUM.fetch_add(1, SeqCst).to_string();
        TMP_DIR.path().join(name)
    }

    async fn get_event_cache_store() -> Result<SqliteEventCacheStore, EventCacheStoreError> {
        let tmpdir_path = new_event_cache_store_workspace();

        tracing::info!("using event cache store @ {}", tmpdir_path.to_str().unwrap());

        Ok(SqliteEventCacheStore::open(tmpdir_path.to_str().unwrap(), None).await.unwrap())
    }

    event_cache_store_integration_tests!();
    event_cache_store_integration_tests_time!();

    #[async_test]
    async fn test_encryption_encode_decode_thread_id_roundtrip() {
        let store = get_event_cache_store().await.expect("creating cache store failed");
        let thread_id: OwnedEventId = event_id!("$event").to_owned();

        let encoded_thread_id = store.encryption.encode_thread_id(&thread_id).unwrap();
        let decoded_thread_id: OwnedEventId =
            store.encryption.decode_thread_id(&encoded_thread_id).unwrap();

        assert_eq!(thread_id, decoded_thread_id);
    }

    #[async_test]
    async fn test_pool_size() {
        let tmpdir_path = new_event_cache_store_workspace();
        let store_open_config = SqliteStoreConfig::new(tmpdir_path).pool_max_size(42);

        let store = SqliteEventCacheStore::open_with_config(&store_open_config).await.unwrap();

        assert_eq!(store.pool_max_size().await, Some(42));
    }

    #[async_test]
    async fn test_linked_chunk_remove_chunk() {
        let store = get_event_cache_store().await.expect("creating cache store failed");

        // Run corresponding integration test
        store.clone().into_event_cache_store().test_linked_chunk_remove_chunk().await;

        // Check that cascading worked. Yes, SQLite, I doubt you.
        let gaps = store
            .read()
            .await
            .unwrap()
            .with_transaction(|txn| -> rusqlite::Result<_> {
                let mut gaps = Vec::new();
                for data in txn
                    .prepare("SELECT chunk_id FROM gap_chunks ORDER BY chunk_id")?
                    .query_map((), |row| row.get::<_, u64>(0))?
                {
                    gaps.push(data?);
                }
                Ok(gaps)
            })
            .await
            .unwrap();

        // Check that the gaps match those set up in the corresponding integration test
        // above
        assert_eq!(gaps, vec![42, 44]);
    }

    #[async_test]
    async fn test_linked_chunk_remove_item() {
        let store = get_event_cache_store().await.expect("creating cache store failed");

        // Run corresponding integration test
        store.clone().into_event_cache_store().test_linked_chunk_remove_item().await;

        let room_id = *DEFAULT_TEST_ROOM_ID;
        let hashed_linked_chunk_id = store
            .encryption
            .encode_linked_chunk(keys::LINKED_CHUNKS, &LinkedChunkId::Room(room_id));

        // Make sure the position have been updated for the remaining events.
        let num_rows: u64 = store
            .read()
            .await
            .unwrap()
            .with_transaction(move |txn| {
                txn.query_one(
                    "SELECT COUNT(*) FROM event_chunks WHERE chunk_id = 42 AND linked_chunk_id = ? AND position IN (2, 3, 4)",
                    (hashed_linked_chunk_id,),
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();
        assert_eq!(num_rows, 3);
    }

    #[async_test]
    async fn test_linked_chunk_clear() {
        let store = get_event_cache_store().await.expect("creating cache store failed");

        // Run corresponding integration test
        store.clone().into_event_cache_store().test_linked_chunk_clear().await;

        // Check that cascading worked. Yes, SQLite, I doubt you.
        store
            .read()
            .await
            .unwrap()
            .with_transaction(|txn| -> rusqlite::Result<_> {
                let num_gaps = txn
                    .prepare("SELECT COUNT(chunk_id) FROM gap_chunks ORDER BY chunk_id")?
                    .query_one((), |row| row.get::<_, u64>(0))?;
                assert_eq!(num_gaps, 0);

                let num_events = txn
                    .prepare("SELECT COUNT(event_id) FROM event_chunks ORDER BY chunk_id")?
                    .query_one((), |row| row.get::<_, u64>(0))?;
                assert_eq!(num_events, 0);

                Ok(())
            })
            .await
            .unwrap();
    }

    #[async_test]
    async fn test_linked_chunk_update_is_a_transaction() {
        let store = get_event_cache_store().await.expect("creating cache store failed");

        let room_id = *DEFAULT_TEST_ROOM_ID;
        let linked_chunk_id = LinkedChunkId::Room(room_id);

        // Trigger a violation of the unique constraint on the (room id, chunk id)
        // couple.
        let err = store
            .handle_linked_chunk_updates(
                linked_chunk_id,
                vec![
                    Update::NewItemsChunk {
                        previous: None,
                        new: ChunkIdentifier::new(42),
                        next: None,
                    },
                    Update::NewItemsChunk {
                        previous: None,
                        new: ChunkIdentifier::new(42),
                        next: None,
                    },
                ],
            )
            .await
            .unwrap_err();

        // The operation fails with a constraint violation error.
        assert_matches!(err, crate::error::Error::Sqlite(err) => {
            assert_matches!(err.sqlite_error_code(), Some(rusqlite::ErrorCode::ConstraintViolation));
        });

        // If the updates have been handled transactionally, then no new chunks should
        // have been added; failure of the second update leads to the first one being
        // rolled back.
        let chunks = store.load_all_chunks(linked_chunk_id).await.unwrap();
        assert!(chunks.is_empty());
    }
}

#[cfg(test)]
mod encrypted_tests {
    use std::sync::{
        LazyLock,
        atomic::{AtomicU32, Ordering::SeqCst},
    };

    use matrix_sdk_base::{
        event_cache::store::{EventCacheStore, EventCacheStoreError},
        event_cache_store_integration_tests, event_cache_store_integration_tests_time,
    };
    use matrix_sdk_test::{async_test, event_factory::EventFactory};
    use ruma::{
        event_id,
        events::{relation::RelationType, room::message::RoomMessageEventContentWithoutRelation},
        room_id, user_id,
    };
    use tempfile::{TempDir, tempdir};

    use super::SqliteEventCacheStore;

    static TMP_DIR: LazyLock<TempDir> = LazyLock::new(|| tempdir().unwrap());
    static NUM: AtomicU32 = AtomicU32::new(0);

    async fn get_event_cache_store() -> Result<SqliteEventCacheStore, EventCacheStoreError> {
        let name = NUM.fetch_add(1, SeqCst).to_string();
        let tmpdir_path = TMP_DIR.path().join(name);

        tracing::info!("using event cache store @ {}", tmpdir_path.to_str().unwrap());

        Ok(SqliteEventCacheStore::open(
            tmpdir_path.to_str().unwrap(),
            Some("default_test_password"),
        )
        .await
        .unwrap())
    }

    event_cache_store_integration_tests!();
    event_cache_store_integration_tests_time!();

    #[async_test]
    async fn test_no_sqlite_injection_in_find_event_relations() {
        let room_id = room_id!("!test:localhost");
        let another_room_id = room_id!("!r1:matrix.org");
        let sender = user_id!("@alice:localhost");

        let store = get_event_cache_store()
            .await
            .expect("We should be able to create a new, empty, event cache store");

        let f = EventFactory::new().room(room_id).sender(sender);

        // Create an event for the first room.
        let event_id = event_id!("$DO_NOT_FIND_ME:matrix.org");
        let event = f.text_msg("DO NOT FIND").event_id(event_id).into_event();

        // Create a related event.
        let edit_id = event_id!("$find_me:matrix.org");
        let edit = f
            .text_msg("Find me")
            .event_id(edit_id)
            .edit(event_id, RoomMessageEventContentWithoutRelation::text_plain("jebote"))
            .into_event();

        // Create an event for the second room.
        let f = f.room(another_room_id);

        let another_event_id = event_id!("$DO_NOT_FIND_ME_EITHER:matrix.org");
        let another_event =
            f.text_msg("DO NOT FIND ME EITHER").event_id(another_event_id).into_event();

        // Save the events in the DB.
        store.save_event(room_id, event).await.unwrap();
        store.save_event(room_id, edit).await.unwrap();
        store.save_event(another_room_id, another_event).await.unwrap();

        // Craft a `RelationType` that will inject some SQL to be executed. The
        // `OR 1=1` ensures that all the previous parameters, the room
        // ID and event ID are ignored.
        let filter = Some(vec![RelationType::Replacement, "x\") OR 1=1; --".into()]);

        // Attempt to find events in the first room.
        let results = store
            .find_event_relations(room_id, event_id, filter.as_deref())
            .await
            .expect("We should be able to attempt to find event relations");

        // Ensure that we only got the single related event the first room contains.
        similar_asserts::assert_eq!(
            results.len(),
            1,
            "We should only have loaded events for the first room {results:#?}"
        );

        // The event needs to be the edit event, otherwise something is wrong.
        let (found_event, _) = &results[0];
        assert_eq!(
            found_event.event_id(),
            Some(edit_id),
            "The single event we found should be the edit event"
        );
    }
}

#[cfg(test)]
mod close_reopen_tests {
    use std::sync::{
        LazyLock,
        atomic::{AtomicU32, Ordering::SeqCst},
    };

    use matrix_sdk_base::{event_cache::store::EventCacheStore, linked_chunk::LinkedChunkId};
    use matrix_sdk_test::{DEFAULT_TEST_ROOM_ID, async_test};
    use tempfile::{TempDir, tempdir};

    use super::SqliteEventCacheStore;

    static TMP_DIR: LazyLock<TempDir> = LazyLock::new(|| tempdir().unwrap());
    static NUM: AtomicU32 = AtomicU32::new(0);

    async fn new_store() -> SqliteEventCacheStore {
        let name = NUM.fetch_add(1, SeqCst).to_string();
        let tmpdir_path = TMP_DIR.path().join(name);
        SqliteEventCacheStore::open(tmpdir_path, None).await.unwrap()
    }

    #[async_test]
    async fn test_close_completes_without_timeout() {
        let store = new_store().await;

        // Close should complete quickly without hitting the 5s timeout.
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

        let err = store.load_all_chunks(LinkedChunkId::Room(*DEFAULT_TEST_ROOM_ID)).await;
        assert!(err.is_err(), "read should fail when closed");

        let err_msg = err.unwrap_err().to_string();
        assert!(err_msg.contains("closed"), "error should mention 'closed', got: {err_msg}");
    }

    #[async_test]
    async fn test_write_fails_when_closed() {
        let store = new_store().await;
        store.close().await.unwrap();

        let err = store.try_take_leased_lock(1000, "test_lock", "holder").await;
        assert!(err.is_err(), "write should fail when closed");

        let err_msg = err.unwrap_err().to_string();
        assert!(err_msg.contains("closed"), "error should mention 'closed', got: {err_msg}");
    }

    #[async_test]
    async fn test_data_persists_across_close_reopen() {
        let store = new_store().await;

        // Take a lease lock — this is persisted in the database.
        let result = store.try_take_leased_lock(60_000, "test_lock", "holder").await.unwrap();
        assert!(result.is_some(), "should have acquired the lock");

        // Close and reopen.
        store.close().await.unwrap();
        store.reopen().await.unwrap();

        // The lock should still be held by the original holder after reopen.
        let result = store.try_take_leased_lock(60_000, "test_lock", "other_holder").await.unwrap();
        assert!(result.is_none(), "lock should still be held by the original holder after reopen");
    }

    #[async_test]
    async fn test_multiple_close_reopen_cycles() {
        let store = new_store().await;

        for _ in 0..5 {
            store.close().await.unwrap();
            store.reopen().await.unwrap();

            // After each cycle, the store should be fully operational.
            let result = store.load_all_chunks(LinkedChunkId::Room(*DEFAULT_TEST_ROOM_ID)).await;
            assert!(result.is_ok(), "store should work after close/reopen cycle");
        }
    }

    #[async_test]
    async fn test_pool_is_fully_drained_after_close() {
        let store = new_store().await;

        // Do a few reads to exercise the pool.
        let _ = store.load_all_chunks(LinkedChunkId::Room(*DEFAULT_TEST_ROOM_ID)).await;
        let _ = store.load_all_chunks(LinkedChunkId::Room(*DEFAULT_TEST_ROOM_ID)).await;

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
        let result = store.load_all_chunks(LinkedChunkId::Room(*DEFAULT_TEST_ROOM_ID)).await;
        assert!(result.is_ok(), "read should succeed immediately after reopen");

        // Write should work immediately after reopen.
        let result = store.try_take_leased_lock(1000, "test_lock", "holder").await;
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
