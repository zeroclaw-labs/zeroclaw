use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt, iter,
    path::{Path, PathBuf},
    str::FromStr as _,
    sync::Arc,
};

use async_trait::async_trait;
use deadpool::managed::PoolConfig;
use matrix_sdk_base::{
    MinimalRoomMemberEvent, ROOM_VERSION_FALLBACK, ROOM_VERSION_RULES_FALLBACK, RoomInfo,
    RoomMemberships, RoomState, StateChanges, StateStore, StateStoreDataKey, StateStoreDataValue,
    deserialized_responses::{DisplayName, RawAnySyncOrStrippedState, SyncOrStrippedState},
    store::{
        ChildTransactionId, DependentQueuedRequest, DependentQueuedRequestKind, QueueWedgeError,
        QueuedRequest, QueuedRequestKind, RoomLoadSettings, SentRequestKey,
        StoredThreadSubscription, ThreadSubscriptionStatus, migration_helpers::RoomInfoV1,
    },
};
use matrix_sdk_store_encryption::StoreCipher;
use ruma::{
    CanonicalJsonObject, EventId, MilliSecondsSinceUnixEpoch, OwnedEventId, OwnedRoomId,
    OwnedTransactionId, OwnedUserId, RoomId, TransactionId, UInt, UserId,
    canonical_json::{RedactedBecause, redact},
    events::{
        AnyGlobalAccountDataEvent, AnyRoomAccountDataEvent, AnySyncStateEvent,
        GlobalAccountDataEventType, RoomAccountDataEventType, StateEventType,
        presence::PresenceEvent,
        receipt::{Receipt, ReceiptThread, ReceiptType},
        room::{
            create::RoomCreateEventContent,
            member::{StrippedRoomMemberEvent, SyncRoomMemberEvent},
        },
    },
    profile::{UserProfile, UserProfileUpdate},
    serde::Raw,
};
use rusqlite::{OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    sync::{Mutex, OwnedMutexGuard},
};
use tracing::{debug, instrument, warn};

use crate::{
    OpenStoreError, RuntimeConfig, Secret, SqliteStoreConfig,
    connection::{self, Connection as SqliteAsyncConn, Pool as SqlitePool, SqliteConnections},
    error::{Error, Result},
    utils::{
        EncryptableStore, Key, SqliteAsyncConnExt, SqliteKeyValueStoreAsyncConnExt,
        SqliteKeyValueStoreConnExt,
    },
};

mod keys {
    // Tables
    pub const KV_BLOB: &str = "kv_blob";
    pub const ROOM_INFO: &str = "room_info";
    pub const STATE_EVENT: &str = "state_event";
    pub const GLOBAL_ACCOUNT_DATA: &str = "global_account_data";
    pub const ROOM_ACCOUNT_DATA: &str = "room_account_data";
    pub const MEMBER: &str = "member";
    pub const PROFILE: &str = "profile";
    pub const RECEIPT: &str = "receipt";
    pub const DISPLAY_NAME: &str = "display_name";
    pub const SEND_QUEUE: &str = "send_queue_events";
    pub const DEPENDENTS_SEND_QUEUE: &str = "dependent_send_queue_events";
    pub const THREAD_SUBSCRIPTIONS: &str = "thread_subscriptions";
    pub const GLOBAL_PROFILES: &str = "global_profiles";
}

/// The filename used for the SQLITE database file used by the state store.
pub const DATABASE_NAME: &str = "matrix-sdk-state.sqlite3";

/// An SQLite-based state store.
#[derive(Clone)]
pub struct SqliteStateStore {
    store_cipher: Option<Arc<StoreCipher>>,

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
impl fmt::Debug for SqliteStateStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqliteStateStore").finish_non_exhaustive()
    }
}

impl SqliteStateStore {
    /// Open the SQLite-based state store at the given path using the given
    /// given passphrase to encrypt private data.
    pub async fn open(
        path: impl AsRef<Path>,
        passphrase: Option<&str>,
    ) -> Result<Self, OpenStoreError> {
        Self::open_with_config(&SqliteStoreConfig::new(path).passphrase(passphrase)).await
    }

    /// Open the SQLite-based state store at the given path using the given
    /// key to encrypt private data.
    pub async fn open_with_key(
        path: impl AsRef<Path>,
        key: Option<&[u8]>,
    ) -> Result<Self, OpenStoreError> {
        Self::open_with_config(&SqliteStoreConfig::new(path).key(key)).await
    }

    /// Open the SQLite-based state store with the config open config.
    pub async fn open_with_config(config: &SqliteStoreConfig) -> Result<Self, OpenStoreError> {
        fs::create_dir_all(&config.path).await.map_err(OpenStoreError::CreateDir)?;

        let pool = config.build_pool_of_connections(DATABASE_NAME)?;
        let pool_config = config.pool_config;
        let runtime_config = config.runtime_config;

        let this =
            Self::open_with_pool(pool, config.secret.clone(), pool_config, runtime_config).await?;
        this.read().await?.apply_runtime_config(runtime_config).await?;

        Ok(this)
    }

    /// Create an SQLite-based state store using the given SQLite database pool.
    /// The given secret will be used to encrypt private data.
    pub(crate) async fn open_with_pool(
        pool: SqlitePool,
        secret: Option<Secret>,
        pool_config: PoolConfig,
        runtime_config: RuntimeConfig,
    ) -> Result<Self, OpenStoreError> {
        let db_path = pool.manager().database_path.clone();
        let conn = pool.get().await?;

        let mut version = conn.db_version().await?;

        if version == 0 {
            init(&conn).await?;
            version = 1;
        }

        let store_cipher = match secret {
            Some(s) => Some(Arc::new(conn.get_or_create_store_cipher(s).await?)),
            None => None,
        };
        let this = Self {
            store_cipher,
            connections: Arc::new(Mutex::new(Some(SqliteConnections {
                pool,
                write_connection: Arc::new(Mutex::new(conn)),
            }))),
            db_path,
            pool_config,
            runtime_config,
        };
        this.run_migrations(version, None).await?;

        this.read().await?.wal_checkpoint().await;

        Ok(this)
    }

    /// Run database migrations from the given `from` version to the given `to`
    /// version
    ///
    /// If `to` is `None`, the current database version will be used.
    async fn run_migrations(&self, from: u8, to: Option<u8>) -> Result<()> {
        if to == Some(1) {
            return Ok(());
        }

        let conn = self.write().await?;

        if from < 2 {
            debug!("Upgrading database to version 2");
            let this = self.clone();
            conn.with_transaction(move |txn| {
                // Create new table.
                txn.execute_batch(include_str!(
                    "../migrations/state_store/002_a_create_new_room_info.sql"
                ))?;

                // Migrate data to new table.
                for data in txn
                    .prepare("SELECT data FROM room_info")?
                    .query_map((), |row| row.get::<_, Vec<u8>>(0))?
                {
                    let data = data?;
                    let room_info: RoomInfoV1 = this.deserialize_json(&data)?;

                    let room_id = this.encode_key(keys::ROOM_INFO, room_info.room_id());
                    let state = this
                        .encode_key(keys::ROOM_INFO, serde_json::to_string(&room_info.state())?);
                    txn.prepare_cached(
                        "INSERT OR REPLACE INTO new_room_info (room_id, state, data)
                         VALUES (?, ?, ?)",
                    )?
                    .execute((room_id, state, data))?;
                }

                // Replace old table.
                txn.execute_batch(include_str!(
                    "../migrations/state_store/002_b_replace_room_info.sql"
                ))?;

                txn.set_db_version(2)?;
                Result::<_, Error>::Ok(())
            })
            .await?;
        }

        if to == Some(2) {
            return Ok(());
        }

        // Migration to v3: RoomInfo format has changed.
        if from < 3 {
            debug!("Upgrading database to version 3");
            let this = self.clone();
            conn.with_transaction(move |txn| {
                // Migrate data .
                for data in txn
                    .prepare("SELECT data FROM room_info")?
                    .query_map((), |row| row.get::<_, Vec<u8>>(0))?
                {
                    let data = data?;
                    let room_info_v1: RoomInfoV1 = this.deserialize_json(&data)?;

                    // Get the `m.room.create` event from the room state.
                    let room_id = this.encode_key(keys::STATE_EVENT, room_info_v1.room_id());
                    let event_type =
                        this.encode_key(keys::STATE_EVENT, StateEventType::RoomCreate.to_string());
                    let create_res = txn
                        .prepare(
                            "SELECT stripped, data FROM state_event
                             WHERE room_id = ? AND event_type = ?",
                        )?
                        .query_one([room_id, event_type], |row| {
                            Ok((row.get::<_, bool>(0)?, row.get::<_, Vec<u8>>(1)?))
                        })
                        .optional()?;

                    let create = create_res.and_then(|(stripped, data)| {
                        let create = if stripped {
                            SyncOrStrippedState::<RoomCreateEventContent>::Stripped(
                                this.deserialize_json(&data).ok()?,
                            )
                        } else {
                            SyncOrStrippedState::Sync(this.deserialize_json(&data).ok()?)
                        };
                        Some(create)
                    });

                    let migrated_room_info = room_info_v1.migrate(create.as_ref());

                    let data = this.serialize_json(&migrated_room_info)?;
                    let room_id = this.encode_key(keys::ROOM_INFO, migrated_room_info.room_id());
                    txn.prepare_cached("UPDATE room_info SET data = ? WHERE room_id = ?")?
                        .execute((data, room_id))?;
                }

                txn.set_db_version(3)?;
                Result::<_, Error>::Ok(())
            })
            .await?;
        }

        if to == Some(3) {
            return Ok(());
        }

        if from < 4 {
            debug!("Upgrading database to version 4");
            conn.with_transaction(move |txn| {
                // Create new table.
                txn.execute_batch(include_str!("../migrations/state_store/003_send_queue.sql"))?;
                txn.set_db_version(4)
            })
            .await?;
        }

        if to == Some(4) {
            return Ok(());
        }

        if from < 5 {
            debug!("Upgrading database to version 5");
            conn.with_transaction(move |txn| {
                // Create new table.
                txn.execute_batch(include_str!(
                    "../migrations/state_store/004_send_queue_with_roomid_value.sql"
                ))?;
                txn.set_db_version(4)
            })
            .await?;
        }

        if to == Some(5) {
            return Ok(());
        }

        if from < 6 {
            debug!("Upgrading database to version 6");
            conn.with_transaction(move |txn| {
                // Create new table.
                txn.execute_batch(include_str!(
                    "../migrations/state_store/005_send_queue_dependent_events.sql"
                ))?;
                txn.set_db_version(6)
            })
            .await?;
        }

        if to == Some(6) {
            return Ok(());
        }

        if from < 7 {
            debug!("Upgrading database to version 7");
            conn.with_transaction(move |txn| {
                // Drop media table.
                txn.execute_batch(include_str!("../migrations/state_store/006_drop_media.sql"))?;
                txn.set_db_version(7)
            })
            .await?;
        }

        if to == Some(7) {
            return Ok(());
        }

        if from < 8 {
            debug!("Upgrading database to version 8");
            // Replace all existing wedged events with a generic error.
            let error = QueueWedgeError::GenericApiError {
                msg: "local echo failed to send in a previous session".into(),
            };
            let default_err = self.serialize_value(&error)?;

            conn.with_transaction(move |txn| {
                // Update send queue table to persist the wedge reason if any.
                txn.execute_batch(include_str!("../migrations/state_store/007_a_send_queue_wedge_reason.sql"))?;

                // Migrate the data, add a generic error for currently wedged events

                for wedged_entries in txn
                    .prepare("SELECT room_id, transaction_id FROM send_queue_events WHERE wedged = 1")?
                    .query_map((), |row| {
                        Ok(
                            (row.get::<_, Vec<u8>>(0)?,row.get::<_, String>(1)?)
                        )
                    })? {

                    let (room_id, transaction_id) = wedged_entries?;

                    txn.prepare_cached("UPDATE send_queue_events SET wedge_reason = ? WHERE room_id = ? AND transaction_id = ?")?
                        .execute((default_err.clone(), room_id, transaction_id))?;
                }


                // Clean up the table now that data is migrated
                txn.execute_batch(include_str!("../migrations/state_store/007_b_send_queue_clean.sql"))?;

                txn.set_db_version(8)
            })
                .await?;
        }

        if to == Some(8) {
            return Ok(());
        }

        if from < 9 {
            debug!("Upgrading database to version 9");
            conn.with_transaction(move |txn| {
                // Run the migration.
                txn.execute_batch(include_str!("../migrations/state_store/008_send_queue.sql"))?;
                txn.set_db_version(9)
            })
            .await?;
        }

        if to == Some(9) {
            return Ok(());
        }

        if from < 10 {
            debug!("Upgrading database to version 10");
            conn.with_transaction(move |txn| {
                // Run the migration.
                txn.execute_batch(include_str!(
                    "../migrations/state_store/009_send_queue_priority.sql"
                ))?;
                txn.set_db_version(10)
            })
            .await?;
        }

        if to == Some(10) {
            return Ok(());
        }

        if from < 11 {
            debug!("Upgrading database to version 11");
            conn.with_transaction(move |txn| {
                // Run the migration.
                txn.execute_batch(include_str!(
                    "../migrations/state_store/010_send_queue_enqueue_time.sql"
                ))?;
                txn.set_db_version(11)
            })
            .await?;
        }

        if to == Some(11) {
            return Ok(());
        }

        if from < 12 {
            debug!("Upgrading database to version 12");
            // Defragment the DB and optimize its size on the filesystem.
            // This should have been run in the migration for version 7, to reduce the size
            // of the DB as we removed the media cache.
            conn.vacuum().await?;
            conn.set_kv("version", vec![12]).await?;
        }

        if to == Some(12) {
            return Ok(());
        }

        if from < 13 {
            debug!("Upgrading database to version 13");
            conn.with_transaction(move |txn| {
                // Run the migration.
                txn.execute_batch(include_str!(
                    "../migrations/state_store/011_thread_subscriptions.sql"
                ))?;
                txn.set_db_version(13)
            })
            .await?;
        }

        if to == Some(13) {
            return Ok(());
        }

        if from < 14 {
            debug!("Upgrading database to version 14");
            conn.with_transaction(move |txn| {
                // Run the migration.
                txn.execute_batch(include_str!(
                    "../migrations/state_store/012_thread_subscriptions_bumpstamp.sql"
                ))?;
                txn.set_db_version(14)
            })
            .await?;
        }

        if to == Some(14) {
            return Ok(());
        }

        if from < 15 {
            debug!("Upgrading database to version 15");
            conn.with_transaction(move |txn| {
                // Run the migration.
                txn.execute_batch(include_str!(
                    "../migrations/state_store/013_send_queue_new_parent_key_format.sql"
                ))?;
                txn.set_db_version(15)
            })
            .await?;
        }

        if to == Some(15) {
            return Ok(());
        }

        if from < 16 {
            debug!("Upgrading database to version 16");
            conn.with_transaction(move |txn| {
                // Run the migration.
                txn.execute_batch(include_str!(
                    "../migrations/state_store/014_global_profiles.sql"
                ))?;
                txn.set_db_version(16)
            })
            .await?;
        }

        if to == Some(16) {
            return Ok(());
        }

        Ok(())
    }

    fn encode_state_store_data_key(&self, key: StateStoreDataKey<'_>) -> Key {
        let key_s = match key {
            StateStoreDataKey::SyncToken => Cow::Borrowed(StateStoreDataKey::SYNC_TOKEN),
            StateStoreDataKey::SupportedVersions => {
                Cow::Borrowed(StateStoreDataKey::SUPPORTED_VERSIONS)
            }
            StateStoreDataKey::WellKnown => Cow::Borrowed(StateStoreDataKey::WELL_KNOWN),
            StateStoreDataKey::Filter(f) => {
                Cow::Owned(format!("{}:{f}", StateStoreDataKey::FILTER))
            }
            StateStoreDataKey::UserAvatarUrl(u) => {
                Cow::Owned(format!("{}:{u}", StateStoreDataKey::USER_AVATAR_URL))
            }
            StateStoreDataKey::RecentlyVisitedRooms(b) => {
                Cow::Owned(format!("{}:{b}", StateStoreDataKey::RECENTLY_VISITED_ROOMS))
            }
            StateStoreDataKey::UtdHookManagerData => {
                Cow::Borrowed(StateStoreDataKey::UTD_HOOK_MANAGER_DATA)
            }
            StateStoreDataKey::OneTimeKeyAlreadyUploaded => {
                Cow::Borrowed(StateStoreDataKey::ONE_TIME_KEY_ALREADY_UPLOADED)
            }
            StateStoreDataKey::ComposerDraft(room_id, thread_root) => {
                if let Some(thread_root) = thread_root {
                    Cow::Owned(format!(
                        "{}:{room_id}:{thread_root}",
                        StateStoreDataKey::COMPOSER_DRAFT
                    ))
                } else {
                    Cow::Owned(format!("{}:{room_id}", StateStoreDataKey::COMPOSER_DRAFT))
                }
            }
            StateStoreDataKey::SeenKnockRequests(room_id) => {
                Cow::Owned(format!("{}:{room_id}", StateStoreDataKey::SEEN_KNOCK_REQUESTS))
            }
            StateStoreDataKey::ThreadSubscriptionsCatchupTokens => {
                Cow::Borrowed(StateStoreDataKey::THREAD_SUBSCRIPTIONS_CATCHUP_TOKENS)
            }
            StateStoreDataKey::HomeserverCapabilities => {
                Cow::Borrowed(StateStoreDataKey::HOMESERVER_CAPABILITIES)
            }
        };

        self.encode_key(keys::KV_BLOB, &*key_s)
    }

    fn encode_presence_key(&self, user_id: &UserId) -> Key {
        self.encode_key(keys::KV_BLOB, format!("presence:{user_id}"))
    }

    fn encode_custom_key(&self, key: &[u8]) -> Key {
        let mut full_key = b"custom:".to_vec();
        full_key.extend(key);
        self.encode_key(keys::KV_BLOB, full_key)
    }

    /// Acquire a connection for executing read operations.
    /// Returns `StoreClosed` if closed.
    #[instrument(skip_all)]
    async fn read(&self) -> Result<SqliteAsyncConn> {
        let pool = {
            let guard = self.connections.lock().await;
            let conns = guard.as_ref().ok_or(Error::StoreClosed)?;
            conns.pool.clone()
        };
        Ok(pool.get().await?)
    }

    /// Acquire a connection for executing write operations.
    /// Returns `StoreClosed` if closed.
    #[instrument(skip_all)]
    async fn write(&self) -> Result<OwnedMutexGuard<SqliteAsyncConn>> {
        let write_conn = {
            let guard = self.connections.lock().await;
            let conns = guard.as_ref().ok_or(Error::StoreClosed)?;
            conns.write_connection.clone()
        };
        Ok(write_conn.lock_owned().await)
    }

    fn remove_maybe_stripped_room_data(
        &self,
        txn: &Transaction<'_>,
        room_id: &RoomId,
        stripped: bool,
    ) -> rusqlite::Result<()> {
        let state_event_room_id = self.encode_key(keys::STATE_EVENT, room_id);
        txn.remove_room_state_events(&state_event_room_id, Some(stripped))?;

        let member_room_id = self.encode_key(keys::MEMBER, room_id);
        txn.remove_room_members(&member_room_id, Some(stripped))
    }

    pub async fn vacuum(&self) -> Result<()> {
        self.write().await?.vacuum().await
    }

    pub async fn get_db_size(&self) -> Result<Option<usize>> {
        let read_conn = self.read().await?;
        Ok(Some(read_conn.get_db_size().await?))
    }
}

impl EncryptableStore for SqliteStateStore {
    fn get_cypher(&self) -> Option<&StoreCipher> {
        self.store_cipher.as_deref()
    }
}

/// Initialize the database.
async fn init(conn: &SqliteAsyncConn) -> Result<()> {
    // First turn on WAL mode, this can't be done in the transaction, it fails with
    // the error message: "cannot change into wal mode from within a transaction".
    conn.execute_batch("PRAGMA journal_mode = wal;").await?;
    conn.with_transaction(|txn| {
        txn.execute_batch(include_str!("../migrations/state_store/001_init.sql"))?;
        txn.set_db_version(1)?;

        Ok(())
    })
    .await
}

trait SqliteConnectionStateStoreExt {
    fn set_kv_blob(&self, key: &[u8], value: &[u8]) -> rusqlite::Result<()>;

    fn set_global_account_data(&self, event_type: &[u8], data: &[u8]) -> rusqlite::Result<()>;

    fn set_room_account_data(
        &self,
        room_id: &[u8],
        event_type: &[u8],
        data: &[u8],
    ) -> rusqlite::Result<()>;
    fn remove_room_account_data(&self, room_id: &[u8]) -> rusqlite::Result<()>;

    fn set_room_info(&self, room_id: &[u8], state: &[u8], data: &[u8]) -> rusqlite::Result<()>;
    fn get_room_info(&self, room_id: &[u8]) -> rusqlite::Result<Option<Vec<u8>>>;
    fn remove_room_info(&self, room_id: &[u8]) -> rusqlite::Result<()>;

    fn set_state_event(
        &self,
        room_id: &[u8],
        event_type: &[u8],
        state_key: &[u8],
        stripped: bool,
        event_id: Option<&[u8]>,
        data: &[u8],
    ) -> rusqlite::Result<()>;
    fn get_state_event_by_id(
        &self,
        room_id: &[u8],
        event_id: &[u8],
    ) -> rusqlite::Result<Option<Vec<u8>>>;
    fn remove_room_state_events(
        &self,
        room_id: &[u8],
        stripped: Option<bool>,
    ) -> rusqlite::Result<()>;

    fn set_member(
        &self,
        room_id: &[u8],
        user_id: &[u8],
        membership: &[u8],
        stripped: bool,
        data: &[u8],
    ) -> rusqlite::Result<()>;
    fn remove_room_members(&self, room_id: &[u8], stripped: Option<bool>) -> rusqlite::Result<()>;

    fn set_profile(&self, room_id: &[u8], user_id: &[u8], data: &[u8]) -> rusqlite::Result<()>;
    fn remove_room_profiles(&self, room_id: &[u8]) -> rusqlite::Result<()>;
    fn remove_room_profile(&self, room_id: &[u8], user_id: &[u8]) -> rusqlite::Result<()>;

    fn set_receipt(
        &self,
        room_id: &[u8],
        user_id: &[u8],
        receipt_type: &[u8],
        thread_id: &[u8],
        event_id: &[u8],
        data: &[u8],
    ) -> rusqlite::Result<()>;
    fn remove_room_receipts(&self, room_id: &[u8]) -> rusqlite::Result<()>;

    fn set_display_name(&self, room_id: &[u8], name: &[u8], data: &[u8]) -> rusqlite::Result<()>;
    fn remove_display_name(&self, room_id: &[u8], name: &[u8]) -> rusqlite::Result<()>;
    fn remove_room_display_names(&self, room_id: &[u8]) -> rusqlite::Result<()>;
    fn remove_room_send_queue(&self, room_id: &[u8]) -> rusqlite::Result<()>;
    fn remove_room_dependent_send_queue(&self, room_id: &[u8]) -> rusqlite::Result<()>;
}

impl SqliteConnectionStateStoreExt for rusqlite::Connection {
    fn set_kv_blob(&self, key: &[u8], value: &[u8]) -> rusqlite::Result<()> {
        self.execute("INSERT OR REPLACE INTO kv_blob VALUES (?, ?)", (key, value))?;
        Ok(())
    }

    fn set_global_account_data(&self, event_type: &[u8], data: &[u8]) -> rusqlite::Result<()> {
        self.prepare_cached(
            "INSERT OR REPLACE INTO global_account_data (event_type, data)
             VALUES (?, ?)",
        )?
        .execute((event_type, data))?;
        Ok(())
    }

    fn set_room_account_data(
        &self,
        room_id: &[u8],
        event_type: &[u8],
        data: &[u8],
    ) -> rusqlite::Result<()> {
        self.prepare_cached(
            "INSERT OR REPLACE INTO room_account_data (room_id, event_type, data)
             VALUES (?, ?, ?)",
        )?
        .execute((room_id, event_type, data))?;
        Ok(())
    }

    fn remove_room_account_data(&self, room_id: &[u8]) -> rusqlite::Result<()> {
        self.prepare(
            "DELETE FROM room_account_data
             WHERE room_id = ?",
        )?
        .execute((room_id,))?;
        Ok(())
    }

    fn set_room_info(&self, room_id: &[u8], state: &[u8], data: &[u8]) -> rusqlite::Result<()> {
        self.prepare_cached(
            "INSERT OR REPLACE INTO room_info (room_id, state, data)
             VALUES (?, ?, ?)",
        )?
        .execute((room_id, state, data))?;
        Ok(())
    }

    fn get_room_info(&self, room_id: &[u8]) -> rusqlite::Result<Option<Vec<u8>>> {
        self.query_one("SELECT data FROM room_info WHERE room_id = ?", (room_id,), |row| row.get(0))
            .optional()
    }

    /// Remove the room info for the given room.
    fn remove_room_info(&self, room_id: &[u8]) -> rusqlite::Result<()> {
        self.prepare_cached("DELETE FROM room_info WHERE room_id = ?")?.execute((room_id,))?;
        Ok(())
    }

    fn set_state_event(
        &self,
        room_id: &[u8],
        event_type: &[u8],
        state_key: &[u8],
        stripped: bool,
        event_id: Option<&[u8]>,
        data: &[u8],
    ) -> rusqlite::Result<()> {
        self.prepare_cached(
            "INSERT OR REPLACE
             INTO state_event (room_id, event_type, state_key, stripped, event_id, data)
             VALUES (?, ?, ?, ?, ?, ?)",
        )?
        .execute((room_id, event_type, state_key, stripped, event_id, data))?;
        Ok(())
    }

    fn get_state_event_by_id(
        &self,
        room_id: &[u8],
        event_id: &[u8],
    ) -> rusqlite::Result<Option<Vec<u8>>> {
        self.query_one(
            "SELECT data FROM state_event WHERE room_id = ? AND event_id = ?",
            (room_id, event_id),
            |row| row.get(0),
        )
        .optional()
    }

    /// Remove state events for the given room.
    ///
    /// If `stripped` is `Some()`, only removes state events for the given
    /// stripped state. Otherwise, state events are removed regardless of the
    /// stripped state.
    fn remove_room_state_events(
        &self,
        room_id: &[u8],
        stripped: Option<bool>,
    ) -> rusqlite::Result<()> {
        if let Some(stripped) = stripped {
            self.prepare_cached("DELETE FROM state_event WHERE room_id = ? AND stripped = ?")?
                .execute((room_id, stripped))?;
        } else {
            self.prepare_cached("DELETE FROM state_event WHERE room_id = ?")?
                .execute((room_id,))?;
        }
        Ok(())
    }

    fn set_member(
        &self,
        room_id: &[u8],
        user_id: &[u8],
        membership: &[u8],
        stripped: bool,
        data: &[u8],
    ) -> rusqlite::Result<()> {
        self.prepare_cached(
            "INSERT OR REPLACE
             INTO member (room_id, user_id, membership, stripped, data)
             VALUES (?, ?, ?, ?, ?)",
        )?
        .execute((room_id, user_id, membership, stripped, data))?;
        Ok(())
    }

    /// Remove members for the given room.
    ///
    /// If `stripped` is `Some()`, only removes members for the given stripped
    /// state. Otherwise, members are removed regardless of the stripped state.
    fn remove_room_members(&self, room_id: &[u8], stripped: Option<bool>) -> rusqlite::Result<()> {
        if let Some(stripped) = stripped {
            self.prepare_cached("DELETE FROM member WHERE room_id = ? AND stripped = ?")?
                .execute((room_id, stripped))?;
        } else {
            self.prepare_cached("DELETE FROM member WHERE room_id = ?")?.execute((room_id,))?;
        }
        Ok(())
    }

    fn set_profile(&self, room_id: &[u8], user_id: &[u8], data: &[u8]) -> rusqlite::Result<()> {
        self.prepare_cached(
            "INSERT OR REPLACE
             INTO profile (room_id, user_id, data)
             VALUES (?, ?, ?)",
        )?
        .execute((room_id, user_id, data))?;
        Ok(())
    }

    fn remove_room_profiles(&self, room_id: &[u8]) -> rusqlite::Result<()> {
        self.prepare("DELETE FROM profile WHERE room_id = ?")?.execute((room_id,))?;
        Ok(())
    }

    fn remove_room_profile(&self, room_id: &[u8], user_id: &[u8]) -> rusqlite::Result<()> {
        self.prepare("DELETE FROM profile WHERE room_id = ? AND user_id = ?")?
            .execute((room_id, user_id))?;
        Ok(())
    }

    fn set_receipt(
        &self,
        room_id: &[u8],
        user_id: &[u8],
        receipt_type: &[u8],
        thread: &[u8],
        event_id: &[u8],
        data: &[u8],
    ) -> rusqlite::Result<()> {
        self.prepare_cached(
            "INSERT OR REPLACE
             INTO receipt (room_id, user_id, receipt_type, thread, event_id, data)
             VALUES (?, ?, ?, ?, ?, ?)",
        )?
        .execute((room_id, user_id, receipt_type, thread, event_id, data))?;
        Ok(())
    }

    fn remove_room_receipts(&self, room_id: &[u8]) -> rusqlite::Result<()> {
        self.prepare("DELETE FROM receipt WHERE room_id = ?")?.execute((room_id,))?;
        Ok(())
    }

    fn set_display_name(&self, room_id: &[u8], name: &[u8], data: &[u8]) -> rusqlite::Result<()> {
        self.prepare_cached(
            "INSERT OR REPLACE
             INTO display_name (room_id, name, data)
             VALUES (?, ?, ?)",
        )?
        .execute((room_id, name, data))?;
        Ok(())
    }

    fn remove_display_name(&self, room_id: &[u8], name: &[u8]) -> rusqlite::Result<()> {
        self.prepare("DELETE FROM display_name WHERE room_id = ? AND name = ?")?
            .execute((room_id, name))?;
        Ok(())
    }

    fn remove_room_display_names(&self, room_id: &[u8]) -> rusqlite::Result<()> {
        self.prepare("DELETE FROM display_name WHERE room_id = ?")?.execute((room_id,))?;
        Ok(())
    }

    fn remove_room_send_queue(&self, room_id: &[u8]) -> rusqlite::Result<()> {
        self.prepare("DELETE FROM send_queue_events WHERE room_id = ?")?.execute((room_id,))?;
        Ok(())
    }

    fn remove_room_dependent_send_queue(&self, room_id: &[u8]) -> rusqlite::Result<()> {
        self.prepare("DELETE FROM dependent_send_queue_events WHERE room_id = ?")?
            .execute((room_id,))?;
        Ok(())
    }
}

#[async_trait]
trait SqliteObjectStateStoreExt: SqliteAsyncConnExt {
    async fn get_kv_blob(&self, key: Key) -> Result<Option<Vec<u8>>> {
        Ok(self
            .query_one("SELECT value FROM kv_blob WHERE key = ?", (key,), |row| row.get(0))
            .await
            .optional()?)
    }

    async fn get_kv_blobs(&self, keys: Vec<Key>) -> Result<Vec<Vec<u8>>> {
        let keys_length = keys.len();

        self.chunk_large_query_over(keys, Some(keys_length), |txn, keys| {
            let sql =
                format!("SELECT value FROM kv_blob WHERE key IN ({})", keys.host_parameters());

            let params = rusqlite::params_from_iter(keys);

            Ok(txn
                .prepare(&sql)?
                .query(params)?
                .mapped(|row| row.get(0))
                .collect::<Result<_, _>>()?)
        })
        .await
    }

    async fn set_kv_blob(&self, key: Key, value: Vec<u8>) -> Result<()>;

    async fn delete_kv_blob(&self, key: Key) -> Result<()> {
        self.execute("DELETE FROM kv_blob WHERE key = ?", (key,)).await?;
        Ok(())
    }

    async fn get_room_infos(&self, room_id: Option<Key>) -> Result<Vec<Vec<u8>>> {
        Ok(match room_id {
            None => {
                self.prepare("SELECT data FROM room_info", move |mut stmt| {
                    stmt.query_map((), |row| row.get(0))?.collect()
                })
                .await?
            }

            Some(room_id) => {
                self.prepare("SELECT data FROM room_info WHERE room_id = ?", move |mut stmt| {
                    stmt.query((room_id,))?.mapped(|row| row.get(0)).collect()
                })
                .await?
            }
        })
    }

    async fn get_maybe_stripped_state_events_for_keys(
        &self,
        room_id: Key,
        event_type: Key,
        state_keys: Vec<Key>,
    ) -> Result<Vec<(bool, Vec<u8>)>> {
        self.chunk_large_query_over(state_keys, None, move |txn, state_keys| {
            let sql = format!(
                "SELECT stripped, data FROM state_event
                 WHERE room_id = ? AND event_type = ? AND state_key IN ({})",
                state_keys.host_parameters()
            );

            let params = rusqlite::params_from_iter(
                [room_id.clone(), event_type.clone()].into_iter().chain(state_keys),
            );

            Ok(txn
                .prepare(&sql)?
                .query(params)?
                .mapped(|row| Ok((row.get(0)?, row.get(1)?)))
                .collect::<Result<_, _>>()?)
        })
        .await
    }

    async fn get_maybe_stripped_state_events(
        &self,
        room_id: Key,
        event_type: Key,
    ) -> Result<Vec<(bool, Vec<u8>)>> {
        Ok(self
            .prepare(
                "SELECT stripped, data FROM state_event
                 WHERE room_id = ? AND event_type = ?",
                |mut stmt| {
                    stmt.query((room_id, event_type))?
                        .mapped(|row| Ok((row.get(0)?, row.get(1)?)))
                        .collect()
                },
            )
            .await?)
    }

    async fn get_profiles(
        &self,
        room_id: Key,
        user_ids: Vec<Key>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let user_ids_length = user_ids.len();

        self.chunk_large_query_over(user_ids, Some(user_ids_length), move |txn, user_ids| {
            let sql = format!(
                "SELECT user_id, data FROM profile WHERE room_id = ? AND user_id IN ({})",
                user_ids.host_parameters(),
            );

            let params = rusqlite::params_from_iter(iter::once(room_id.clone()).chain(user_ids));

            Ok(txn
                .prepare(&sql)?
                .query(params)?
                .mapped(|row| Ok((row.get(0)?, row.get(1)?)))
                .collect::<Result<_, _>>()?)
        })
        .await
    }

    async fn get_global_profiles(&self, user_ids: Vec<Key>) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let user_ids_length = user_ids.len();

        self.chunk_large_query_over(user_ids, Some(user_ids_length), move |txn, user_ids| {
            let sql = format!(
                "SELECT user_id, profile_data FROM global_profiles WHERE user_id IN ({})",
                user_ids.host_parameters(),
            );

            let params = rusqlite::params_from_iter(user_ids);

            Ok(txn
                .prepare(&sql)?
                .query(params)?
                .mapped(|row| Ok((row.get(0)?, row.get(1)?)))
                .collect::<Result<_, _>>()?)
        })
        .await
    }

    async fn get_user_ids(&self, room_id: Key, memberships: Vec<Key>) -> Result<Vec<Vec<u8>>> {
        let res = if memberships.is_empty() {
            self.prepare("SELECT data FROM member WHERE room_id = ?", |mut stmt| {
                stmt.query((room_id,))?.mapped(|row| row.get(0)).collect()
            })
            .await?
        } else {
            self.chunk_large_query_over(memberships, None, move |txn, memberships| {
                let sql = format!(
                    "SELECT data FROM member WHERE room_id = ? AND membership IN ({})",
                    memberships.host_parameters(),
                );

                let params =
                    rusqlite::params_from_iter(iter::once(room_id.clone()).chain(memberships));

                Ok(txn
                    .prepare(&sql)?
                    .query(params)?
                    .mapped(|row| row.get(0))
                    .collect::<Result<_, _>>()?)
            })
            .await?
        };

        Ok(res)
    }

    async fn get_global_account_data(&self, event_type: Key) -> Result<Option<Vec<u8>>> {
        Ok(self
            .query_one(
                "SELECT data FROM global_account_data WHERE event_type = ?",
                (event_type,),
                |row| row.get(0),
            )
            .await
            .optional()?)
    }

    async fn get_room_account_data(
        &self,
        room_id: Key,
        event_type: Key,
    ) -> Result<Option<Vec<u8>>> {
        Ok(self
            .query_one(
                "SELECT data FROM room_account_data WHERE room_id = ? AND event_type = ?",
                (room_id, event_type),
                |row| row.get(0),
            )
            .await
            .optional()?)
    }

    async fn get_display_names(
        &self,
        room_id: Key,
        names: Vec<Key>,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let names_length = names.len();

        self.chunk_large_query_over(names, Some(names_length), move |txn, names| {
            let sql = format!(
                "SELECT name, data FROM display_name WHERE room_id = ? AND name IN ({})",
                names.host_parameters()
            );

            let params = rusqlite::params_from_iter(iter::once(room_id.clone()).chain(names));

            Ok(txn
                .prepare(&sql)?
                .query(params)?
                .mapped(|row| Ok((row.get(0)?, row.get(1)?)))
                .collect::<Result<_, _>>()?)
        })
        .await
    }

    async fn get_user_receipt(
        &self,
        room_id: Key,
        receipt_type: Key,
        receipt_thread: Key,
        user_id: Key,
    ) -> Result<Option<Vec<u8>>> {
        Ok(self
            .query_one(
                "SELECT data FROM receipt
                 WHERE room_id = ? AND receipt_type = ? AND thread = ? and user_id = ?",
                (room_id, receipt_type, receipt_thread, user_id),
                |row| row.get(0),
            )
            .await
            .optional()?)
    }

    async fn get_event_receipts(
        &self,
        room_id: Key,
        receipt_type: Key,
        thread: Key,
        event_id: Key,
    ) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .prepare(
                "SELECT data FROM receipt
                 WHERE room_id = ? AND receipt_type = ? AND thread = ? and event_id = ?",
                |mut stmt| {
                    stmt.query((room_id, receipt_type, thread, event_id))?
                        .mapped(|row| row.get(0))
                        .collect()
                },
            )
            .await?)
    }
}

#[async_trait]
impl SqliteObjectStateStoreExt for SqliteAsyncConn {
    async fn set_kv_blob(&self, key: Key, value: Vec<u8>) -> Result<()> {
        Ok(self.interact(move |conn| conn.set_kv_blob(&key, &value)).await.unwrap()?)
    }
}

#[async_trait]
impl StateStore for SqliteStateStore {
    type Error = Error;

    async fn get_kv_data(&self, key: StateStoreDataKey<'_>) -> Result<Option<StateStoreDataValue>> {
        self.read()
            .await?
            .get_kv_blob(self.encode_state_store_data_key(key))
            .await?
            .map(|data| {
                Ok(match key {
                    StateStoreDataKey::SyncToken => {
                        StateStoreDataValue::SyncToken(self.deserialize_value(&data)?)
                    }
                    StateStoreDataKey::SupportedVersions => {
                        StateStoreDataValue::SupportedVersions(self.deserialize_value(&data)?)
                    }
                    StateStoreDataKey::WellKnown => {
                        StateStoreDataValue::WellKnown(self.deserialize_value(&data)?)
                    }
                    StateStoreDataKey::Filter(_) => {
                        StateStoreDataValue::Filter(self.deserialize_value(&data)?)
                    }
                    StateStoreDataKey::UserAvatarUrl(_) => {
                        StateStoreDataValue::UserAvatarUrl(self.deserialize_value(&data)?)
                    }
                    StateStoreDataKey::RecentlyVisitedRooms(_) => {
                        StateStoreDataValue::RecentlyVisitedRooms(self.deserialize_value(&data)?)
                    }
                    StateStoreDataKey::UtdHookManagerData => {
                        StateStoreDataValue::UtdHookManagerData(self.deserialize_value(&data)?)
                    }
                    StateStoreDataKey::OneTimeKeyAlreadyUploaded => {
                        StateStoreDataValue::OneTimeKeyAlreadyUploaded
                    }
                    StateStoreDataKey::ComposerDraft(_, _) => {
                        StateStoreDataValue::ComposerDraft(self.deserialize_value(&data)?)
                    }
                    StateStoreDataKey::SeenKnockRequests(_) => {
                        StateStoreDataValue::SeenKnockRequests(self.deserialize_value(&data)?)
                    }
                    StateStoreDataKey::ThreadSubscriptionsCatchupTokens => {
                        StateStoreDataValue::ThreadSubscriptionsCatchupTokens(
                            self.deserialize_value(&data)?,
                        )
                    }
                    StateStoreDataKey::HomeserverCapabilities => {
                        StateStoreDataValue::HomeserverCapabilities(self.deserialize_value(&data)?)
                    }
                })
            })
            .transpose()
    }

    async fn set_kv_data(
        &self,
        key: StateStoreDataKey<'_>,
        value: StateStoreDataValue,
    ) -> Result<()> {
        let serialized_value = match key {
            StateStoreDataKey::SyncToken => self.serialize_value(
                &value.into_sync_token().expect("Session data not a sync token"),
            )?,
            StateStoreDataKey::SupportedVersions => self.serialize_value(
                &value
                    .into_supported_versions()
                    .expect("Session data not containing supported versions"),
            )?,
            StateStoreDataKey::WellKnown => self.serialize_value(
                &value.into_well_known().expect("Session data not containing well-known"),
            )?,
            StateStoreDataKey::Filter(_) => {
                self.serialize_value(&value.into_filter().expect("Session data not a filter"))?
            }
            StateStoreDataKey::UserAvatarUrl(_) => self.serialize_value(
                &value.into_user_avatar_url().expect("Session data not an user avatar url"),
            )?,
            StateStoreDataKey::RecentlyVisitedRooms(_) => self.serialize_value(
                &value.into_recently_visited_rooms().expect("Session data not breadcrumbs"),
            )?,
            StateStoreDataKey::UtdHookManagerData => self.serialize_value(
                &value.into_utd_hook_manager_data().expect("Session data not UtdHookManagerData"),
            )?,
            StateStoreDataKey::OneTimeKeyAlreadyUploaded => {
                self.serialize_value(&true).expect("We should be able to serialize a boolean")
            }
            StateStoreDataKey::ComposerDraft(_, _) => self.serialize_value(
                &value.into_composer_draft().expect("Session data not a composer draft"),
            )?,
            StateStoreDataKey::SeenKnockRequests(_) => self.serialize_value(
                &value
                    .into_seen_knock_requests()
                    .expect("Session data is not a set of seen knock request ids"),
            )?,
            StateStoreDataKey::ThreadSubscriptionsCatchupTokens => self.serialize_value(
                &value
                    .into_thread_subscriptions_catchup_tokens()
                    .expect("Session data is not a list of thread subscription catchup tokens"),
            )?,
            StateStoreDataKey::HomeserverCapabilities => self.serialize_value(
                &value
                    .into_homeserver_capabilities()
                    .expect("Session data is not the homeserver capabilities"),
            )?,
        };

        self.write()
            .await?
            .set_kv_blob(self.encode_state_store_data_key(key), serialized_value)
            .await
    }

    async fn remove_kv_data(&self, key: StateStoreDataKey<'_>) -> Result<()> {
        self.write().await?.delete_kv_blob(self.encode_state_store_data_key(key)).await
    }

    async fn save_changes(&self, changes: &StateChanges) -> Result<()> {
        let changes = changes.to_owned();
        let this = self.clone();
        self.write()
            .await?
            .with_transaction(move |txn| {
                let StateChanges {
                    sync_token,
                    account_data,
                    presence,
                    profiles,
                    profiles_to_delete,
                    state,
                    room_account_data,
                    room_infos,
                    receipts,
                    redactions,
                    stripped_state,
                    ambiguity_maps,
                    global_profiles,
                } = changes;

                if let Some(sync_token) = sync_token {
                    let key = this.encode_state_store_data_key(StateStoreDataKey::SyncToken);
                    let value = this.serialize_value(&sync_token)?;
                    txn.set_kv_blob(&key, &value)?;
                }

                for (event_type, event) in account_data {
                    let event_type =
                        this.encode_key(keys::GLOBAL_ACCOUNT_DATA, event_type.to_string());
                    let data = this.serialize_json(&event)?;
                    txn.set_global_account_data(&event_type, &data)?;
                }

                for (room_id, events) in room_account_data {
                    let room_id = this.encode_key(keys::ROOM_ACCOUNT_DATA, room_id);
                    for (event_type, event) in events {
                        let event_type =
                            this.encode_key(keys::ROOM_ACCOUNT_DATA, event_type.to_string());
                        let data = this.serialize_json(&event)?;
                        txn.set_room_account_data(&room_id, &event_type, &data)?;
                    }
                }

                for (user_id, event) in presence {
                    let key = this.encode_presence_key(&user_id);
                    let value = this.serialize_json(&event)?;
                    txn.set_kv_blob(&key, &value)?;
                }

                for (room_id, room_info) in room_infos {
                    let stripped = room_info.state() == RoomState::Invited;
                    // Remove non-stripped data for stripped rooms and vice-versa.
                    this.remove_maybe_stripped_room_data(txn, &room_id, !stripped)?;

                    let room_id = this.encode_key(keys::ROOM_INFO, room_id);
                    let state = this
                        .encode_key(keys::ROOM_INFO, serde_json::to_string(&room_info.state())?);
                    let data = this.serialize_json(&room_info)?;
                    txn.set_room_info(&room_id, &state, &data)?;
                }

                for (room_id, user_ids) in profiles_to_delete {
                    let room_id = this.encode_key(keys::PROFILE, room_id);
                    for user_id in user_ids {
                        let user_id = this.encode_key(keys::PROFILE, user_id);
                        txn.remove_room_profile(&room_id, &user_id)?;
                    }
                }

                for (room_id, state_event_types) in state {
                    let profiles = profiles.get(&room_id);
                    let encoded_room_id = this.encode_key(keys::STATE_EVENT, &room_id);

                    for (event_type, state_events) in state_event_types {
                        let encoded_event_type =
                            this.encode_key(keys::STATE_EVENT, event_type.to_string());

                        for (state_key, raw_state_event) in state_events {
                            let encoded_state_key = this.encode_key(keys::STATE_EVENT, &state_key);
                            let data = this.serialize_json(&raw_state_event)?;

                            let event_id: Option<String> =
                                raw_state_event.get_field("event_id").ok().flatten();
                            let encoded_event_id =
                                event_id.as_ref().map(|e| this.encode_key(keys::STATE_EVENT, e));

                            txn.set_state_event(
                                &encoded_room_id,
                                &encoded_event_type,
                                &encoded_state_key,
                                false,
                                encoded_event_id.as_deref(),
                                &data,
                            )?;

                            if event_type == StateEventType::RoomMember {
                                let member_event = match raw_state_event
                                    .deserialize_as_unchecked::<SyncRoomMemberEvent>()
                                {
                                    Ok(ev) => ev,
                                    Err(e) => {
                                        debug!(event_id, "Failed to deserialize member event: {e}");
                                        continue;
                                    }
                                };

                                let encoded_room_id = this.encode_key(keys::MEMBER, &room_id);
                                let user_id = this.encode_key(keys::MEMBER, &state_key);
                                let membership = this
                                    .encode_key(keys::MEMBER, member_event.membership().as_str());
                                let data = this.serialize_value(&state_key)?;

                                txn.set_member(
                                    &encoded_room_id,
                                    &user_id,
                                    &membership,
                                    false,
                                    &data,
                                )?;

                                if let Some(profile) =
                                    profiles.and_then(|p| p.get(member_event.state_key()))
                                {
                                    let room_id = this.encode_key(keys::PROFILE, &room_id);
                                    let user_id = this.encode_key(keys::PROFILE, &state_key);
                                    let data = this.serialize_json(&profile)?;
                                    txn.set_profile(&room_id, &user_id, &data)?;
                                }
                            }
                        }
                    }
                }

                for (room_id, stripped_state_event_types) in stripped_state {
                    let encoded_room_id = this.encode_key(keys::STATE_EVENT, &room_id);

                    for (event_type, stripped_state_events) in stripped_state_event_types {
                        let encoded_event_type =
                            this.encode_key(keys::STATE_EVENT, event_type.to_string());

                        for (state_key, raw_stripped_state_event) in stripped_state_events {
                            let encoded_state_key = this.encode_key(keys::STATE_EVENT, &state_key);
                            let data = this.serialize_json(&raw_stripped_state_event)?;
                            txn.set_state_event(
                                &encoded_room_id,
                                &encoded_event_type,
                                &encoded_state_key,
                                true,
                                None,
                                &data,
                            )?;

                            if event_type == StateEventType::RoomMember {
                                let member_event = match raw_stripped_state_event
                                    .deserialize_as_unchecked::<StrippedRoomMemberEvent>(
                                ) {
                                    Ok(ev) => ev,
                                    Err(e) => {
                                        debug!("Failed to deserialize stripped member event: {e}");
                                        continue;
                                    }
                                };

                                let room_id = this.encode_key(keys::MEMBER, &room_id);
                                let user_id = this.encode_key(keys::MEMBER, &state_key);
                                let membership = this.encode_key(
                                    keys::MEMBER,
                                    member_event.content.membership.as_str(),
                                );
                                let data = this.serialize_value(&state_key)?;

                                txn.set_member(&room_id, &user_id, &membership, true, &data)?;
                            }
                        }
                    }
                }

                for (room_id, receipt_event) in receipts {
                    let room_id = this.encode_key(keys::RECEIPT, room_id);

                    for (event_id, receipt_types) in receipt_event {
                        let encoded_event_id = this.encode_key(keys::RECEIPT, &event_id);

                        for (receipt_type, receipt_users) in receipt_types {
                            let receipt_type =
                                this.encode_key(keys::RECEIPT, receipt_type.as_str());

                            for (user_id, receipt) in receipt_users {
                                let encoded_user_id = this.encode_key(keys::RECEIPT, &user_id);
                                // We cannot have a NULL primary key so we rely on serialization
                                // instead of the string representation.
                                let thread = this.encode_key(
                                    keys::RECEIPT,
                                    rmp_serde::to_vec_named(&receipt.thread)?,
                                );
                                let data = this.serialize_json(&ReceiptData {
                                    receipt,
                                    event_id: event_id.clone(),
                                    user_id,
                                })?;

                                txn.set_receipt(
                                    &room_id,
                                    &encoded_user_id,
                                    &receipt_type,
                                    &thread,
                                    &encoded_event_id,
                                    &data,
                                )?;
                            }
                        }
                    }
                }

                for (room_id, redactions) in redactions {
                    let make_redaction_rules = || {
                        let encoded_room_id = this.encode_key(keys::ROOM_INFO, &room_id);
                        txn.get_room_info(&encoded_room_id)
                            .ok()
                            .flatten()
                            .and_then(|v| this.deserialize_json::<RoomInfo>(&v).ok())
                            .map(|info| info.room_version_rules_or_default())
                            .unwrap_or_else(|| {
                                warn!(
                                    ?room_id,
                                    "Unable to get the room version rules, defaulting to rules for room version {ROOM_VERSION_FALLBACK}"
                                );
                                ROOM_VERSION_RULES_FALLBACK
                            }).redaction
                    };

                    let encoded_room_id = this.encode_key(keys::STATE_EVENT, &room_id);
                    let mut redaction_rules = None;

                    for (event_id, redaction) in redactions {
                        let event_id = this.encode_key(keys::STATE_EVENT, event_id);

                        if let Some(Ok(raw_event)) = txn
                            .get_state_event_by_id(&encoded_room_id, &event_id)?
                            .map(|value| this.deserialize_json::<Raw<AnySyncStateEvent>>(&value))
                        {
                            let event = raw_event.deserialize()?;
                            let redacted = redact(
                                raw_event.deserialize_as::<CanonicalJsonObject>()?,
                                redaction_rules.get_or_insert_with(make_redaction_rules),
                                Some(RedactedBecause::from_raw_event(&redaction)?),
                            )
                            .map_err(Error::Redaction)?;
                            let data = this.serialize_json(&redacted)?;

                            let event_type =
                                this.encode_key(keys::STATE_EVENT, event.event_type().to_string());
                            let state_key = this.encode_key(keys::STATE_EVENT, event.state_key());

                            txn.set_state_event(
                                &encoded_room_id,
                                &event_type,
                                &state_key,
                                false,
                                Some(&event_id),
                                &data,
                            )?;
                        }
                    }
                }

                for (room_id, display_names) in ambiguity_maps {
                    let room_id = this.encode_key(keys::DISPLAY_NAME, room_id);

                    for (name, user_ids) in display_names {
                        let encoded_name = this.encode_key(
                            keys::DISPLAY_NAME,
                            name.as_normalized_str().unwrap_or_else(|| name.as_raw_str()),
                        );
                        let data = this.serialize_json(&user_ids)?;

                        if user_ids.is_empty() {
                            txn.remove_display_name(&room_id, &encoded_name)?;

                            // We can't do a migration to merge the previously distinct buckets of
                            // user IDs since the display names themselves are hashed before they
                            // are persisted in the store. So the store will always retain two
                            // buckets: one for raw display names and one for normalised ones.
                            //
                            // We therefore do the next best thing, which is a sort of a soft
                            // migration: we fetch both the raw and normalised buckets, then merge
                            // the user IDs contained in them into a separate, temporary merged
                            // bucket. The SDK then operates on the merged buckets exclusively. See
                            // the comment in `get_users_with_display_names` for details.
                            //
                            // If the merged bucket is empty, that must mean that both the raw and
                            // normalised buckets were also empty, so we can remove both from the
                            // store.
                            let raw_name = this.encode_key(keys::DISPLAY_NAME, name.as_raw_str());
                            txn.remove_display_name(&room_id, &raw_name)?;
                        } else {
                            // We only create new buckets with the normalized display name.
                            txn.set_display_name(&room_id, &encoded_name, &data)?;
                        }
                    }
                }

                for (raw_user_id, profile_update) in global_profiles {
                    let user_id = this.encode_key(keys::GLOBAL_PROFILES, &raw_user_id);
                    match profile_update {
                        UserProfileUpdate::Updated(profile_changes) => {
                            let existing_data: Option<Vec<u8>> = txn
                                .prepare_cached(
                                    "SELECT profile_data FROM global_profiles WHERE user_id = ?",
                                )?
                                .query_one([&user_id], |row| row.get(0))
                                .optional()?;

                            let mut profile: UserProfile = existing_data
                                .map(|data| this.deserialize_json(&data))
                                .transpose()?
                                .unwrap_or_default();
                            profile.apply(profile_changes);

                            let serialized = this.serialize_json(&profile)?;
                            txn.prepare_cached(
                                "INSERT OR REPLACE INTO global_profiles (user_id, profile_data) VALUES (?, ?)",
                            )?
                            .execute((&user_id, serialized))?;
                        }
                        // The user left all shared rooms, so drop their stored profile.
                        UserProfileUpdate::Dropped => {
                            txn.prepare_cached("DELETE FROM global_profiles WHERE user_id = ?")?
                                .execute([&user_id])?;
                        }
                        _ => {
                            warn!(%raw_user_id, "Unhandled UserProfileUpdate variant; ignoring");
                        }
                    }
                }

                Ok::<_, Error>(())
            })
            .await?;

        Ok(())
    }

    async fn get_presence_event(&self, user_id: &UserId) -> Result<Option<Raw<PresenceEvent>>> {
        self.read()
            .await?
            .get_kv_blob(self.encode_presence_key(user_id))
            .await?
            .map(|data| self.deserialize_json(&data))
            .transpose()
    }

    async fn get_presence_events(
        &self,
        user_ids: &[OwnedUserId],
    ) -> Result<Vec<Raw<PresenceEvent>>> {
        if user_ids.is_empty() {
            return Ok(Vec::new());
        }

        let user_ids = user_ids.iter().map(|u| self.encode_presence_key(u)).collect();
        self.read()
            .await?
            .get_kv_blobs(user_ids)
            .await?
            .into_iter()
            .map(|data| self.deserialize_json(&data))
            .collect()
    }

    async fn get_state_event(
        &self,
        room_id: &RoomId,
        event_type: StateEventType,
        state_key: &str,
    ) -> Result<Option<RawAnySyncOrStrippedState>> {
        Ok(self
            .get_state_events_for_keys(room_id, event_type, &[state_key])
            .await?
            .into_iter()
            .next())
    }

    async fn get_state_events(
        &self,
        room_id: &RoomId,
        event_type: StateEventType,
    ) -> Result<Vec<RawAnySyncOrStrippedState>> {
        let room_id = self.encode_key(keys::STATE_EVENT, room_id);
        let event_type = self.encode_key(keys::STATE_EVENT, event_type.to_string());
        self.read()
            .await?
            .get_maybe_stripped_state_events(room_id, event_type)
            .await?
            .into_iter()
            .map(|(stripped, data)| {
                let ev = if stripped {
                    RawAnySyncOrStrippedState::Stripped(self.deserialize_json(&data)?)
                } else {
                    RawAnySyncOrStrippedState::Sync(self.deserialize_json(&data)?)
                };

                Ok(ev)
            })
            .collect()
    }

    async fn get_state_events_for_keys(
        &self,
        room_id: &RoomId,
        event_type: StateEventType,
        state_keys: &[&str],
    ) -> Result<Vec<RawAnySyncOrStrippedState>, Self::Error> {
        if state_keys.is_empty() {
            return Ok(Vec::new());
        }

        let room_id = self.encode_key(keys::STATE_EVENT, room_id);
        let event_type = self.encode_key(keys::STATE_EVENT, event_type.to_string());
        let state_keys = state_keys.iter().map(|k| self.encode_key(keys::STATE_EVENT, k)).collect();
        self.read()
            .await?
            .get_maybe_stripped_state_events_for_keys(room_id, event_type, state_keys)
            .await?
            .into_iter()
            .map(|(stripped, data)| {
                let ev = if stripped {
                    RawAnySyncOrStrippedState::Stripped(self.deserialize_json(&data)?)
                } else {
                    RawAnySyncOrStrippedState::Sync(self.deserialize_json(&data)?)
                };

                Ok(ev)
            })
            .collect()
    }

    async fn get_profile(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<Option<MinimalRoomMemberEvent>> {
        let room_id = self.encode_key(keys::PROFILE, room_id);
        let user_ids = vec![self.encode_key(keys::PROFILE, user_id)];

        self.read()
            .await?
            .get_profiles(room_id, user_ids)
            .await?
            .into_iter()
            .next()
            .map(|(_, data)| self.deserialize_json(&data))
            .transpose()
    }

    async fn get_profiles<'a>(
        &self,
        room_id: &RoomId,
        user_ids: &'a [OwnedUserId],
    ) -> Result<BTreeMap<&'a UserId, MinimalRoomMemberEvent>> {
        if user_ids.is_empty() {
            return Ok(BTreeMap::new());
        }

        let room_id = self.encode_key(keys::PROFILE, room_id);
        let mut user_ids_map = user_ids
            .iter()
            .map(|u| (self.encode_key(keys::PROFILE, u), u.as_ref()))
            .collect::<BTreeMap<_, _>>();
        let user_ids = user_ids_map.keys().cloned().collect();

        self.read()
            .await?
            .get_profiles(room_id, user_ids)
            .await?
            .into_iter()
            .map(|(user_id, data)| {
                Ok((
                    user_ids_map
                        .remove(user_id.as_slice())
                        .expect("returned user IDs were requested"),
                    self.deserialize_json(&data)?,
                ))
            })
            .collect()
    }

    async fn get_user_ids(
        &self,
        room_id: &RoomId,
        membership: RoomMemberships,
    ) -> Result<Vec<OwnedUserId>> {
        let room_id = self.encode_key(keys::MEMBER, room_id);
        let memberships = membership
            .as_vec()
            .into_iter()
            .map(|m| self.encode_key(keys::MEMBER, m.as_str()))
            .collect();
        self.read()
            .await?
            .get_user_ids(room_id, memberships)
            .await?
            .iter()
            .map(|data| self.deserialize_value(data))
            .collect()
    }

    async fn get_room_infos(&self, room_load_settings: &RoomLoadSettings) -> Result<Vec<RoomInfo>> {
        self.read()
            .await?
            .get_room_infos(match room_load_settings {
                RoomLoadSettings::All => None,
                RoomLoadSettings::One(room_id) => Some(self.encode_key(keys::ROOM_INFO, room_id)),
            })
            .await?
            .into_iter()
            .map(|data| self.deserialize_json(&data))
            .collect()
    }

    async fn get_users_with_display_name(
        &self,
        room_id: &RoomId,
        display_name: &DisplayName,
    ) -> Result<BTreeSet<OwnedUserId>> {
        let room_id = self.encode_key(keys::DISPLAY_NAME, room_id);
        let names = vec![self.encode_key(
            keys::DISPLAY_NAME,
            display_name.as_normalized_str().unwrap_or_else(|| display_name.as_raw_str()),
        )];

        Ok(self
            .read()
            .await?
            .get_display_names(room_id, names)
            .await?
            .into_iter()
            .next()
            .map(|(_, data)| self.deserialize_json(&data))
            .transpose()?
            .unwrap_or_default())
    }

    async fn get_users_with_display_names<'a>(
        &self,
        room_id: &RoomId,
        display_names: &'a [DisplayName],
    ) -> Result<HashMap<&'a DisplayName, BTreeSet<OwnedUserId>>> {
        let mut result = HashMap::new();

        if display_names.is_empty() {
            return Ok(result);
        }

        let room_id = self.encode_key(keys::DISPLAY_NAME, room_id);
        let mut names_map = display_names
            .iter()
            .flat_map(|display_name| {
                // We encode the display name as the `raw_str()` and the normalized string.
                //
                // This is for compatibility reasons since:
                //  1. Previously "Alice" and "alice" were considered to be distinct display
                //     names, while we now consider them to be the same so we need to merge the
                //     previously distinct buckets of user IDs.
                //  2. We can't do a migration to merge the previously distinct buckets of user
                //     IDs since the display names itself are hashed before they are persisted
                //     in the store.
                let raw =
                    (self.encode_key(keys::DISPLAY_NAME, display_name.as_raw_str()), display_name);
                let normalized = display_name.as_normalized_str().map(|normalized| {
                    (self.encode_key(keys::DISPLAY_NAME, normalized), display_name)
                });

                iter::once(raw).chain(normalized)
            })
            .collect::<BTreeMap<_, _>>();
        let names = names_map.keys().cloned().collect();

        for (name, data) in self.read().await?.get_display_names(room_id, names).await?.into_iter()
        {
            let display_name =
                names_map.remove(name.as_slice()).expect("returned display names were requested");
            let user_ids: BTreeSet<_> = self.deserialize_json(&data)?;

            result.entry(display_name).or_insert_with(BTreeSet::new).extend(user_ids);
        }

        Ok(result)
    }

    async fn get_account_data_event(
        &self,
        event_type: GlobalAccountDataEventType,
    ) -> Result<Option<Raw<AnyGlobalAccountDataEvent>>> {
        let event_type = self.encode_key(keys::GLOBAL_ACCOUNT_DATA, event_type.to_string());
        self.read()
            .await?
            .get_global_account_data(event_type)
            .await?
            .map(|value| self.deserialize_json(&value))
            .transpose()
    }

    async fn get_room_account_data_event(
        &self,
        room_id: &RoomId,
        event_type: RoomAccountDataEventType,
    ) -> Result<Option<Raw<AnyRoomAccountDataEvent>>> {
        let room_id = self.encode_key(keys::ROOM_ACCOUNT_DATA, room_id);
        let event_type = self.encode_key(keys::ROOM_ACCOUNT_DATA, event_type.to_string());
        self.read()
            .await?
            .get_room_account_data(room_id, event_type)
            .await?
            .map(|value| self.deserialize_json(&value))
            .transpose()
    }

    async fn get_user_room_receipt_event(
        &self,
        room_id: &RoomId,
        receipt_type: ReceiptType,
        receipt_thread: &ReceiptThread,
        user_id: &UserId,
    ) -> Result<Option<(OwnedEventId, Receipt)>> {
        let room_id = self.encode_key(keys::RECEIPT, room_id);
        let receipt_type = self.encode_key(keys::RECEIPT, receipt_type.to_string());
        // We cannot have a NULL primary key so we rely on serialization instead of the
        // string representation.
        let receipt_thread =
            self.encode_key(keys::RECEIPT, rmp_serde::to_vec_named(receipt_thread)?);
        let user_id = self.encode_key(keys::RECEIPT, user_id);

        self.read()
            .await?
            .get_user_receipt(room_id, receipt_type, receipt_thread, user_id)
            .await?
            .map(|value| {
                self.deserialize_json::<ReceiptData>(&value).map(|d| (d.event_id, d.receipt))
            })
            .transpose()
    }

    async fn get_event_room_receipt_events(
        &self,
        room_id: &RoomId,
        receipt_type: ReceiptType,
        receipt_thread: &ReceiptThread,
        event_id: &EventId,
    ) -> Result<Vec<(OwnedUserId, Receipt)>> {
        let room_id = self.encode_key(keys::RECEIPT, room_id);
        let receipt_type = self.encode_key(keys::RECEIPT, receipt_type.to_string());
        // We cannot have a NULL primary key so we rely on serialization instead of the
        // string representation.
        let receipt_thread =
            self.encode_key(keys::RECEIPT, rmp_serde::to_vec_named(receipt_thread)?);
        let event_id = self.encode_key(keys::RECEIPT, event_id);

        self.read()
            .await?
            .get_event_receipts(room_id, receipt_type, receipt_thread, event_id)
            .await?
            .iter()
            .map(|value| {
                self.deserialize_json::<ReceiptData>(value).map(|d| (d.user_id, d.receipt))
            })
            .collect()
    }

    async fn get_custom_value(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.read().await?.get_kv_blob(self.encode_custom_key(key)).await
    }

    async fn set_custom_value_no_read(&self, key: &[u8], value: Vec<u8>) -> Result<()> {
        let conn = self.write().await?;
        let key = self.encode_custom_key(key);
        conn.set_kv_blob(key, value).await?;
        Ok(())
    }

    async fn set_custom_value(&self, key: &[u8], value: Vec<u8>) -> Result<Option<Vec<u8>>> {
        let conn = self.write().await?;
        let key = self.encode_custom_key(key);
        let previous = conn.get_kv_blob(key.clone()).await?;
        conn.set_kv_blob(key, value).await?;
        Ok(previous)
    }

    async fn remove_custom_value(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let conn = self.write().await?;
        let key = self.encode_custom_key(key);
        let previous = conn.get_kv_blob(key.clone()).await?;
        if previous.is_some() {
            conn.delete_kv_blob(key).await?;
        }
        Ok(previous)
    }

    async fn remove_room(&self, room_id: &RoomId) -> Result<()> {
        let this = self.clone();
        let room_id = room_id.to_owned();

        let conn = self.write().await?;

        conn.with_transaction(move |txn| -> Result<()> {
            let room_info_room_id = this.encode_key(keys::ROOM_INFO, &room_id);
            txn.remove_room_info(&room_info_room_id)?;

            let state_event_room_id = this.encode_key(keys::STATE_EVENT, &room_id);
            txn.remove_room_state_events(&state_event_room_id, None)?;

            let member_room_id = this.encode_key(keys::MEMBER, &room_id);
            txn.remove_room_members(&member_room_id, None)?;

            let profile_room_id = this.encode_key(keys::PROFILE, &room_id);
            txn.remove_room_profiles(&profile_room_id)?;

            let room_account_data_room_id = this.encode_key(keys::ROOM_ACCOUNT_DATA, &room_id);
            txn.remove_room_account_data(&room_account_data_room_id)?;

            let receipt_room_id = this.encode_key(keys::RECEIPT, &room_id);
            txn.remove_room_receipts(&receipt_room_id)?;

            let display_name_room_id = this.encode_key(keys::DISPLAY_NAME, &room_id);
            txn.remove_room_display_names(&display_name_room_id)?;

            let send_queue_room_id = this.encode_key(keys::SEND_QUEUE, &room_id);
            txn.remove_room_send_queue(&send_queue_room_id)?;

            let dependent_send_queue_room_id =
                this.encode_key(keys::DEPENDENTS_SEND_QUEUE, &room_id);
            txn.remove_room_dependent_send_queue(&dependent_send_queue_room_id)?;

            let thread_subscriptions_room_id =
                this.encode_key(keys::THREAD_SUBSCRIPTIONS, &room_id);
            txn.execute(
                "DELETE FROM thread_subscriptions WHERE room_id = ?",
                (thread_subscriptions_room_id,),
            )?;

            Ok(())
        })
        .await?;

        conn.vacuum().await
    }

    async fn save_send_queue_request(
        &self,
        room_id: &RoomId,
        transaction_id: OwnedTransactionId,
        created_at: MilliSecondsSinceUnixEpoch,
        content: QueuedRequestKind,
        priority: usize,
    ) -> Result<(), Self::Error> {
        let room_id_key = self.encode_key(keys::SEND_QUEUE, room_id);
        let room_id_value = self.serialize_value(&room_id.to_owned())?;

        let content = self.serialize_json(&content)?;
        // The transaction id is used both as a key (in remove/update) and a value (as
        // it's useful for the callers), so we keep it as is, and neither hash
        // it (with encode_key) or encrypt it (through serialize_value). After
        // all, it carries no personal information, so this is considered fine.

        let created_at_ts: u64 = created_at.0.into();
        self.write()
            .await?
            .with_transaction(move |txn| {
                txn.prepare_cached("INSERT INTO send_queue_events (room_id, room_id_val, transaction_id, content, priority, created_at) VALUES (?, ?, ?, ?, ?, ?)")?.execute((room_id_key, room_id_value, transaction_id.to_string(), content, priority, created_at_ts))?;
                Ok(())
            })
            .await
    }

    async fn update_send_queue_request(
        &self,
        room_id: &RoomId,
        transaction_id: &TransactionId,
        content: QueuedRequestKind,
    ) -> Result<bool, Self::Error> {
        let room_id = self.encode_key(keys::SEND_QUEUE, room_id);

        let content = self.serialize_json(&content)?;
        // See comment in [`Self::save_send_queue_request`] to understand why the
        // transaction id is neither encrypted or hashed.
        let transaction_id = transaction_id.to_string();

        let num_updated = self.write()
            .await?
            .with_transaction(move |txn| {
                txn.prepare_cached("UPDATE send_queue_events SET wedge_reason = NULL, content = ? WHERE room_id = ? AND transaction_id = ?")?.execute((content, room_id, transaction_id))
            })
            .await?;

        Ok(num_updated > 0)
    }

    async fn remove_send_queue_request(
        &self,
        room_id: &RoomId,
        transaction_id: &TransactionId,
    ) -> Result<bool, Self::Error> {
        let room_id = self.encode_key(keys::SEND_QUEUE, room_id);

        // See comment in `save_send_queue_request`.
        let transaction_id = transaction_id.to_string();

        let num_deleted = self
            .write()
            .await?
            .with_transaction(move |txn| {
                txn.prepare_cached(
                    "DELETE FROM send_queue_events WHERE room_id = ? AND transaction_id = ?",
                )?
                .execute((room_id, &transaction_id))
            })
            .await?;

        Ok(num_deleted > 0)
    }

    async fn load_send_queue_requests(
        &self,
        room_id: &RoomId,
    ) -> Result<Vec<QueuedRequest>, Self::Error> {
        let room_id = self.encode_key(keys::SEND_QUEUE, room_id);

        // Note: ROWID is always present and is an auto-incremented integer counter. We
        // want to maintain the insertion order, so we can sort using it.
        // Note 2: transaction_id is not encoded, see why in `save_send_queue_request`.
        let res: Vec<(String, Vec<u8>, Option<Vec<u8>>, usize, Option<u64>)> = self
            .read()
            .await?
            .prepare(
                "SELECT transaction_id, content, wedge_reason, priority, created_at FROM send_queue_events WHERE room_id = ? ORDER BY priority DESC, ROWID",
                |mut stmt| {
                    stmt.query((room_id,))?
                        .mapped(|row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))
                        .collect()
                },
            )
            .await?;

        let mut requests = Vec::with_capacity(res.len());

        for entry in res {
            let created_at = entry
                .4
                .and_then(UInt::new)
                .map_or_else(MilliSecondsSinceUnixEpoch::now, MilliSecondsSinceUnixEpoch);

            requests.push(QueuedRequest {
                transaction_id: entry.0.into(),
                kind: self.deserialize_json(&entry.1)?,
                error: entry.2.map(|v| self.deserialize_value(&v)).transpose()?,
                priority: entry.3,
                created_at,
            });
        }

        Ok(requests)
    }

    async fn update_send_queue_request_status(
        &self,
        room_id: &RoomId,
        transaction_id: &TransactionId,
        error: Option<QueueWedgeError>,
    ) -> Result<(), Self::Error> {
        let room_id = self.encode_key(keys::SEND_QUEUE, room_id);

        // See comment in `save_send_queue_request`.
        let transaction_id = transaction_id.to_string();

        // Serialize the error to json bytes (encrypted if option is enabled) if set.
        let error_value = error.map(|e| self.serialize_value(&e)).transpose()?;

        self.write()
            .await?
            .with_transaction(move |txn| {
                txn.prepare_cached("UPDATE send_queue_events SET wedge_reason = ? WHERE room_id = ? AND transaction_id = ?")?.execute((error_value, room_id, transaction_id))?;
                Ok(())
            })
            .await
    }

    async fn load_rooms_with_unsent_requests(&self) -> Result<Vec<OwnedRoomId>, Self::Error> {
        // If the values were not encrypted, we could use `SELECT DISTINCT` here, but we
        // have to manually do the deduplication: indeed, for all X, encrypt(X)
        // != encrypted(X), since we use a nonce in the encryption process.

        let res: Vec<Vec<u8>> = self
            .read()
            .await?
            .prepare("SELECT room_id_val FROM send_queue_events", |mut stmt| {
                stmt.query(())?.mapped(|row| row.get(0)).collect()
            })
            .await?;

        // So we collect the results into a `BTreeSet` to perform the deduplication, and
        // then rejigger that into a vector.
        Ok(res
            .into_iter()
            .map(|entry| self.deserialize_value(&entry))
            .collect::<Result<BTreeSet<OwnedRoomId>, _>>()?
            .into_iter()
            .collect())
    }

    async fn save_dependent_queued_request(
        &self,
        room_id: &RoomId,
        parent_txn_id: &TransactionId,
        own_txn_id: ChildTransactionId,
        created_at: MilliSecondsSinceUnixEpoch,
        content: DependentQueuedRequestKind,
    ) -> Result<()> {
        let room_id = self.encode_key(keys::DEPENDENTS_SEND_QUEUE, room_id);
        let content = self.serialize_json(&content)?;

        // See comment in `save_send_queue_request`.
        let parent_txn_id = parent_txn_id.to_string();
        let own_txn_id = own_txn_id.to_string();

        let created_at_ts: u64 = created_at.0.into();
        self.write()
            .await?
            .with_transaction(move |txn| {
                txn.prepare_cached(
                    r#"INSERT INTO dependent_send_queue_events
                         (room_id, parent_transaction_id, own_transaction_id, content, created_at)
                       VALUES (?, ?, ?, ?, ?)"#,
                )?
                .execute((
                    room_id,
                    parent_txn_id,
                    own_txn_id,
                    content,
                    created_at_ts,
                ))?;
                Ok(())
            })
            .await
    }

    async fn update_dependent_queued_request(
        &self,
        room_id: &RoomId,
        own_transaction_id: &ChildTransactionId,
        new_content: DependentQueuedRequestKind,
    ) -> Result<bool> {
        let room_id = self.encode_key(keys::DEPENDENTS_SEND_QUEUE, room_id);
        let content = self.serialize_json(&new_content)?;

        // See comment in `save_send_queue_request`.
        let own_txn_id = own_transaction_id.to_string();

        let num_updated = self
            .write()
            .await?
            .with_transaction(move |txn| {
                txn.prepare_cached(
                    r#"UPDATE dependent_send_queue_events
                       SET content = ?
                       WHERE own_transaction_id = ?
                       AND room_id = ?"#,
                )?
                .execute((content, own_txn_id, room_id))
            })
            .await?;

        if num_updated > 1 {
            return Err(Error::InconsistentUpdate);
        }

        Ok(num_updated == 1)
    }

    async fn mark_dependent_queued_requests_as_ready(
        &self,
        room_id: &RoomId,
        parent_txn_id: &TransactionId,
        parent_key: SentRequestKey,
    ) -> Result<usize> {
        let room_id = self.encode_key(keys::DEPENDENTS_SEND_QUEUE, room_id);
        let parent_key = self.serialize_json(&parent_key)?;

        // See comment in `save_send_queue_request`.
        let parent_txn_id = parent_txn_id.to_string();

        self.write()
            .await?
            .with_transaction(move |txn| {
                Ok(txn.prepare_cached(
                    "UPDATE dependent_send_queue_events SET parent_key = ? WHERE parent_transaction_id = ? and room_id = ?",
                )?
                .execute((parent_key, parent_txn_id, room_id))?)
            })
            .await
    }

    async fn remove_dependent_queued_request(
        &self,
        room_id: &RoomId,
        txn_id: &ChildTransactionId,
    ) -> Result<bool> {
        let room_id = self.encode_key(keys::DEPENDENTS_SEND_QUEUE, room_id);

        // See comment in `save_send_queue_request`.
        let txn_id = txn_id.to_string();

        let num_deleted = self
            .write()
            .await?
            .with_transaction(move |txn| {
                txn.prepare_cached(
                    "DELETE FROM dependent_send_queue_events WHERE own_transaction_id = ? AND room_id = ?",
                )?
                .execute((txn_id, room_id))
            })
            .await?;

        Ok(num_deleted > 0)
    }

    async fn load_dependent_queued_requests(
        &self,
        room_id: &RoomId,
    ) -> Result<Vec<DependentQueuedRequest>> {
        let room_id = self.encode_key(keys::DEPENDENTS_SEND_QUEUE, room_id);

        // Note: transaction_id is not encoded, see why in `save_send_queue_request`.
        let res: Vec<(String, String, Option<Vec<u8>>, Vec<u8>, Option<u64>)> = self
            .read()
            .await?
            .prepare(
                "SELECT own_transaction_id, parent_transaction_id, parent_key, content, created_at FROM dependent_send_queue_events WHERE room_id = ? ORDER BY ROWID",
                |mut stmt| {
                    stmt.query((room_id,))?
                        .mapped(|row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)))
                        .collect()
                },
            )
            .await?;

        let mut dependent_events = Vec::with_capacity(res.len());

        for entry in res {
            let created_at = entry
                .4
                .and_then(UInt::new)
                .map_or_else(MilliSecondsSinceUnixEpoch::now, MilliSecondsSinceUnixEpoch);

            dependent_events.push(DependentQueuedRequest {
                own_transaction_id: entry.0.into(),
                parent_transaction_id: entry.1.into(),
                parent_key: entry.2.map(|json| self.deserialize_json(&json)).transpose()?,
                kind: self.deserialize_json(&entry.3)?,
                created_at,
            });
        }

        Ok(dependent_events)
    }

    async fn upsert_thread_subscriptions(
        &self,
        updates: Vec<(&RoomId, &EventId, StoredThreadSubscription)>,
    ) -> Result<(), Self::Error> {
        let values: Vec<_> = updates
            .into_iter()
            .map(|(room_id, thread_id, subscription)| {
                (
                    self.encode_key(keys::THREAD_SUBSCRIPTIONS, room_id),
                    self.encode_key(keys::THREAD_SUBSCRIPTIONS, thread_id),
                    subscription.status.as_str(),
                    subscription.bump_stamp,
                )
            })
            .collect();

        self.write()
            .await?
            .with_transaction(move |txn| {
                let mut txn = txn.prepare_cached(
                    "INSERT INTO thread_subscriptions (room_id, event_id, status, bump_stamp)
                    VALUES (?, ?, ?, ?)
                    ON CONFLICT (room_id, event_id) DO UPDATE
                    SET
                        status =
                            CASE
                                WHEN thread_subscriptions.bump_stamp IS NULL THEN EXCLUDED.status
                                WHEN EXCLUDED.bump_stamp IS NULL THEN EXCLUDED.status
                                WHEN thread_subscriptions.bump_stamp < EXCLUDED.bump_stamp THEN EXCLUDED.status
                                ELSE thread_subscriptions.status
                            END,
                        bump_stamp =
                            CASE
                                WHEN thread_subscriptions.bump_stamp IS NULL THEN EXCLUDED.bump_stamp
                                WHEN EXCLUDED.bump_stamp IS NULL THEN thread_subscriptions.bump_stamp
                                WHEN thread_subscriptions.bump_stamp < EXCLUDED.bump_stamp THEN EXCLUDED.bump_stamp
                                ELSE thread_subscriptions.bump_stamp
                            END",
                )?;

                for value in values {
                    txn.execute(value)?;
                }

                Result::<_, Error>::Ok(())
            })
            .await?;

        Ok(())
    }

    async fn load_thread_subscription(
        &self,
        room_id: &RoomId,
        thread_id: &EventId,
    ) -> Result<Option<StoredThreadSubscription>, Self::Error> {
        let room_id = self.encode_key(keys::THREAD_SUBSCRIPTIONS, room_id);
        let thread_id = self.encode_key(keys::THREAD_SUBSCRIPTIONS, thread_id);

        Ok(self
            .read()
            .await?
            .query_one(
                "SELECT status, bump_stamp FROM thread_subscriptions WHERE room_id = ? AND event_id = ?",
                (room_id, thread_id),
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<u64>>(1)?))
            )
            .await
            .optional()?
            .map(|(status, bump_stamp)| -> Result<_, Self::Error> {
                let status = ThreadSubscriptionStatus::from_str(&status).map_err(|_| {
                    Error::InvalidData { details: format!("Invalid thread status: {status}") }
                })?;
                Ok(StoredThreadSubscription { status, bump_stamp })
            })
            .transpose()?)
    }

    async fn remove_thread_subscription(
        &self,
        room_id: &RoomId,
        thread_id: &EventId,
    ) -> Result<(), Self::Error> {
        let room_id = self.encode_key(keys::THREAD_SUBSCRIPTIONS, room_id);
        let thread_id = self.encode_key(keys::THREAD_SUBSCRIPTIONS, thread_id);

        self.write()
            .await?
            .execute(
                "DELETE FROM thread_subscriptions WHERE room_id = ? AND event_id = ?",
                (room_id, thread_id),
            )
            .await?;

        Ok(())
    }

    async fn get_global_profile(
        &self,
        user_id: &UserId,
    ) -> Result<Option<UserProfile>, Self::Error> {
        self.read()
            .await?
            .get_global_profiles(vec![self.encode_key(keys::GLOBAL_PROFILES, user_id)])
            .await?
            .into_iter()
            .next()
            .map(|(_, data)| self.deserialize_json(&data))
            .transpose()
    }

    async fn get_global_profiles<'a>(
        &self,
        user_ids: &'a [OwnedUserId],
    ) -> Result<BTreeMap<&'a UserId, UserProfile>, Self::Error> {
        if user_ids.is_empty() {
            return Ok(BTreeMap::new());
        }

        let mut user_ids_map = user_ids
            .iter()
            .map(|u| (self.encode_key(keys::GLOBAL_PROFILES, u), u.as_ref()))
            .collect::<BTreeMap<_, _>>();
        let user_ids = user_ids_map.keys().cloned().collect();

        self.read()
            .await?
            .get_global_profiles(user_ids)
            .await?
            .into_iter()
            .map(|(user_id, data)| {
                Ok((
                    user_ids_map
                        .remove(user_id.as_slice())
                        .expect("returned user IDs were requested"),
                    self.deserialize_json(&data)?,
                ))
            })
            .collect()
    }

    async fn optimize(&self) -> Result<(), Self::Error> {
        Ok(self.vacuum().await?)
    }

    async fn get_size(&self) -> Result<Option<usize>, Self::Error> {
        self.get_db_size().await
    }

    async fn close(&self) -> Result<(), Self::Error> {
        connection::close_connections(&self.connections, "State store").await;
        Ok(())
    }

    async fn reopen(&self) -> Result<(), Self::Error> {
        connection::reopen_connections(
            &self.connections,
            self.db_path.clone(),
            self.pool_config,
            self.runtime_config,
        )
        .await?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReceiptData {
    receipt: Receipt,
    event_id: OwnedEventId,
    user_id: OwnedUserId,
}

#[cfg(test)]
mod tests {
    use std::sync::{
        LazyLock,
        atomic::{AtomicU32, Ordering::SeqCst},
    };

    use matrix_sdk_base::{StateStore, StoreError, statestore_integration_tests};
    use tempfile::{TempDir, tempdir};

    use super::SqliteStateStore;

    static TMP_DIR: LazyLock<TempDir> = LazyLock::new(|| tempdir().unwrap());
    static NUM: AtomicU32 = AtomicU32::new(0);

    async fn get_store() -> Result<impl StateStore, StoreError> {
        let name = NUM.fetch_add(1, SeqCst).to_string();
        let tmpdir_path = TMP_DIR.path().join(name);

        tracing::info!("using store @ {}", tmpdir_path.to_str().unwrap());

        Ok(SqliteStateStore::open(tmpdir_path.to_str().unwrap(), None).await.unwrap())
    }

    statestore_integration_tests!();
}

#[cfg(test)]
mod encrypted_tests {
    use std::{
        path::PathBuf,
        sync::{
            LazyLock,
            atomic::{AtomicU32, Ordering::SeqCst},
        },
    };

    use base64::Engine as _;
    use matrix_sdk_base::{StateStore, StoreError, statestore_integration_tests};
    use matrix_sdk_test::async_test;
    use tempfile::{TempDir, tempdir};

    use super::SqliteStateStore;
    use crate::{Base64Variant, SqliteStoreConfig, utils::SqliteAsyncConnExt};

    static TMP_DIR: LazyLock<TempDir> = LazyLock::new(|| tempdir().unwrap());
    static NUM: AtomicU32 = AtomicU32::new(0);

    fn new_state_store_workspace() -> PathBuf {
        let name = NUM.fetch_add(1, SeqCst).to_string();
        TMP_DIR.path().join(name)
    }

    async fn get_store() -> Result<impl StateStore, StoreError> {
        let tmpdir_path = new_state_store_workspace();

        tracing::info!("using store @ {}", tmpdir_path.to_str().unwrap());

        Ok(SqliteStateStore::open(tmpdir_path.to_str().unwrap(), Some("default_test_password"))
            .await
            .unwrap())
    }

    /// The two passphrase methods are interchangeable in both directions.
    #[async_test]
    async fn test_high_entropy_passphrase_migrates_a_passphrase_store() {
        const KEY: &[u8; 32] = b"a randomly generated passphrase ";
        let tmpdir_path = new_state_store_workspace();

        let passphrase = base64::prelude::BASE64_STANDARD.encode(KEY);

        let config = SqliteStoreConfig::new(&tmpdir_path).passphrase(Some(&passphrase));
        drop(SqliteStateStore::open_with_config(&config).await.unwrap());

        // Migrates and caches the copy...
        let config = SqliteStoreConfig::new(&tmpdir_path)
            .high_entropy_passphrase(Some(KEY), Base64Variant::Padded);
        drop(SqliteStateStore::open_with_config(&config).await.unwrap());

        // ...which the next open uses.
        drop(SqliteStateStore::open_with_config(&config).await.unwrap());

        // The `cipher` entry was replaced, so the old passphrase can't work anymore.
        let config = SqliteStoreConfig::new(&tmpdir_path).passphrase(Some(&passphrase));
        drop(
            SqliteStateStore::open_with_config(&config)
                .await
                .expect_err("The old passphrase-only method shouldn't work anymore"),
        );

        // The `cipher` entry was replaced, so now only high entropy or key work.
        let config = SqliteStoreConfig::new(&tmpdir_path)
            .high_entropy_passphrase(Some(KEY), Base64Variant::Padded);
        drop(
            SqliteStateStore::open_with_config(&config)
                .await
                .expect("The high-entropy method should continue to work"),
        );

        let config = SqliteStoreConfig::new(&tmpdir_path).key(Some(KEY));
        drop(
            SqliteStateStore::open_with_config(&config).await.expect("The key should work as well"),
        );

        let config = SqliteStoreConfig::new(&tmpdir_path).high_entropy_passphrase(
            Some(b"wrong passphrase can't work 1234"),
            Base64Variant::Padded,
        );
        assert!(SqliteStateStore::open_with_config(&config).await.is_err());
        let config = SqliteStoreConfig::new(&tmpdir_path).passphrase(Some("wrong"));
        assert!(SqliteStateStore::open_with_config(&config).await.is_err());
    }

    #[async_test]
    async fn test_pool_size() {
        let tmpdir_path = new_state_store_workspace();
        let store_open_config = SqliteStoreConfig::new(tmpdir_path).pool_max_size(42);

        let store = SqliteStateStore::open_with_config(&store_open_config).await.unwrap();

        let guard = store.connections.lock().await;
        assert_eq!(guard.as_ref().unwrap().pool.status().max_size, 42);
    }

    #[async_test]
    async fn test_cache_size() {
        let tmpdir_path = new_state_store_workspace();
        let store_open_config = SqliteStoreConfig::new(tmpdir_path).cache_size(1500);

        let store = SqliteStateStore::open_with_config(&store_open_config).await.unwrap();

        let conn = store.read().await.unwrap();
        let cache_size =
            conn.query_row("PRAGMA cache_size", (), |row| row.get::<_, i32>(0)).await.unwrap();

        // The value passed to `SqliteStoreConfig` is in bytes. Check it is
        // converted to kibibytes. Also, it must be a negative value because it
        // _is_ the size in kibibytes, not in page size.
        assert_eq!(cache_size, -(1500 / 1024));
    }

    #[async_test]
    async fn test_journal_size_limit() {
        let tmpdir_path = new_state_store_workspace();
        let store_open_config = SqliteStoreConfig::new(tmpdir_path).journal_size_limit(1500);

        let store = SqliteStateStore::open_with_config(&store_open_config).await.unwrap();

        let conn = store.read().await.unwrap();
        let journal_size_limit = conn
            .query_row("PRAGMA journal_size_limit", (), |row| row.get::<_, u32>(0))
            .await
            .unwrap();

        // The value passed to `SqliteStoreConfig` is in bytes. It stays in
        // bytes in SQLite.
        assert_eq!(journal_size_limit, 1500);
    }

    statestore_integration_tests!();
}

#[cfg(test)]
mod migration_tests {
    use std::{
        path::{Path, PathBuf},
        sync::{
            Arc, LazyLock,
            atomic::{AtomicU32, Ordering::SeqCst},
        },
    };

    use as_variant::as_variant;
    use matrix_sdk_base::{
        RoomState, StateStore,
        media::{MediaFormat, MediaRequestParameters},
        store::{
            ChildTransactionId, DependentQueuedRequestKind, RoomLoadSettings,
            SerializableEventContent,
        },
        sync::UnreadNotificationsCount,
    };
    use matrix_sdk_test::async_test;
    use ruma::{
        EventId, MilliSecondsSinceUnixEpoch, OwnedTransactionId, RoomId, TransactionId, UserId,
        events::{
            StateEventType,
            room::{MediaSource, create::RoomCreateEventContent, message::RoomMessageEventContent},
        },
        room_id, server_name, user_id,
    };
    use rusqlite::Transaction;
    use serde::{Deserialize, Serialize};
    use serde_json::json;
    use tempfile::{TempDir, tempdir};
    use tokio::{fs, sync::Mutex};
    use zeroize::Zeroizing;

    use super::{DATABASE_NAME, SqliteStateStore, init, keys};
    use crate::{
        OpenStoreError, Secret, SqliteStoreConfig, connection,
        error::{Error, Result},
        utils::{EncryptableStore as _, SqliteAsyncConnExt, SqliteKeyValueStoreAsyncConnExt},
    };

    static TMP_DIR: LazyLock<TempDir> = LazyLock::new(|| tempdir().unwrap());
    static NUM: AtomicU32 = AtomicU32::new(0);
    const SECRET: &str = "secret";

    fn new_path() -> PathBuf {
        let name = NUM.fetch_add(1, SeqCst).to_string();
        TMP_DIR.path().join(name)
    }

    async fn create_fake_db(path: &Path, version: u8) -> Result<SqliteStateStore> {
        let config = SqliteStoreConfig::new(path);

        fs::create_dir_all(&config.path).await.map_err(OpenStoreError::CreateDir).unwrap();

        let pool = config.build_pool_of_connections(DATABASE_NAME).unwrap();
        let db_path = pool.manager().database_path.clone();
        let conn = pool.get().await?;

        init(&conn).await?;

        let store_cipher = Some(Arc::new(
            conn.get_or_create_store_cipher(Secret::PassPhrase(Zeroizing::new(SECRET.to_owned())))
                .await
                .unwrap(),
        ));
        let this = SqliteStateStore {
            store_cipher,
            connections: Arc::new(Mutex::new(Some(connection::SqliteConnections {
                pool,
                write_connection: Arc::new(Mutex::new(conn)),
            }))),
            db_path,
            pool_config: deadpool::managed::PoolConfig::default(),
            runtime_config: crate::RuntimeConfig::default(),
        };
        this.run_migrations(1, Some(version)).await?;

        Ok(this)
    }

    fn room_info_v1_json(
        room_id: &RoomId,
        state: RoomState,
        name: Option<&str>,
        creator: Option<&UserId>,
    ) -> serde_json::Value {
        // Test with name set or not.
        let name_content = match name {
            Some(name) => json!({ "name": name }),
            None => json!({ "name": null }),
        };
        // Test with creator set or not.
        let create_content = match creator {
            Some(creator) => RoomCreateEventContent::new_v1(creator.to_owned()),
            None => RoomCreateEventContent::new_v11(),
        };

        json!({
            "room_id": room_id,
            "room_type": state,
            "notification_counts": UnreadNotificationsCount::default(),
            "summary": {
                "heroes": [],
                "joined_member_count": 0,
                "invited_member_count": 0,
            },
            "members_synced": false,
            "base_info": {
                "dm_targets": [],
                "max_power_level": 100,
                "name": {
                    "Original": {
                        "content": name_content,
                    },
                },
                "create": {
                    "Original": {
                        "content": create_content,
                    }
                }
            },
        })
    }

    #[async_test]
    pub async fn test_migrating_v1_to_v2() {
        let path = new_path();
        // Create and populate db.
        {
            let db = create_fake_db(&path, 1).await.unwrap();
            let conn = db.read().await.unwrap();

            let this = db.clone();
            conn.with_transaction(move |txn| {
                for i in 0..5 {
                    let room_id = RoomId::parse(format!("!room_{i}:localhost")).unwrap();
                    let (state, stripped) =
                        if i < 3 { (RoomState::Joined, false) } else { (RoomState::Invited, true) };
                    let info = room_info_v1_json(&room_id, state, None, None);

                    let room_id = this.encode_key(keys::ROOM_INFO, room_id);
                    let data = this.serialize_json(&info)?;

                    txn.prepare_cached(
                        "INSERT INTO room_info (room_id, stripped, data)
                         VALUES (?, ?, ?)",
                    )?
                    .execute((room_id, stripped, data))?;
                }

                Result::<_, Error>::Ok(())
            })
            .await
            .unwrap();
        }

        // This transparently migrates to the latest version.
        let store = SqliteStateStore::open(path, Some(SECRET)).await.unwrap();

        // Check all room infos are there.
        assert_eq!(store.get_room_infos(&RoomLoadSettings::default()).await.unwrap().len(), 5);
    }

    // Add a room in version 2 format of the state store.
    fn add_room_v2(
        this: &SqliteStateStore,
        txn: &Transaction<'_>,
        room_id: &RoomId,
        name: Option<&str>,
        create_creator: Option<&UserId>,
        create_sender: Option<&UserId>,
    ) -> Result<(), Error> {
        let room_info_json = room_info_v1_json(room_id, RoomState::Joined, name, create_creator);

        let encoded_room_id = this.encode_key(keys::ROOM_INFO, room_id);
        let encoded_state =
            this.encode_key(keys::ROOM_INFO, serde_json::to_string(&RoomState::Joined)?);
        let data = this.serialize_json(&room_info_json)?;

        txn.prepare_cached(
            "INSERT INTO room_info (room_id, state, data)
             VALUES (?, ?, ?)",
        )?
        .execute((encoded_room_id, encoded_state, data))?;

        // Test with or without `m.room.create` event in the room state.
        let Some(create_sender) = create_sender else {
            return Ok(());
        };

        let create_content = match create_creator {
            Some(creator) => RoomCreateEventContent::new_v1(creator.to_owned()),
            None => RoomCreateEventContent::new_v11(),
        };

        let event_id = EventId::new_v1(server_name!("dummy.local"));
        let create_event = json!({
            "content": create_content,
            "event_id": event_id,
            "sender": create_sender.to_owned(),
            "origin_server_ts": MilliSecondsSinceUnixEpoch::now(),
            "state_key": "",
            "type": "m.room.create",
            "unsigned": {},
        });

        let encoded_room_id = this.encode_key(keys::STATE_EVENT, room_id);
        let encoded_event_type =
            this.encode_key(keys::STATE_EVENT, StateEventType::RoomCreate.to_string());
        let encoded_state_key = this.encode_key(keys::STATE_EVENT, "");
        let stripped = false;
        let encoded_event_id = this.encode_key(keys::STATE_EVENT, event_id);
        let data = this.serialize_json(&create_event)?;

        txn.prepare_cached(
            "INSERT
             INTO state_event (room_id, event_type, state_key, stripped, event_id, data)
             VALUES (?, ?, ?, ?, ?, ?)",
        )?
        .execute((
            encoded_room_id,
            encoded_event_type,
            encoded_state_key,
            stripped,
            encoded_event_id,
            data,
        ))?;

        Ok(())
    }

    #[async_test]
    pub async fn test_migrating_v2_to_v3() {
        let path = new_path();

        // Room A: with name, creator and sender.
        let room_a_id = room_id!("!room_a:dummy.local");
        let room_a_name = "Room A";
        let room_a_creator = user_id!("@creator:dummy.local");
        // Use a different sender to check that sender is used over creator in
        // migration.
        let room_a_create_sender = user_id!("@sender:dummy.local");

        // Room B: without name, creator and sender.
        let room_b_id = room_id!("!room_b:dummy.local");

        // Room C: only with sender.
        let room_c_id = room_id!("!room_c:dummy.local");
        let room_c_create_sender = user_id!("@creator:dummy.local");

        // Create and populate db.
        {
            let db = create_fake_db(&path, 2).await.unwrap();
            let conn = db.read().await.unwrap();

            let this = db.clone();
            conn.with_transaction(move |txn| {
                add_room_v2(
                    &this,
                    txn,
                    room_a_id,
                    Some(room_a_name),
                    Some(room_a_creator),
                    Some(room_a_create_sender),
                )?;
                add_room_v2(&this, txn, room_b_id, None, None, None)?;
                add_room_v2(&this, txn, room_c_id, None, None, Some(room_c_create_sender))?;

                Result::<_, Error>::Ok(())
            })
            .await
            .unwrap();
        }

        // This transparently migrates to the latest version.
        let store = SqliteStateStore::open(path, Some(SECRET)).await.unwrap();

        // Check all room infos are there.
        let room_infos = store.get_room_infos(&RoomLoadSettings::default()).await.unwrap();
        assert_eq!(room_infos.len(), 3);

        let room_a = room_infos.iter().find(|r| r.room_id() == room_a_id).unwrap();
        assert_eq!(room_a.name(), Some(room_a_name));
        assert_eq!(room_a.creators(), Some(vec![room_a_create_sender.to_owned()]));

        let room_b = room_infos.iter().find(|r| r.room_id() == room_b_id).unwrap();
        assert_eq!(room_b.name(), None);
        assert_eq!(room_b.creators(), None);

        let room_c = room_infos.iter().find(|r| r.room_id() == room_c_id).unwrap();
        assert_eq!(room_c.name(), None);
        assert_eq!(room_c.creators(), Some(vec![room_c_create_sender.to_owned()]));
    }

    #[async_test]
    pub async fn test_migrating_v7_to_v9() {
        let path = new_path();

        let room_id = room_id!("!room_a:dummy.local");
        let wedged_event_transaction_id = TransactionId::new();
        let local_event_transaction_id = TransactionId::new();

        // Create and populate db.
        {
            let db = create_fake_db(&path, 7).await.unwrap();
            let conn = db.read().await.unwrap();

            let wedge_tx = wedged_event_transaction_id.clone();
            let local_tx = local_event_transaction_id.clone();

            conn.with_transaction(move |txn| {
                add_dependent_send_queue_event_v7(
                    &db,
                    txn,
                    room_id,
                    &local_tx,
                    ChildTransactionId::new(),
                    DependentQueuedRequestKind::RedactEvent,
                )?;
                add_send_queue_event_v7(&db, txn, &wedge_tx, room_id, true)?;
                add_send_queue_event_v7(&db, txn, &local_tx, room_id, false)?;
                Result::<_, Error>::Ok(())
            })
            .await
            .unwrap();
        }

        // This transparently migrates to the latest version, which clears up all
        // requests and dependent requests.
        let store = SqliteStateStore::open(path, Some(SECRET)).await.unwrap();

        let requests = store.load_send_queue_requests(room_id).await.unwrap();
        assert!(requests.is_empty());

        let dependent_requests = store.load_dependent_queued_requests(room_id).await.unwrap();
        assert!(dependent_requests.is_empty());
    }

    fn add_send_queue_event_v7(
        this: &SqliteStateStore,
        txn: &Transaction<'_>,
        transaction_id: &TransactionId,
        room_id: &RoomId,
        is_wedged: bool,
    ) -> Result<(), Error> {
        let content =
            SerializableEventContent::new(&RoomMessageEventContent::text_plain("Hello").into())?;

        let room_id_key = this.encode_key(keys::SEND_QUEUE, room_id);
        let room_id_value = this.serialize_value(&room_id.to_owned())?;

        let content = this.serialize_json(&content)?;

        txn.prepare_cached("INSERT INTO send_queue_events (room_id, room_id_val, transaction_id, content, wedged) VALUES (?, ?, ?, ?, ?)")?
            .execute((room_id_key, room_id_value, transaction_id.to_string(), content, is_wedged))?;

        Ok(())
    }

    fn add_dependent_send_queue_event_v7(
        this: &SqliteStateStore,
        txn: &Transaction<'_>,
        room_id: &RoomId,
        parent_txn_id: &TransactionId,
        own_txn_id: ChildTransactionId,
        content: DependentQueuedRequestKind,
    ) -> Result<(), Error> {
        let room_id_value = this.serialize_value(&room_id.to_owned())?;

        let parent_txn_id = parent_txn_id.to_string();
        let own_txn_id = own_txn_id.to_string();
        let content = this.serialize_json(&content)?;

        txn.prepare_cached(
            "INSERT INTO dependent_send_queue_events
                         (room_id, parent_transaction_id, own_transaction_id, content)
                       VALUES (?, ?, ?, ?)",
        )?
        .execute((room_id_value, parent_txn_id, own_txn_id, content))?;

        Ok(())
    }

    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub enum LegacyDependentQueuedRequestKind {
        UploadFileWithThumbnail {
            content_type: String,
            cache_key: MediaRequestParameters,
            related_to: OwnedTransactionId,
        },
    }

    #[async_test]
    pub async fn test_dependent_queued_request_variant_renaming() {
        let path = new_path();
        let db = create_fake_db(&path, 7).await.unwrap();

        let cache_key = MediaRequestParameters {
            format: MediaFormat::File,
            source: MediaSource::Plain("https://server.local/foobar".into()),
        };
        let related_to = TransactionId::new();
        let request = LegacyDependentQueuedRequestKind::UploadFileWithThumbnail {
            content_type: "image/png".to_owned(),
            cache_key,
            related_to: related_to.clone(),
        };

        let data = db
            .serialize_json(&request)
            .expect("should be able to serialize legacy dependent request");
        let deserialized: DependentQueuedRequestKind = db.deserialize_json(&data).expect(
            "should be able to deserialize dependent request from legacy dependent request",
        );

        as_variant!(deserialized, DependentQueuedRequestKind::UploadFileOrThumbnail { related_to: de_related_to, .. } => {
            assert_eq!(de_related_to, related_to);
        });
    }
}

#[cfg(test)]
mod close_reopen_tests {
    use std::sync::{
        LazyLock,
        atomic::{AtomicU32, Ordering::SeqCst},
    };

    use matrix_sdk_base::StateStore;
    use matrix_sdk_test::async_test;
    use tempfile::{TempDir, tempdir};

    use super::SqliteStateStore;

    static TMP_DIR: LazyLock<TempDir> = LazyLock::new(|| tempdir().unwrap());
    static NUM: AtomicU32 = AtomicU32::new(0);

    async fn new_store() -> SqliteStateStore {
        let name = NUM.fetch_add(1, SeqCst).to_string();
        let tmpdir_path = TMP_DIR.path().join(name);
        SqliteStateStore::open(tmpdir_path.to_str().unwrap(), None).await.unwrap()
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

        // Connections should be None after close.
        {
            let guard = store.connections.lock().await;
            assert!(guard.is_none());
        }

        store.reopen().await.unwrap();

        // Connections should be Some after reopen.
        {
            let guard = store.connections.lock().await;
            assert!(guard.is_some(), "connections should be Some after reopen");
        }
    }

    #[async_test]
    async fn test_close_is_idempotent() {
        let store = new_store().await;

        // First close.
        store.close().await.unwrap();
        // Second close should also succeed (no-op).
        store.close().await.unwrap();

        let guard = store.connections.lock().await;
        assert!(guard.is_none());
    }

    #[async_test]
    async fn test_reopen_is_idempotent() {
        let store = new_store().await;

        // Reopen on an active store should be a no-op.
        store.reopen().await.unwrap();

        // Connections should still be Some.
        let guard = store.connections.lock().await;
        assert!(guard.is_some());
    }

    #[async_test]
    async fn test_read_fails_when_closed() {
        let store = new_store().await;
        store.close().await.unwrap();

        let err = store.get_custom_value(b"some_key").await;
        assert!(err.is_err(), "read should fail when closed");

        let err_msg = err.unwrap_err().to_string();
        assert!(err_msg.contains("closed"), "error should mention 'closed', got: {err_msg}");
    }

    #[async_test]
    async fn test_write_fails_when_closed() {
        let store = new_store().await;
        store.close().await.unwrap();

        let err = store.set_custom_value(b"key", b"value".to_vec()).await;
        assert!(err.is_err(), "write should fail when closed");

        let err_msg = err.unwrap_err().to_string();
        assert!(err_msg.contains("closed"), "error should mention 'closed', got: {err_msg}");
    }

    #[async_test]
    async fn test_data_persists_across_close_reopen() {
        let store = new_store().await;

        // Write some data.
        store.set_custom_value(b"test_key", b"test_value".to_vec()).await.unwrap();

        // Verify it's there.
        let value = store.get_custom_value(b"test_key").await.unwrap();
        assert_eq!(value.as_deref(), Some(b"test_value".as_slice()));

        // Close and reopen.
        store.close().await.unwrap();
        store.reopen().await.unwrap();

        // Data should still be there after reopen.
        let value = store.get_custom_value(b"test_key").await.unwrap();
        assert_eq!(
            value.as_deref(),
            Some(b"test_value".as_slice()),
            "data should persist across close/reopen"
        );
    }

    #[async_test]
    async fn test_multiple_close_reopen_cycles() {
        let store = new_store().await;

        for i in 0..3 {
            let key = format!("key_{i}");
            let value = format!("value_{i}");

            store.set_custom_value(key.as_bytes(), value.as_bytes().to_vec()).await.unwrap();

            store.close().await.unwrap();
            store.reopen().await.unwrap();

            // Verify all previously written data is still accessible.
            for j in 0..=i {
                let k = format!("key_{j}");
                let v = format!("value_{j}");
                let retrieved = store.get_custom_value(k.as_bytes()).await.unwrap();
                assert_eq!(
                    retrieved.as_deref(),
                    Some(v.as_bytes()),
                    "data for key_{j} should persist after cycle {i}"
                );
            }
        }
    }

    #[async_test]
    async fn test_pool_is_fully_drained_after_close() {
        let store = new_store().await;

        // Do a few reads to exercise the pool.
        let _ = store.get_custom_value(b"key1").await;
        let _ = store.get_custom_value(b"key2").await;

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

        // Write should work immediately.
        store.set_custom_value(b"after_reopen", b"works".to_vec()).await.unwrap();

        // Read should work immediately.
        let value = store.get_custom_value(b"after_reopen").await.unwrap();
        assert_eq!(value.as_deref(), Some(b"works".as_slice()));
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
