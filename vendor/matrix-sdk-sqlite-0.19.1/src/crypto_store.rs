// Copyright 2022, 2026 The Matrix.org Foundation C.I.C.
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

use std::{
    collections::HashMap,
    fmt,
    ops::Deref,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use deadpool::managed::PoolConfig;
use matrix_sdk_base::cross_process_lock::CrossProcessLockGeneration;
use matrix_sdk_crypto::{
    Account, DeviceData, GossipRequest, GossippedSecret, SecretInfo, TrackedUser, UserIdentityData,
    olm::{
        InboundGroupSession, OutboundGroupSession, PickledInboundGroupSession,
        PrivateCrossSigningIdentity, SenderDataType, Session, StaticAccountData,
    },
    store::{
        CryptoStore,
        types::{
            BackupKeys, Changes, DehydratedDeviceKey, PendingChanges, RoomKeyCounts,
            RoomKeyWithheldEntry, RoomPendingKeyBundleDetails, RoomSettings,
            StoredRoomKeyBundleData,
        },
    },
};
use matrix_sdk_store_encryption::StoreCipher;
use ruma::{
    DeviceId, MilliSecondsSinceUnixEpoch, OwnedDeviceId, RoomId, TransactionId, UserId,
    events::secret::request::SecretName,
};
use rusqlite::{OptionalExtension, named_params, params_from_iter};
use tokio::{
    fs,
    sync::{Mutex, OwnedMutexGuard},
};
use tracing::{debug, instrument, warn};
use vodozemac::Curve25519PublicKey;
use zeroize::Zeroizing;

use crate::{
    OpenStoreError, RuntimeConfig, Secret, SqliteStoreConfig,
    connection::{self, Connection as SqliteAsyncConn, Pool as SqlitePool, SqliteConnections},
    error::{Error, Result},
    utils::{
        EncryptableStore, Key, SqliteAsyncConnExt, SqliteKeyValueStoreAsyncConnExt,
        SqliteKeyValueStoreConnExt,
    },
};

/// The database name.
const DATABASE_NAME: &str = "matrix-sdk-crypto.sqlite3";

/// An SQLite-based crypto store.
#[derive(Clone)]
pub struct SqliteCryptoStore {
    store_cipher: Option<Arc<StoreCipher>>,

    /// `Some` when active, `None` when closed.
    /// The outer `Mutex` serialises close/reopen with connection access.
    connections: Arc<Mutex<Option<SqliteConnections>>>,

    /// Retained so we can rebuild the pool on reopen.
    db_path: PathBuf,

    /// Retained so we can rebuild the pool on reopen.
    pool_config: PoolConfig,

    /// Retained so we can re-apply runtime config on reopen.
    runtime_config: RuntimeConfig,

    // DB values cached in memory
    static_account: Arc<RwLock<Option<StaticAccountData>>>,
    save_changes_lock: Arc<Mutex<()>>,
}

#[cfg(not(tarpaulin_include))]
impl fmt::Debug for SqliteCryptoStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqliteCryptoStore").finish_non_exhaustive()
    }
}

impl EncryptableStore for SqliteCryptoStore {
    fn get_cypher(&self) -> Option<&StoreCipher> {
        self.store_cipher.as_deref()
    }
}

impl SqliteCryptoStore {
    /// Create an `SqliteCryptoStore` struct without trying to create the
    /// database or migrate to a newer version.  This is only for use
    /// internally, and for testing.
    ///
    /// # Arguments
    ///
    /// * `secret` - The secret used to encrypt the data.
    ///
    /// * `pool` - A connection pool to use for reading from the store.
    ///
    /// * `conn` - The connection to use for writing to the store.
    pub(crate) async fn create_raw(
        secret: Option<Secret>,
        pool: SqlitePool,
        conn: SqliteAsyncConn,
        pool_config: PoolConfig,
        runtime_config: RuntimeConfig,
    ) -> Result<Self, OpenStoreError> {
        let store_cipher = match secret {
            Some(s) => Some(Arc::new(conn.get_or_create_store_cipher(s).await?)),
            None => None,
        };

        let db_path = pool.manager().database_path.clone();

        Ok(Self {
            store_cipher,
            connections: Arc::new(Mutex::new(Some(SqliteConnections {
                pool,
                write_connection: Arc::new(Mutex::new(conn)),
            }))),
            db_path,
            pool_config,
            runtime_config,
            static_account: Arc::new(RwLock::new(None)),
            save_changes_lock: Default::default(),
        })
    }

    /// Open the SQLite-based crypto store at the given path using the given
    /// passphrase to encrypt private data.
    pub async fn open(
        path: impl AsRef<Path>,
        passphrase: Option<&str>,
    ) -> Result<Self, OpenStoreError> {
        Self::open_with_config(&SqliteStoreConfig::new(path).passphrase(passphrase)).await
    }

    /// Open the SQLite-based crypto store at the given path using the given
    /// key to encrypt private data.
    pub async fn open_with_key(
        path: impl AsRef<Path>,
        key: Option<&[u8]>,
    ) -> Result<Self, OpenStoreError> {
        Self::open_with_config(&SqliteStoreConfig::new(path).key(key)).await
    }

    /// Open the SQLite-based crypto store with the config open config.
    pub async fn open_with_config(config: &SqliteStoreConfig) -> Result<Self, OpenStoreError> {
        fs::create_dir_all(&config.path).await.map_err(OpenStoreError::CreateDir)?;

        let pool = config.build_pool_of_connections(DATABASE_NAME)?;
        let pool_config = config.pool_config();
        let runtime_config = config.runtime_config();

        let this =
            Self::open_with_pool(pool, config.secret.clone(), pool_config, runtime_config).await?;
        this.read().await?.apply_runtime_config(runtime_config).await?;

        Ok(this)
    }

    /// Create an SQLite-based crypto store using the given SQLite database
    /// pool. The given secret will be used to encrypt private data.
    async fn open_with_pool(
        pool: SqlitePool,
        secret: Option<Secret>,
        pool_config: PoolConfig,
        runtime_config: RuntimeConfig,
    ) -> Result<Self, OpenStoreError> {
        let conn = pool.get().await?;

        let version = conn.db_version().await?;
        debug!("Opened sqlite store with version {}", version);

        let version = initialize_store(&conn, version).await?;

        let store = Self::create_raw(secret, pool, conn, pool_config, runtime_config).await?;

        run_migrations(&store, version, None).await?;

        store.write().await?.wal_checkpoint().await;

        Ok(store)
    }

    fn deserialize_and_unpickle_inbound_group_session(
        &self,
        value: Vec<u8>,
        backed_up: bool,
    ) -> Result<InboundGroupSession> {
        let mut pickle: PickledInboundGroupSession = self.deserialize_value(&value)?;

        // The `backed_up` SQL column is the source of truth, because we update it
        // inside `mark_inbound_group_sessions_as_backed_up` and don't update
        // the pickled value inside the `data` column (until now, when we are puling it
        // out of the DB).
        pickle.backed_up = backed_up;

        Ok(InboundGroupSession::from_pickle(pickle)?)
    }

    fn deserialize_key_request(&self, value: &[u8], sent_out: bool) -> Result<GossipRequest> {
        let mut request: GossipRequest = self.deserialize_value(value)?;
        // sent_out SQL column is source of truth, sent_out field in serialized value
        // needed for other stores though
        request.sent_out = sent_out;
        Ok(request)
    }

    fn get_static_account(&self) -> Option<StaticAccountData> {
        self.static_account.read().unwrap().clone()
    }

    /// Acquire a connection for executing read operations.
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
    #[instrument(skip_all)]
    pub(crate) async fn write(&self) -> Result<OwnedMutexGuard<SqliteAsyncConn>> {
        let write_connection = {
            let guard = self.connections.lock().await;
            let conns = guard.as_ref().ok_or(Error::StoreClosed)?;
            conns.write_connection.clone()
        };
        Ok(write_connection.lock_owned().await)
    }
}

const DATABASE_VERSION: u8 = 19;

/// key for the dehydrated device pickle key in the key/value table.
const DEHYDRATED_DEVICE_PICKLE_KEY: &str = "dehydrated_device_pickle_key";

/// Initialize the database to version 1
///
/// This must be done before creating the store cipher, because the store cipher
/// requires the `kv` table.
///
/// # Arguments
///
/// * `conn` - The connection to use.
///
/// * `version` - the current version of the database.
pub(crate) async fn initialize_store(conn: &SqliteAsyncConn, version: u8) -> Result<u8> {
    if version == 0 {
        debug!("Creating database");
    } else if version < DATABASE_VERSION {
        debug!(version, new_version = DATABASE_VERSION, "Upgrading database");
    } else {
        return Ok(version);
    }

    if version < 1 {
        debug!("Creating database");
        // First turn on WAL mode, this can't be done in the transaction, it fails with
        // the error message: "cannot change into wal mode from within a transaction".
        conn.execute_batch("PRAGMA journal_mode = wal;").await?;
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!("../migrations/crypto_store/001_init.sql"))?;
            txn.set_db_version(1)
        })
        .await?;
        return Ok(1);
    }

    Ok(version)
}

/// Run migrations for the given version of the database.
///
/// # Arguments
///
/// * `store` - The store to run the migrations on
///
/// * `version` - The current version of the database.
///
/// * `max_version` - The maximum version that the database will be migrated to.
///   Only used for testing, so will only be checked for the versions that are
///   needed for tests.
pub(crate) async fn run_migrations(
    store: &SqliteCryptoStore,
    version: u8,
    max_version: Option<u8>,
) -> Result<()> {
    let conn = store.write().await?;

    if version < 2 {
        debug!("Upgrading database to version 2");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!("../migrations/crypto_store/002_reset_olm_hash.sql"))?;
            txn.set_db_version(2)
        })
        .await?;
    }

    if version < 3 {
        debug!("Upgrading database to version 3");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!("../migrations/crypto_store/003_room_settings.sql"))?;
            txn.set_db_version(3)
        })
        .await?;
    }

    if version < 4 {
        debug!("Upgrading database to version 4");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/crypto_store/004_drop_outbound_group_sessions.sql"
            ))?;
            txn.set_db_version(4)
        })
        .await?;
    }

    if version < 5 {
        debug!("Upgrading database to version 5");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!("../migrations/crypto_store/005_withheld_code.sql"))?;
            txn.set_db_version(5)
        })
        .await?;
    }

    if version < 6 {
        debug!("Upgrading database to version 6");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/crypto_store/006_drop_outbound_group_sessions.sql"
            ))?;
            txn.set_db_version(6)
        })
        .await?;
    }

    if version < 7 {
        debug!("Upgrading database to version 7");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!("../migrations/crypto_store/007_lock_leases.sql"))?;
            txn.set_db_version(7)
        })
        .await?;
    }

    if version < 8 {
        debug!("Upgrading database to version 8");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!("../migrations/crypto_store/008_secret_inbox.sql"))?;
            txn.set_db_version(8)
        })
        .await?;
    }

    if version < 9 {
        debug!("Upgrading database to version 9");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/crypto_store/009_inbound_group_session_sender_key_sender_data_type.sql"
            ))?;
            txn.set_db_version(9)
        })
        .await?;
    }

    if version < 10 {
        debug!("Upgrading database to version 10");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/crypto_store/010_received_room_key_bundles.sql"
            ))?;
            txn.set_db_version(10)
        })
        .await?;
    }

    if version < 11 {
        debug!("Upgrading database to version 11");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/crypto_store/011_received_room_key_bundles_with_curve_key.sql"
            ))?;
            txn.set_db_version(11)
        })
        .await?;
    }

    if version < 12 {
        debug!("Upgrading database to version 12");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/crypto_store/012_withheld_code_by_room.sql"
            ))?;
            txn.set_db_version(12)
        })
        .await?;
    }

    if version < 13 {
        debug!("Upgrading database to version 13");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/crypto_store/013_lease_locks_with_generation.sql"
            ))?;
            txn.set_db_version(13)
        })
        .await?;
    }

    if version < 14 {
        debug!("Upgrading database to version 14");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/crypto_store/014_room_key_backups_fully_downloaded.sql"
            ))?;
            txn.set_db_version(14)
        })
        .await?;
    }

    if version < 15 {
        debug!("Upgrading database to version 15");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/crypto_store/015_rooms_pending_key_bundle.sql"
            ))?;
            txn.set_db_version(15)
        })
        .await?;
    }

    if version < 16 {
        debug!("Upgrading database to version 16");
        conn.with_transaction(|txn| {
            txn.execute_batch(include_str!(
                "../migrations/crypto_store/016_remove_old_generation_counter.sql"
            ))?;
            txn.set_db_version(16)
        })
        .await?;
    }

    if max_version.is_some_and(|max_version| max_version < 17) {
        return Ok(());
    }

    if version < 17 {
        debug!("Upgrading database to version 17");
        let store = store.clone();
        conn.with_transaction(move |txn| {
            txn.execute_batch(include_str!(
                "../migrations/crypto_store/017_add_new_secrets_inbox.sql"
            ))?;
            let mut select_query = txn.prepare("SELECT data FROM secrets")?;
            let mut secrets = select_query.query([])?;
            let mut insert_query = txn.prepare(
                "INSERT OR IGNORE INTO secrets_inbox (secret_name, secret)
            VALUES (?1, ?2)",
            )?;
            while let Some(row) = secrets.next()? {
                let Ok(secret) =
                    store.deserialize_json::<GossippedSecret>(row.get::<_, Vec<u8>>(0)?.as_ref())
                else {
                    continue;
                };
                let Ok(encoded_secret) = store.serialize_json(&secret.event.content.secret) else {
                    continue;
                };
                insert_query.execute((
                    store.encode_key("secrets_inbox", secret.secret_name.to_string()),
                    &encoded_secret,
                ))?;
            }
            txn.execute_batch(include_str!(
                "../migrations/crypto_store/017_drop_old_secrets_inbox.sql"
            ))?;
            txn.set_db_version(17)
        })
        .await?;
    }

    if version < 18 {
        debug!("Upgrading database to version 18");
        let store = store.clone();
        conn.with_transaction(move |txn| {
            txn.execute_batch(include_str!(
                "../migrations/crypto_store/018_add_gossip_request_info.sql"
            ))?;
            let mut select_query =
                txn.prepare("SELECT request_id, sent_out, data FROM key_requests")?;
            let mut requests = select_query.query([])?;
            let mut update_query =
                txn.prepare("UPDATE OR REPLACE key_requests SET info = ?1 WHERE request_id = ?2")?;
            while let Some(row) = requests.next()? {
                let Ok(request) = store.deserialize_key_request(
                    row.get::<_, Vec<u8>>(2)?.as_ref(),
                    row.get::<_, bool>(1)?,
                ) else {
                    continue;
                };
                let info = store.encode_key("key_requests", request.info.as_key());
                update_query.execute((info, row.get::<_, Vec<u8>>(0)?))?;
            }
            txn.set_db_version(18)
        })
        .await?;
    }

    if version < 19 {
        debug!("Upgrading database to version 19");
        // Remove the sliding sync `pos` value stored in the crypto store.
        // There was recently an event cache migration that emptied the cache but never
        // reset the `pos` value, this fixes it.
        let user_id = store.load_account().await?.map(|account| account.user_id.clone());

        conn.with_transaction(move |txn| {
            if let Some(user_id) = user_id {
                txn.clear_kv(&format!("sliding_sync_store::room-list::{user_id}::instance"))?;
            }
            txn.set_db_version(19)
        })
        .await?;
    }

    Ok(())
}

trait SqliteConnectionExt {
    fn set_session(
        &self,
        session_id: &[u8],
        sender_key: &[u8],
        data: &[u8],
    ) -> rusqlite::Result<()>;

    fn set_inbound_group_session(
        &self,
        room_id: &[u8],
        session_id: &[u8],
        data: &[u8],
        backed_up: bool,
        sender_key: Option<&[u8]>,
        sender_data_type: Option<u8>,
    ) -> rusqlite::Result<()>;

    fn set_outbound_group_session(&self, room_id: &[u8], data: &[u8]) -> rusqlite::Result<()>;

    fn set_device(&self, user_id: &[u8], device_id: &[u8], data: &[u8]) -> rusqlite::Result<()>;
    fn delete_device(&self, user_id: &[u8], device_id: &[u8]) -> rusqlite::Result<()>;

    fn set_identity(&self, user_id: &[u8], data: &[u8]) -> rusqlite::Result<()>;

    fn add_olm_hash(&self, data: &[u8]) -> rusqlite::Result<()>;

    fn set_key_request(
        &self,
        request_id: &[u8],
        sent_out: bool,
        data: &[u8],
        info: &[u8],
    ) -> rusqlite::Result<()>;

    fn set_direct_withheld(
        &self,
        session_id: &[u8],
        room_id: &[u8],
        data: &[u8],
    ) -> rusqlite::Result<()>;

    fn set_room_settings(&self, room_id: &[u8], data: &[u8]) -> rusqlite::Result<()>;

    fn set_secret(&self, request_id: &[u8], data: &[u8]) -> rusqlite::Result<()>;

    fn set_received_room_key_bundle(
        &self,
        room_id: &[u8],
        user_id: &[u8],
        data: &[u8],
    ) -> rusqlite::Result<()>;

    fn set_has_downloaded_all_room_keys(&self, room_id: &[u8]) -> rusqlite::Result<()>;

    fn set_room_pending_key_bundle(
        &self,
        room_id: &[u8],
        details: Option<&[u8]>,
    ) -> rusqlite::Result<()>;
}

impl SqliteConnectionExt for rusqlite::Connection {
    fn set_session(
        &self,
        session_id: &[u8],
        sender_key: &[u8],
        data: &[u8],
    ) -> rusqlite::Result<()> {
        self.execute(
            "INSERT INTO session (session_id, sender_key, data)
             VALUES (?1, ?2, ?3)
             ON CONFLICT (session_id) DO UPDATE SET data = ?3",
            (session_id, sender_key, data),
        )?;
        Ok(())
    }

    fn set_inbound_group_session(
        &self,
        room_id: &[u8],
        session_id: &[u8],
        data: &[u8],
        backed_up: bool,
        sender_key: Option<&[u8]>,
        sender_data_type: Option<u8>,
    ) -> rusqlite::Result<()> {
        self.execute(
            "INSERT INTO inbound_group_session (session_id, room_id, data, backed_up, sender_key, sender_data_type) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (session_id) DO UPDATE SET data = ?3, backed_up = ?4, sender_key = ?5, sender_data_type = ?6",
            (session_id, room_id, data, backed_up, sender_key, sender_data_type),
        )?;
        Ok(())
    }

    fn set_outbound_group_session(&self, room_id: &[u8], data: &[u8]) -> rusqlite::Result<()> {
        self.execute(
            "INSERT INTO outbound_group_session (room_id, data) \
             VALUES (?1, ?2)
             ON CONFLICT (room_id) DO UPDATE SET data = ?2",
            (room_id, data),
        )?;
        Ok(())
    }

    fn set_device(&self, user_id: &[u8], device_id: &[u8], data: &[u8]) -> rusqlite::Result<()> {
        self.execute(
            "INSERT INTO device (user_id, device_id, data) \
             VALUES (?1, ?2, ?3)
             ON CONFLICT (user_id, device_id) DO UPDATE SET data = ?3",
            (user_id, device_id, data),
        )?;
        Ok(())
    }

    fn delete_device(&self, user_id: &[u8], device_id: &[u8]) -> rusqlite::Result<()> {
        self.execute(
            "DELETE FROM device WHERE user_id = ? AND device_id = ?",
            (user_id, device_id),
        )?;
        Ok(())
    }

    fn set_identity(&self, user_id: &[u8], data: &[u8]) -> rusqlite::Result<()> {
        self.execute(
            "INSERT INTO identity (user_id, data) \
             VALUES (?1, ?2)
             ON CONFLICT (user_id) DO UPDATE SET data = ?2",
            (user_id, data),
        )?;
        Ok(())
    }

    fn add_olm_hash(&self, data: &[u8]) -> rusqlite::Result<()> {
        self.execute("INSERT INTO olm_hash (data) VALUES (?) ON CONFLICT DO NOTHING", (data,))?;
        Ok(())
    }

    fn set_key_request(
        &self,
        request_id: &[u8],
        sent_out: bool,
        data: &[u8],
        info: &[u8],
    ) -> rusqlite::Result<()> {
        // The first `ON CONFLICT` cause will update a request if we try to save
        // it again.  The second `ON CONFLICT` will replace an old request for
        // the same key/secret with the new request.
        self.execute(
            "INSERT INTO key_requests (request_id, sent_out, data, info)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT (request_id) DO UPDATE SET sent_out = ?2, data = ?3, info = ?4
            ON CONFLICT (info) DO UPDATE SET request_id = ?1, sent_out = ?2, data = ?3",
            (request_id, sent_out, data, info),
        )?;
        Ok(())
    }

    fn set_direct_withheld(
        &self,
        session_id: &[u8],
        room_id: &[u8],
        data: &[u8],
    ) -> rusqlite::Result<()> {
        self.execute(
            "INSERT INTO direct_withheld_info (session_id, room_id, data)
            VALUES (?1, ?2, ?3)
            ON CONFLICT (session_id) DO UPDATE SET room_id = ?2, data = ?3",
            (session_id, room_id, data),
        )?;
        Ok(())
    }

    fn set_room_settings(&self, room_id: &[u8], data: &[u8]) -> rusqlite::Result<()> {
        self.execute(
            "INSERT INTO room_settings (room_id, data)
            VALUES (?1, ?2)
            ON CONFLICT (room_id) DO UPDATE SET data = ?2",
            (room_id, data),
        )?;
        Ok(())
    }

    fn set_secret(&self, secret_name: &[u8], secret: &[u8]) -> rusqlite::Result<()> {
        // Ignore duplicate values, since we may get set the same secret
        // multiple times.
        self.execute(
            "INSERT OR IGNORE INTO secrets_inbox (secret_name, secret)
            VALUES (?1, ?2)",
            (secret_name, secret),
        )?;

        Ok(())
    }

    fn set_received_room_key_bundle(
        &self,
        room_id: &[u8],
        sender_user_id: &[u8],
        data: &[u8],
    ) -> rusqlite::Result<()> {
        self.execute(
            "INSERT INTO received_room_key_bundle(room_id, sender_user_id, bundle_data)
            VALUES (?1, ?2, ?3)
            ON CONFLICT (room_id, sender_user_id) DO UPDATE SET bundle_data = ?3",
            (room_id, sender_user_id, data),
        )?;
        Ok(())
    }

    fn set_room_pending_key_bundle(
        &self,
        room_id: &[u8],
        data: Option<&[u8]>,
    ) -> rusqlite::Result<()> {
        if let Some(data) = data {
            self.execute(
                "INSERT INTO rooms_pending_key_bundle (room_id, data)
                 VALUES (?1, ?2)
                 ON CONFLICT (room_id) DO UPDATE SET data = ?2",
                (room_id, data),
            )?;
        } else {
            self.execute("DELETE FROM rooms_pending_key_bundle WHERE room_id = ?1", (room_id,))?;
        }
        Ok(())
    }

    fn set_has_downloaded_all_room_keys(&self, room_id: &[u8]) -> rusqlite::Result<()> {
        self.execute(
            "INSERT INTO room_key_backups_fully_downloaded(room_id)
             VALUES (?1)
             ON CONFLICT(room_id) DO NOTHING",
            (room_id,),
        )?;
        Ok(())
    }
}

#[async_trait]
trait SqliteObjectCryptoStoreExt: SqliteAsyncConnExt {
    async fn get_sessions_for_sender_key(&self, sender_key: Key) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .prepare("SELECT data FROM session WHERE sender_key = ?", |mut stmt| {
                stmt.query((sender_key,))?.mapped(|row| row.get(0)).collect()
            })
            .await?)
    }

    async fn get_inbound_group_session(
        &self,
        session_id: Key,
    ) -> Result<Option<(Vec<u8>, Vec<u8>, bool)>> {
        Ok(self
            .query_one(
                "SELECT room_id, data, backed_up FROM inbound_group_session WHERE session_id = ?",
                (session_id,),
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .await
            .optional()?)
    }

    async fn get_inbound_group_sessions(&self) -> Result<Vec<(Vec<u8>, bool)>> {
        Ok(self
            .prepare("SELECT data, backed_up FROM inbound_group_session", |mut stmt| {
                stmt.query(())?.mapped(|row| Ok((row.get(0)?, row.get(1)?))).collect()
            })
            .await?)
    }

    async fn get_inbound_group_session_counts(
        &self,
        _backup_version: Option<&str>,
    ) -> Result<RoomKeyCounts> {
        let total = self
            .query_one("SELECT count(*) FROM inbound_group_session", (), |row| row.get(0))
            .await?;
        let backed_up = self
            .query_one(
                "SELECT count(*) FROM inbound_group_session WHERE backed_up = TRUE",
                (),
                |row| row.get(0),
            )
            .await?;
        Ok(RoomKeyCounts { total, backed_up })
    }

    async fn get_inbound_group_sessions_by_room_id(
        &self,
        room_id: Key,
    ) -> Result<Vec<(Vec<u8>, bool)>> {
        Ok(self
            .prepare(
                "SELECT data, backed_up FROM inbound_group_session WHERE room_id = :room_id",
                move |mut stmt| {
                    stmt.query(named_params! {
                        ":room_id": room_id,
                    })?
                    .mapped(|row| Ok((row.get(0)?, row.get(1)?)))
                    .collect()
                },
            )
            .await?)
    }

    async fn get_inbound_group_sessions_for_device_batch(
        &self,
        sender_key: Key,
        sender_data_type: SenderDataType,
        after_session_id: Option<Key>,
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, bool)>> {
        Ok(self
            .prepare(
                "
                SELECT data, backed_up
                FROM inbound_group_session
                WHERE sender_key = :sender_key
                    AND sender_data_type = :sender_data_type
                    AND session_id > :after_session_id
                ORDER BY session_id
                LIMIT :limit
                ",
                move |mut stmt| {
                    let sender_data_type = sender_data_type as u8;

                    // If we are not provided with an `after_session_id`, use a key which will sort
                    // before all real keys: the empty string.
                    let after_session_id = after_session_id.unwrap_or(Key::Plain(Vec::new()));

                    stmt.query(named_params! {
                        ":sender_key": sender_key,
                        ":sender_data_type": sender_data_type,
                        ":after_session_id": after_session_id,
                        ":limit": limit,
                    })?
                    .mapped(|row| Ok((row.get(0)?, row.get(1)?)))
                    .collect()
                },
            )
            .await?)
    }

    async fn get_inbound_group_sessions_for_backup(&self, limit: usize) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .prepare(
                "SELECT data FROM inbound_group_session WHERE backed_up = FALSE LIMIT ?",
                move |mut stmt| stmt.query((limit,))?.mapped(|row| row.get(0)).collect(),
            )
            .await?)
    }

    async fn mark_inbound_group_sessions_as_backed_up(&self, session_ids: Vec<Key>) -> Result<()> {
        if session_ids.is_empty() {
            // We are not expecting to be called with an empty list of sessions
            warn!("No sessions to mark as backed up!");
            return Ok(());
        }

        self.chunk_large_query_over(session_ids, None, move |txn, session_ids| {
            // Safety: host parameters are not generated using any user input except the
            // number of session IDs, so it is safe from injection.
            let query = format!(
                "UPDATE inbound_group_session SET backed_up = TRUE where session_id IN ({})",
                session_ids.host_parameters()
            );
            txn.prepare(&query)?.execute(params_from_iter(session_ids))?;
            Ok(Vec::<()>::new())
        })
        .await?;

        Ok(())
    }

    async fn reset_inbound_group_session_backup_state(&self) -> Result<()> {
        self.execute("UPDATE inbound_group_session SET backed_up = FALSE", ()).await?;
        Ok(())
    }

    async fn get_outbound_group_session(&self, room_id: Key) -> Result<Option<Vec<u8>>> {
        Ok(self
            .query_one(
                "SELECT data FROM outbound_group_session WHERE room_id = ?",
                (room_id,),
                |row| row.get(0),
            )
            .await
            .optional()?)
    }

    async fn get_device(&self, user_id: Key, device_id: Key) -> Result<Option<Vec<u8>>> {
        Ok(self
            .query_one(
                "SELECT data FROM device WHERE user_id = ? AND device_id = ?",
                (user_id, device_id),
                |row| row.get(0),
            )
            .await
            .optional()?)
    }

    async fn get_user_devices(&self, user_id: Key) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .prepare("SELECT data FROM device WHERE user_id = ?", |mut stmt| {
                stmt.query((user_id,))?.mapped(|row| row.get(0)).collect()
            })
            .await?)
    }

    async fn get_user_identity(&self, user_id: Key) -> Result<Option<Vec<u8>>> {
        Ok(self
            .query_one("SELECT data FROM identity WHERE user_id = ?", (user_id,), |row| row.get(0))
            .await
            .optional()?)
    }

    async fn has_olm_hash(&self, data: Vec<u8>) -> Result<bool> {
        Ok(self
            .query_one("SELECT count(*) FROM olm_hash WHERE data = ?", (data,), |row| {
                row.get::<_, i32>(0)
            })
            .await?
            > 0)
    }

    async fn get_tracked_users(&self) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .prepare("SELECT data FROM tracked_user", |mut stmt| {
                stmt.query(())?.mapped(|row| row.get(0)).collect()
            })
            .await?)
    }

    async fn add_tracked_users(&self, users: Vec<(Key, Vec<u8>)>) -> Result<()> {
        Ok(self
            .prepare(
                "INSERT INTO tracked_user (user_id, data) \
                 VALUES (?1, ?2) \
                 ON CONFLICT (user_id) DO UPDATE SET data = ?2",
                |mut stmt| {
                    for (user_id, data) in users {
                        stmt.execute((user_id, data))?;
                    }

                    Ok(())
                },
            )
            .await?)
    }

    async fn get_outgoing_secret_request(
        &self,
        request_id: Key,
    ) -> Result<Option<(Vec<u8>, bool)>> {
        Ok(self
            .query_one(
                "SELECT data, sent_out FROM key_requests WHERE request_id = ?",
                (request_id,),
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .await
            .optional()?)
    }

    async fn get_secret_request_by_info(&self, info: Key) -> Result<Option<(Vec<u8>, bool)>> {
        Ok(self
            .query_one("SELECT data, sent_out FROM key_requests WHERE info = ?", (info,), |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .await
            .optional()?)
    }

    async fn get_unsent_secret_requests(&self) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .prepare("SELECT data FROM key_requests WHERE sent_out = FALSE", |mut stmt| {
                stmt.query(())?.mapped(|row| row.get(0)).collect()
            })
            .await?)
    }

    async fn delete_key_request(&self, request_id: Key) -> Result<()> {
        self.execute("DELETE FROM key_requests WHERE request_id = ?", (request_id,)).await?;
        Ok(())
    }

    async fn get_secrets_from_inbox(&self, secret_name: Key) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .prepare("SELECT secret FROM secrets_inbox WHERE secret_name = ?", |mut stmt| {
                stmt.query((secret_name,))?.mapped(|row| row.get(0)).collect()
            })
            .await?)
    }

    async fn delete_secrets_from_inbox(&self, secret_name: Key) -> Result<()> {
        self.execute("DELETE FROM secrets_inbox WHERE secret_name = ?", (secret_name,)).await?;
        Ok(())
    }

    async fn get_direct_withheld_info(
        &self,
        session_id: Key,
        room_id: Key,
    ) -> Result<Option<Vec<u8>>> {
        Ok(self
            .query_one(
                "SELECT data FROM direct_withheld_info WHERE session_id = ?1 AND room_id = ?2",
                (session_id, room_id),
                |row| row.get(0),
            )
            .await
            .optional()?)
    }

    async fn get_withheld_sessions_by_room_id(&self, room_id: Key) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .prepare("SELECT data FROM direct_withheld_info WHERE room_id = ?1", |mut stmt| {
                stmt.query((room_id,))?.mapped(|row| row.get(0)).collect()
            })
            .await?)
    }

    async fn get_room_settings(&self, room_id: Key) -> Result<Option<Vec<u8>>> {
        Ok(self
            .query_one("SELECT data FROM room_settings WHERE room_id = ?", (room_id,), |row| {
                row.get(0)
            })
            .await
            .optional()?)
    }

    async fn get_received_room_key_bundle(
        &self,
        room_id: Key,
        sender_user: Key,
    ) -> Result<Option<Vec<u8>>> {
        Ok(self
            .query_one(
                "SELECT bundle_data FROM received_room_key_bundle WHERE room_id = ? AND sender_user_id = ?",
                (room_id, sender_user),
                |row| { row.get(0) },
            )
            .await
            .optional()?)
    }

    async fn get_room_pending_key_bundle(&self, room_id: Key) -> Result<Option<Vec<u8>>> {
        Ok(self
            .query_one(
                "SELECT data FROM rooms_pending_key_bundle WHERE room_id = ?",
                (room_id,),
                |row| row.get(0),
            )
            .await
            .optional()?)
    }

    async fn get_all_rooms_pending_key_bundle(&self) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .query_many("SELECT data FROM rooms_pending_key_bundle", (), |row| row.get(0))
            .await?)
    }

    async fn has_downloaded_all_room_keys(&self, room_id: Key) -> Result<bool> {
        Ok(self
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM room_key_backups_fully_downloaded WHERE room_id = ?)",
                (room_id,),
                |row| row.get(0),
            )
            .await?)
    }
}

#[async_trait]
impl SqliteObjectCryptoStoreExt for SqliteAsyncConn {}

#[async_trait]
impl CryptoStore for SqliteCryptoStore {
    type Error = Error;

    async fn load_account(&self) -> Result<Option<Account>> {
        let conn = self.read().await?;
        if let Some(pickle) = conn.get_kv("account").await? {
            let pickle = self.deserialize_value(&pickle)?;

            let account = Account::from_pickle(pickle).map_err(|_| Error::Unpickle)?;

            *self.static_account.write().unwrap() = Some(account.static_data().clone());

            Ok(Some(account))
        } else {
            Ok(None)
        }
    }

    async fn load_identity(&self) -> Result<Option<PrivateCrossSigningIdentity>> {
        let conn = self.read().await?;
        if let Some(i) = conn.get_kv("identity").await? {
            let pickle = self.deserialize_value(&i)?;
            Ok(Some(PrivateCrossSigningIdentity::from_pickle(pickle).map_err(|_| Error::Unpickle)?))
        } else {
            Ok(None)
        }
    }

    async fn save_pending_changes(&self, changes: PendingChanges) -> Result<()> {
        // Serialize calls to `save_pending_changes`; there are multiple await points
        // below, and we're pickling data as we go, so we don't want to
        // invalidate data we've previously read and overwrite it in the store.
        // TODO: #2000 should make this lock go away, or change its shape.
        let _guard = self.save_changes_lock.lock().await;

        let pickled_account = if let Some(account) = changes.account {
            *self.static_account.write().unwrap() = Some(account.static_data().clone());
            Some(account.pickle())
        } else {
            None
        };

        let this = self.clone();
        self.write()
            .await?
            .with_transaction(move |txn| {
                if let Some(pickled_account) = pickled_account {
                    let serialized_account = this.serialize_value(&pickled_account)?;
                    txn.set_kv("account", &serialized_account)?;
                }

                Ok::<_, Error>(())
            })
            .await?;

        Ok(())
    }

    async fn save_changes(&self, changes: Changes) -> Result<()> {
        // Serialize calls to `save_changes`; there are multiple await points below, and
        // we're pickling data as we go, so we don't want to invalidate data
        // we've previously read and overwrite it in the store.
        // TODO: #2000 should make this lock go away, or change its shape.
        let _guard = self.save_changes_lock.lock().await;

        let pickled_private_identity =
            if let Some(i) = changes.private_identity { Some(i.pickle().await) } else { None };

        let mut session_changes = Vec::new();

        for session in changes.sessions {
            let session_id = self.encode_key("session", session.session_id());
            let sender_key = self.encode_key("session", session.sender_key().to_base64());
            let pickle = session.pickle().await;
            session_changes.push((session_id, sender_key, pickle));
        }

        let mut inbound_session_changes = Vec::new();
        for session in changes.inbound_group_sessions {
            let room_id = self.encode_key("inbound_group_session", session.room_id().as_bytes());
            let session_id = self.encode_key("inbound_group_session", session.session_id());
            let pickle = session.pickle().await;
            let sender_key =
                self.encode_key("inbound_group_session", session.sender_key().to_base64());
            inbound_session_changes.push((room_id, session_id, pickle, sender_key));
        }

        let mut outbound_session_changes = Vec::new();
        for session in changes.outbound_group_sessions {
            let room_id = self.encode_key("outbound_group_session", session.room_id().as_bytes());
            let pickle = session.pickle().await;
            outbound_session_changes.push((room_id, pickle));
        }

        let this = self.clone();
        self.write()
            .await?
            .with_transaction(move |txn| {
                if let Some(pickled_private_identity) = &pickled_private_identity {
                    let serialized_private_identity =
                        this.serialize_value(pickled_private_identity)?;
                    txn.set_kv("identity", &serialized_private_identity)?;
                }

                if let Some(token) = &changes.next_batch_token {
                    let serialized_token = this.serialize_value(token)?;
                    txn.set_kv("next_batch_token", &serialized_token)?;
                }

                if let Some(decryption_key) = &changes.backup_decryption_key {
                    let serialized_decryption_key = this.serialize_value(decryption_key)?;
                    txn.set_kv("recovery_key_v1", &serialized_decryption_key)?;
                }

                if let Some(backup_version) = &changes.backup_version {
                    let serialized_backup_version = this.serialize_value(backup_version)?;
                    txn.set_kv("backup_version_v1", &serialized_backup_version)?;
                }

                if let Some(pickle_key) = &changes.dehydrated_device_pickle_key {
                    let serialized_pickle_key = this.serialize_value(pickle_key)?;
                    txn.set_kv(DEHYDRATED_DEVICE_PICKLE_KEY, &serialized_pickle_key)?;
                }

                for device in changes.devices.new.iter().chain(&changes.devices.changed) {
                    let user_id = this.encode_key("device", device.user_id().as_bytes());
                    let device_id = this.encode_key("device", device.device_id().as_bytes());
                    let data = this.serialize_value(&device)?;
                    txn.set_device(&user_id, &device_id, &data)?;
                }

                for device in &changes.devices.deleted {
                    let user_id = this.encode_key("device", device.user_id().as_bytes());
                    let device_id = this.encode_key("device", device.device_id().as_bytes());
                    txn.delete_device(&user_id, &device_id)?;
                }

                for identity in changes.identities.changed.iter().chain(&changes.identities.new) {
                    let user_id = this.encode_key("identity", identity.user_id().as_bytes());
                    let data = this.serialize_value(&identity)?;
                    txn.set_identity(&user_id, &data)?;
                }

                for (session_id, sender_key, pickle) in &session_changes {
                    let serialized_session = this.serialize_value(&pickle)?;
                    txn.set_session(session_id, sender_key, &serialized_session)?;
                }

                for (room_id, session_id, pickle, sender_key) in &inbound_session_changes {
                    let serialized_session = this.serialize_value(&pickle)?;
                    txn.set_inbound_group_session(
                        room_id,
                        session_id,
                        &serialized_session,
                        pickle.backed_up,
                        Some(sender_key),
                        Some(pickle.sender_data.to_type() as u8),
                    )?;
                }

                for (room_id, pickle) in &outbound_session_changes {
                    let serialized_session = this.serialize_json(&pickle)?;
                    txn.set_outbound_group_session(room_id, &serialized_session)?;
                }

                for hash in &changes.message_hashes {
                    let hash = rmp_serde::to_vec(hash)?;
                    txn.add_olm_hash(&hash)?;
                }

                for request in changes.key_requests {
                    let request_id = this.encode_key("key_requests", request.request_id.as_bytes());
                    let serialized_request = this.serialize_value(&request)?;
                    let serialized_info = this.encode_key("key_requests", request.info.as_key());
                    txn.set_key_request(
                        &request_id,
                        request.sent_out,
                        &serialized_request,
                        &serialized_info,
                    )?;
                }

                for (room_id, data) in changes.withheld_session_info {
                    for (session_id, event) in data {
                        let session_id = this.encode_key("direct_withheld_info", session_id);
                        let room_id = this.encode_key("direct_withheld_info", &room_id);
                        let serialized_info = this.serialize_json(&event)?;
                        txn.set_direct_withheld(&session_id, &room_id, &serialized_info)?;
                    }
                }

                for (room_id, settings) in changes.room_settings {
                    let room_id = this.encode_key("room_settings", room_id.as_bytes());
                    let value = this.serialize_value(&settings)?;
                    txn.set_room_settings(&room_id, &value)?;
                }

                for secret in changes.secrets {
                    let secret_name =
                        this.encode_key("secrets_inbox", secret.secret_name.to_string());
                    let value = this.serialize_json(secret.secret.deref())?;
                    txn.set_secret(&secret_name, &value)?;
                }

                for bundle in changes.received_room_key_bundles {
                    let room_id =
                        this.encode_key("received_room_key_bundle", &bundle.bundle_data.room_id);
                    let user_id = this.encode_key("received_room_key_bundle", &bundle.sender_user);
                    let value = this.serialize_value(&bundle)?;
                    txn.set_received_room_key_bundle(&room_id, &user_id, &value)?;
                }

                for room in changes.room_key_backups_fully_downloaded {
                    let room_id = this.encode_key("room_key_backups_fully_downloaded", &room);
                    txn.set_has_downloaded_all_room_keys(&room_id)?;
                }

                for (room, details) in changes.rooms_pending_key_bundle {
                    let room_id = this.encode_key("rooms_pending_key_bundle", &room);
                    let value = details.as_ref().map(|d| this.serialize_value(d)).transpose()?;
                    txn.set_room_pending_key_bundle(&room_id, value.as_deref())?;
                }

                Ok::<_, Error>(())
            })
            .await?;

        Ok(())
    }

    async fn save_inbound_group_sessions(
        &self,
        sessions: Vec<InboundGroupSession>,
        backed_up_to_version: Option<&str>,
    ) -> matrix_sdk_crypto::store::Result<(), Self::Error> {
        // Sanity-check that the data in the sessions corresponds to backed_up_version
        sessions.iter().for_each(|s| {
            let backed_up = s.backed_up();
            if backed_up != backed_up_to_version.is_some() {
                warn!(
                    backed_up,
                    backed_up_to_version,
                    "Session backed-up flag does not correspond to backup version setting",
                );
            }
        });

        // Currently, this store doesn't save the backup version separately, so this
        // just delegates to save_changes.
        self.save_changes(Changes { inbound_group_sessions: sessions, ..Changes::default() }).await
    }

    async fn get_sessions(&self, sender_key: &str) -> Result<Option<Vec<Session>>> {
        let device_keys = self.get_own_device().await?.as_device_keys().clone();

        let sessions: Vec<_> = self
            .read()
            .await?
            .get_sessions_for_sender_key(self.encode_key("session", sender_key.as_bytes()))
            .await?
            .into_iter()
            .map(|bytes| {
                let pickle = self.deserialize_value(&bytes)?;
                Session::from_pickle(device_keys.clone(), pickle).map_err(|_| Error::AccountUnset)
            })
            .collect::<Result<_>>()?;

        if sessions.is_empty() { Ok(None) } else { Ok(Some(sessions)) }
    }

    #[instrument(skip(self))]
    async fn get_inbound_group_session(
        &self,
        room_id: &RoomId,
        session_id: &str,
    ) -> Result<Option<InboundGroupSession>> {
        let session_id = self.encode_key("inbound_group_session", session_id);
        let Some((room_id_from_db, value, backed_up)) =
            self.read().await?.get_inbound_group_session(session_id).await?
        else {
            return Ok(None);
        };

        let room_id = self.encode_key("inbound_group_session", room_id.as_bytes());
        if *room_id != room_id_from_db {
            warn!("expected room_id for session_id doesn't match what's in the DB");
            return Ok(None);
        }

        Ok(Some(self.deserialize_and_unpickle_inbound_group_session(value, backed_up)?))
    }

    async fn get_inbound_group_sessions(&self) -> Result<Vec<InboundGroupSession>> {
        self.read()
            .await?
            .get_inbound_group_sessions()
            .await?
            .into_iter()
            .map(|(value, backed_up)| {
                self.deserialize_and_unpickle_inbound_group_session(value, backed_up)
            })
            .collect()
    }

    async fn get_inbound_group_sessions_by_room_id(
        &self,
        room_id: &RoomId,
    ) -> Result<Vec<InboundGroupSession>> {
        let room_id = self.encode_key("inbound_group_session", room_id.as_bytes());
        self.read()
            .await?
            .get_inbound_group_sessions_by_room_id(room_id)
            .await?
            .into_iter()
            .map(|(value, backed_up)| {
                self.deserialize_and_unpickle_inbound_group_session(value, backed_up)
            })
            .collect()
    }

    async fn get_inbound_group_sessions_for_device_batch(
        &self,
        sender_key: Curve25519PublicKey,
        sender_data_type: SenderDataType,
        after_session_id: Option<String>,
        limit: usize,
    ) -> Result<Vec<InboundGroupSession>, Self::Error> {
        let after_session_id =
            after_session_id.map(|session_id| self.encode_key("inbound_group_session", session_id));
        let sender_key = self.encode_key("inbound_group_session", sender_key.to_base64());

        self.read()
            .await?
            .get_inbound_group_sessions_for_device_batch(
                sender_key,
                sender_data_type,
                after_session_id,
                limit,
            )
            .await?
            .into_iter()
            .map(|(value, backed_up)| {
                self.deserialize_and_unpickle_inbound_group_session(value, backed_up)
            })
            .collect()
    }

    async fn inbound_group_session_counts(
        &self,
        backup_version: Option<&str>,
    ) -> Result<RoomKeyCounts> {
        Ok(self.read().await?.get_inbound_group_session_counts(backup_version).await?)
    }

    async fn inbound_group_sessions_for_backup(
        &self,
        _backup_version: &str,
        limit: usize,
    ) -> Result<Vec<InboundGroupSession>> {
        self.read()
            .await?
            .get_inbound_group_sessions_for_backup(limit)
            .await?
            .into_iter()
            .map(|value| self.deserialize_and_unpickle_inbound_group_session(value, false))
            .collect()
    }

    async fn mark_inbound_group_sessions_as_backed_up(
        &self,
        _backup_version: &str,
        session_ids: &[(&RoomId, &str)],
    ) -> Result<()> {
        Ok(self
            .write()
            .await?
            .mark_inbound_group_sessions_as_backed_up(
                session_ids
                    .iter()
                    .map(|(_, s)| self.encode_key("inbound_group_session", s))
                    .collect(),
            )
            .await?)
    }

    async fn reset_backup_state(&self) -> Result<()> {
        Ok(self.write().await?.reset_inbound_group_session_backup_state().await?)
    }

    async fn load_backup_keys(&self) -> Result<BackupKeys> {
        let conn = self.read().await?;

        let backup_version = conn
            .get_kv("backup_version_v1")
            .await?
            .map(|value| self.deserialize_value(&value))
            .transpose()?;

        let decryption_key = conn
            .get_kv("recovery_key_v1")
            .await?
            .map(|value| self.deserialize_value(&value))
            .transpose()?;

        Ok(BackupKeys { backup_version, decryption_key })
    }

    async fn load_dehydrated_device_pickle_key(&self) -> Result<Option<DehydratedDeviceKey>> {
        let conn = self.read().await?;

        conn.get_kv(DEHYDRATED_DEVICE_PICKLE_KEY)
            .await?
            .map(|value| self.deserialize_value(&value))
            .transpose()
    }

    async fn delete_dehydrated_device_pickle_key(&self) -> Result<(), Self::Error> {
        Ok(self.write().await?.clear_kv(DEHYDRATED_DEVICE_PICKLE_KEY).await?)
    }
    async fn get_outbound_group_session(
        &self,
        room_id: &RoomId,
    ) -> Result<Option<OutboundGroupSession>> {
        let room_id = self.encode_key("outbound_group_session", room_id.as_bytes());
        let Some(value) = self.read().await?.get_outbound_group_session(room_id).await? else {
            return Ok(None);
        };

        let account_info = self.get_static_account().ok_or(Error::AccountUnset)?;

        let pickle = self.deserialize_json(&value)?;
        let session = OutboundGroupSession::from_pickle(
            account_info.device_id,
            account_info.identity_keys,
            pickle,
        )
        .map_err(|_| Error::Unpickle)?;

        return Ok(Some(session));
    }

    async fn load_tracked_users(&self) -> Result<Vec<TrackedUser>> {
        self.read()
            .await?
            .get_tracked_users()
            .await?
            .iter()
            .map(|value| self.deserialize_value(value))
            .collect()
    }

    async fn save_tracked_users(&self, tracked_users: &[(&UserId, bool)]) -> Result<()> {
        let users: Vec<(Key, Vec<u8>)> = tracked_users
            .iter()
            .map(|(u, d)| {
                let user_id = self.encode_key("tracked_users", u.as_bytes());
                let data =
                    self.serialize_value(&TrackedUser { user_id: (*u).into(), dirty: *d })?;
                Ok((user_id, data))
            })
            .collect::<Result<_>>()?;

        Ok(self.write().await?.add_tracked_users(users).await?)
    }

    async fn get_device(
        &self,
        user_id: &UserId,
        device_id: &DeviceId,
    ) -> Result<Option<DeviceData>> {
        let user_id = self.encode_key("device", user_id.as_bytes());
        let device_id = self.encode_key("device", device_id.as_bytes());
        Ok(self
            .read()
            .await?
            .get_device(user_id, device_id)
            .await?
            .map(|value| self.deserialize_value(&value))
            .transpose()?)
    }

    async fn get_user_devices(
        &self,
        user_id: &UserId,
    ) -> Result<HashMap<OwnedDeviceId, DeviceData>> {
        let user_id = self.encode_key("device", user_id.as_bytes());
        self.read()
            .await?
            .get_user_devices(user_id)
            .await?
            .into_iter()
            .map(|value| {
                let device: DeviceData = self.deserialize_value(&value)?;
                Ok((device.device_id().to_owned(), device))
            })
            .collect()
    }

    async fn get_own_device(&self) -> Result<DeviceData> {
        let account_info = self.get_static_account().ok_or(Error::AccountUnset)?;

        Ok(self
            .get_device(&account_info.user_id, &account_info.device_id)
            .await?
            .expect("We should be able to find our own device."))
    }

    async fn get_user_identity(&self, user_id: &UserId) -> Result<Option<UserIdentityData>> {
        let user_id = self.encode_key("identity", user_id.as_bytes());
        Ok(self
            .read()
            .await?
            .get_user_identity(user_id)
            .await?
            .map(|value| self.deserialize_value(&value))
            .transpose()?)
    }

    async fn is_message_known(
        &self,
        message_hash: &matrix_sdk_crypto::olm::OlmMessageHash,
    ) -> Result<bool> {
        let value = rmp_serde::to_vec(message_hash)?;
        Ok(self.read().await?.has_olm_hash(value).await?)
    }

    async fn get_outgoing_secret_requests(
        &self,
        request_id: &TransactionId,
    ) -> Result<Option<GossipRequest>> {
        let request_id = self.encode_key("key_requests", request_id.as_bytes());
        Ok(self
            .read()
            .await?
            .get_outgoing_secret_request(request_id)
            .await?
            .map(|(value, sent_out)| self.deserialize_key_request(&value, sent_out))
            .transpose()?)
    }

    async fn get_secret_request_by_info(
        &self,
        key_info: &SecretInfo,
    ) -> Result<Option<GossipRequest>> {
        let key_info = self.encode_key("key_requests", key_info.as_key());
        Ok(self
            .read()
            .await?
            .get_secret_request_by_info(key_info)
            .await?
            .map(|(value, sent_out)| self.deserialize_key_request(&value, sent_out))
            .transpose()?)
    }

    async fn get_unsent_secret_requests(&self) -> Result<Vec<GossipRequest>> {
        self.read()
            .await?
            .get_unsent_secret_requests()
            .await?
            .iter()
            .map(|value| {
                let request = self.deserialize_key_request(value, false)?;
                Ok(request)
            })
            .collect()
    }

    async fn delete_outgoing_secret_requests(&self, request_id: &TransactionId) -> Result<()> {
        let request_id = self.encode_key("key_requests", request_id.as_bytes());
        Ok(self.write().await?.delete_key_request(request_id).await?)
    }

    async fn get_secrets_from_inbox(
        &self,
        secret_name: &SecretName,
    ) -> Result<Vec<Zeroizing<String>>> {
        let secret_name = self.encode_key("secrets_inbox", secret_name.to_string());

        self.read()
            .await?
            .get_secrets_from_inbox(secret_name)
            .await?
            .into_iter()
            .map(|value| self.deserialize_json(value.as_ref()).map(|value: String| value.into()))
            .collect()
    }

    async fn delete_secrets_from_inbox(&self, secret_name: &SecretName) -> Result<()> {
        let secret_name = self.encode_key("secrets_inbox", secret_name.to_string());
        self.write().await?.delete_secrets_from_inbox(secret_name).await
    }

    async fn get_withheld_info(
        &self,
        room_id: &RoomId,
        session_id: &str,
    ) -> Result<Option<RoomKeyWithheldEntry>> {
        let room_id = self.encode_key("direct_withheld_info", room_id);
        let session_id = self.encode_key("direct_withheld_info", session_id);

        self.read()
            .await?
            .get_direct_withheld_info(session_id, room_id)
            .await?
            .map(|value| {
                let info = self.deserialize_json::<RoomKeyWithheldEntry>(&value)?;
                Ok(info)
            })
            .transpose()
    }

    async fn get_withheld_sessions_by_room_id(
        &self,
        room_id: &RoomId,
    ) -> matrix_sdk_crypto::store::Result<Vec<RoomKeyWithheldEntry>, Self::Error> {
        let room_id = self.encode_key("direct_withheld_info", room_id);

        self.read()
            .await?
            .get_withheld_sessions_by_room_id(room_id)
            .await?
            .into_iter()
            .map(|value| self.deserialize_json(&value))
            .collect()
    }

    async fn get_room_settings(&self, room_id: &RoomId) -> Result<Option<RoomSettings>> {
        let room_id = self.encode_key("room_settings", room_id.as_bytes());
        let Some(value) = self.read().await?.get_room_settings(room_id).await? else {
            return Ok(None);
        };

        let settings = self.deserialize_value(&value)?;

        return Ok(Some(settings));
    }

    async fn get_received_room_key_bundle_data(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
    ) -> Result<Option<StoredRoomKeyBundleData>> {
        let room_id = self.encode_key("received_room_key_bundle", room_id);
        let user_id = self.encode_key("received_room_key_bundle", user_id);
        self.read()
            .await?
            .get_received_room_key_bundle(room_id, user_id)
            .await?
            .map(|value| self.deserialize_value(&value))
            .transpose()
    }

    async fn has_downloaded_all_room_keys(&self, room_id: &RoomId) -> Result<bool> {
        let room_id = self.encode_key("room_key_backups_fully_downloaded", room_id);
        self.read().await?.has_downloaded_all_room_keys(room_id).await
    }

    async fn get_pending_key_bundle_details_for_room(
        &self,
        room_id: &RoomId,
    ) -> Result<Option<RoomPendingKeyBundleDetails>> {
        let room_id = self.encode_key("rooms_pending_key_bundle", room_id.as_bytes());
        let Some(value) = self.read().await?.get_room_pending_key_bundle(room_id).await? else {
            return Ok(None);
        };

        let details = self.deserialize_value(&value)?;
        Ok(Some(details))
    }

    async fn get_all_rooms_pending_key_bundles(&self) -> Result<Vec<RoomPendingKeyBundleDetails>> {
        let details = self.read().await?.get_all_rooms_pending_key_bundle().await?;
        let room_ids = details
            .into_iter()
            .map(|value| self.deserialize_value(&value))
            .collect::<Result<_, _>>()?;
        Ok(room_ids)
    }

    async fn get_custom_value(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let Some(serialized) = self.read().await?.get_kv(key).await? else {
            return Ok(None);
        };
        let value = if let Some(cipher) = &self.store_cipher {
            let encrypted = rmp_serde::from_slice(&serialized)?;
            cipher.decrypt_value_data(encrypted)?
        } else {
            serialized
        };

        Ok(Some(value))
    }

    async fn set_custom_value(&self, key: &str, value: Vec<u8>) -> Result<()> {
        let serialized = if let Some(cipher) = &self.store_cipher {
            let encrypted = cipher.encrypt_value_data(value)?;
            rmp_serde::to_vec_named(&encrypted)?
        } else {
            value
        };

        self.write().await?.set_kv(key, serialized).await?;
        Ok(())
    }

    async fn remove_custom_value(&self, key: &str) -> Result<()> {
        let key = key.to_owned();
        self.write()
            .await?
            .interact(move |conn| conn.execute("DELETE FROM kv WHERE key = ?1", (&key,)))
            .await
            .unwrap()?;
        Ok(())
    }

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

    async fn next_batch_token(&self) -> Result<Option<String>, Self::Error> {
        let conn = self.read().await?;
        if let Some(token) = conn.get_kv("next_batch_token").await? {
            let maybe_token: Option<String> = self.deserialize_value(&token)?;
            Ok(maybe_token)
        } else {
            Ok(None)
        }
    }

    async fn close(&self) -> Result<()> {
        connection::close_connections(&self.connections, "Crypto store").await;
        Ok(())
    }

    async fn reopen(&self) -> Result<()> {
        connection::reopen_connections(
            &self.connections,
            self.db_path.clone(),
            self.pool_config,
            self.runtime_config,
        )
        .await?;
        Ok(())
    }

    async fn get_size(&self) -> Result<Option<usize>, Self::Error> {
        Ok(Some(self.read().await?.get_db_size().await?))
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::LazyLock};

    use matrix_sdk_common::deserialized_responses::WithheldCode;
    use matrix_sdk_crypto::{
        cryptostore_integration_tests, cryptostore_integration_tests_time, olm::SenderDataType,
        store::CryptoStore,
    };
    use matrix_sdk_test::async_test;
    use ruma::{device_id, room_id, user_id};
    use similar_asserts::assert_eq;
    use tempfile::{TempDir, tempdir};
    use tokio::fs;

    use super::SqliteCryptoStore;
    use crate::SqliteStoreConfig;

    static TMP_DIR: LazyLock<TempDir> = LazyLock::new(|| tempdir().unwrap());

    struct TestDb {
        // Needs to be kept alive because the Drop implementation for TempDir deletes the
        // directory.
        _dir: TempDir,
        database: SqliteCryptoStore,
    }

    fn copy_db(data_path: &str) -> TempDir {
        let db_name = super::DATABASE_NAME;

        let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let database_path = manifest_path.join(data_path).join(db_name);

        let tmpdir = tempdir().unwrap();
        let destination = tmpdir.path().join(db_name);

        // Copy the test database to the tempdir so our test runs are idempotent.
        std::fs::copy(&database_path, destination).unwrap();

        tmpdir
    }

    async fn get_test_db(data_path: &str, passphrase: Option<&str>) -> TestDb {
        let tmpdir = copy_db(data_path);

        let database = SqliteCryptoStore::open(tmpdir.path(), passphrase)
            .await
            .expect("Can't open the test store");

        TestDb { _dir: tmpdir, database }
    }

    #[async_test]
    async fn test_pool_size() {
        let store_open_config =
            SqliteStoreConfig::new(TMP_DIR.path().join("test_pool_size")).pool_max_size(42);

        let store = SqliteCryptoStore::open_with_config(&store_open_config).await.unwrap();

        let guard = store.connections.lock().await;
        let conns = guard.as_ref().unwrap();
        assert_eq!(conns.pool.status().max_size, 42);
    }

    /// Test that we didn't regress in our storage layer by loading data from a
    /// pre-filled database, or in other words use a test vector for this.
    #[async_test]
    async fn test_open_test_vector_store() {
        let TestDb { _dir: _, database } = get_test_db("testing/data/storage", None).await;

        let account = database
            .load_account()
            .await
            .unwrap()
            .expect("The test database is prefilled with data, we should find an account");

        let user_id = account.user_id();
        let device_id = account.device_id();

        assert_eq!(
            user_id.as_str(),
            "@pjtest:synapse-oidc.element.dev",
            "The user ID should match to the one we expect."
        );

        assert_eq!(
            device_id.as_str(),
            "v4TqgcuIH6",
            "The device ID should match to the one we expect."
        );

        let device = database
            .get_device(user_id, device_id)
            .await
            .unwrap()
            .expect("Our own device should be found in the store.");

        assert_eq!(device.device_id(), device_id);
        assert_eq!(device.user_id(), user_id);

        assert_eq!(
            device.ed25519_key().expect("The device should have a Ed25519 key.").to_base64(),
            "+cxl1Gl3du5i7UJwfWnoRDdnafFF+xYdAiTYYhYLr8s"
        );

        assert_eq!(
            device.curve25519_key().expect("The device should have a Curve25519 key.").to_base64(),
            "4SL9eEUlpyWSUvjljC5oMjknHQQJY7WZKo5S1KL/5VU"
        );

        let identity = database
            .get_user_identity(user_id)
            .await
            .unwrap()
            .expect("The store should contain an identity.");

        assert_eq!(identity.user_id(), user_id);

        let identity = identity
            .own()
            .expect("The identity should be of the correct type, it should be our own identity.");

        let master_key = identity
            .master_key()
            .get_first_key()
            .expect("Our own identity should have a master key");

        assert_eq!(master_key.to_base64(), "iCUEtB1RwANeqRa5epDrblLk4mer/36sylwQ5hYY3oE");
    }

    /// Test that we didn't regress in our storage layer by loading data from a
    /// pre-filled database, or in other words use a test vector for this.
    #[async_test]
    async fn test_open_test_vector_encrypted_store() {
        let TestDb { _dir: _, database } = get_test_db(
            "testing/data/storage/alice",
            Some(concat!(
                "/rCia2fYAJ+twCZ1Xm2mxFCYcmJdyzkdJjwtgXsziWpYS/UeNxnixuSieuwZXm+x1VsJHmWpl",
                "H+QIQBZpEGZtC9/S/l8xK+WOCesmET0o6yJ/KP73ofDtjBlnNpPwuHLKFpyTbyicpCgQ4UT+5E",
                "UBuJ08TY9Ujdf1D13k5kr5tSZUefDKKCuG1fCRqlU8ByRas1PMQsZxT2W8t7QgBrQiiGmhpo/O",
                "Ti4hfx97GOxncKcxTzppiYQNoHs/f15+XXQD7/oiCcqRIuUlXNsU6hRpFGmbYx2Pi1eyQViQCt",
                "B5dAEiSD0N8U81wXYnpynuTPtnL+hfnOJIn7Sy7mkERQeKg"
            )),
        )
        .await;

        let account = database
            .load_account()
            .await
            .unwrap()
            .expect("The test database is prefilled with data, we should find an account");

        let user_id = account.user_id();
        let device_id = account.device_id();

        assert_eq!(
            user_id.as_str(),
            "@alice:localhost",
            "The user ID should match to the one we expect."
        );

        assert_eq!(
            device_id.as_str(),
            "JVVORTHFXY",
            "The device ID should match to the one we expect."
        );

        let tracked_users =
            database.load_tracked_users().await.expect("Should be tracking some users");

        assert_eq!(tracked_users.len(), 6);

        let known_users = vec![
            user_id!("@alice:localhost"),
            user_id!("@dehydration3:localhost"),
            user_id!("@eve:localhost"),
            user_id!("@bob:localhost"),
            user_id!("@malo:localhost"),
            user_id!("@carl:localhost"),
        ];

        // load the identities
        for user_id in known_users {
            database.get_user_identity(user_id).await.expect("Should load this identity").unwrap();
        }

        let carl_identity =
            database.get_user_identity(user_id!("@carl:localhost")).await.unwrap().unwrap();

        assert_eq!(
            carl_identity.master_key().get_first_key().unwrap().to_base64(),
            "CdhKYYDeBDQveOioXEGWhTPCyzc63Irpar3CNyfun2Q"
        );
        assert!(!carl_identity.was_previously_verified());

        let bob_identity =
            database.get_user_identity(user_id!("@bob:localhost")).await.unwrap().unwrap();

        assert_eq!(
            bob_identity.master_key().get_first_key().unwrap().to_base64(),
            "COh2GYOJWSjem5QPRCaGp9iWV83IELG1IzLKW2S3pFY"
        );
        // Bob is verified so this flag should be set
        assert!(bob_identity.was_previously_verified());

        let known_devices = vec![
            (device_id!("OPXQHCZSKW"), user_id!("@alice:localhost")),
            // a dehydrated one
            (
                device_id!("EvW+9IrGR10KVgVeZP25/KaPfx4R86FofVMcaz7VOho"),
                user_id!("@alice:localhost"),
            ),
            (device_id!("HEEFRFQENV"), user_id!("@alice:localhost")),
            (device_id!("JVVORTHFXY"), user_id!("@alice:localhost")),
            (device_id!("NQUWWSKKHS"), user_id!("@alice:localhost")),
            (device_id!("ORBLPFYCPG"), user_id!("@alice:localhost")),
            (device_id!("YXOWENSEGM"), user_id!("@dehydration3:localhost")),
            (device_id!("VXLFMYCHXC"), user_id!("@bob:localhost")),
            (device_id!("FDGDQAEWOW"), user_id!("@bob:localhost")),
            (device_id!("VXLFMYCHXC"), user_id!("@bob:localhost")),
            (device_id!("FDGDQAEWOW"), user_id!("@bob:localhost")),
            (device_id!("QKUKWJTTQC"), user_id!("@malo:localhost")),
            (device_id!("LOUXJECTFG"), user_id!("@malo:localhost")),
            (device_id!("MKKMAEVLPB"), user_id!("@carl:localhost")),
        ];

        for (device_id, user_id) in known_devices {
            database.get_device(user_id, device_id).await.expect("Should load the device").unwrap();
        }

        let known_sender_key_to_session_count = vec![
            ("FfYcYfDF4nWy+LHdK6CEpIMlFAQDORc30WUkghL06kM", 1),
            ("EvW+9IrGR10KVgVeZP25/KaPfx4R86FofVMcaz7VOho", 1),
            ("hAGsoA4a9M6wwEUX5Q1jux1i+tUngLi01n5AmhDoHTY", 1),
            ("aKqtSJymLzuoglWFwPGk1r/Vm2LE2hFESzXxn4RNjRM", 0),
            ("zHK1psCrgeMn0kaz8hcdvA3INyar9jg1yfrSp0p1pHo", 1),
            ("1QmBA316Wj5jIFRwNOti6N6Xh/vW0bsYCcR4uPfy8VQ", 1),
            ("g5ef2vZF3VXgSPyODIeXpyHIRkuthvLhGvd6uwYggWU", 1),
            ("o7hfupPd1VsNkRIvdlH6ujrEJFSKjFCGbxhAd31XxjI", 1),
            ("Z3RxKQLxY7xpP+ZdOGR2SiNE37SrvmRhW7GPu1UGdm8", 1),
            ("GDomaav8NiY3J+dNEeApJm+O0FooJ3IpVaIyJzCN4w4", 1),
            ("7m7fqkHyEr47V5s/KjaxtJMOr3pSHrrns2q2lWpAQi8", 0),
            ("9psAkPUIF8vNbWbnviX3PlwRcaeO53EHJdNtKpTY1X0", 0),
            ("mqanh+ztw5oRtpqYQgLGW864i6NY2zpoKMIlrcyC+Aw", 0),
            ("fJU/TJdbsv7tVbbpHw1Ke73ziElnM32cNhP2WIg4T10", 0),
            ("sUIeFeFcCZoa5IC6nJ6Vrbvztcyx09m8BBg57XKRClg", 1),
        ];

        for (id, count) in known_sender_key_to_session_count {
            let olm_sessions =
                database.get_sessions(id).await.expect("Should have some olm sessions");

            println!("### Session id: {id:?}");
            assert_eq!(olm_sessions.map_or(0, |v| v.len()), count);
        }

        let inbound_group_sessions = database.get_inbound_group_sessions().await.unwrap();
        assert_eq!(inbound_group_sessions.len(), 15);
        let known_inbound_group_sessions = vec![
            (
                "5hNAxrLai3VI0LKBwfh3wLfksfBFWds0W1a5X5/vSXA",
                room_id!("!SRstFdydzrGwJYtVfm:localhost"),
            ),
            (
                "M6d2eU3y54gaYTbvGSlqa/xc1Az35l56Cp9sxzHWO4g",
                room_id!("!SRstFdydzrGwJYtVfm:localhost"),
            ),
            (
                "IrydwXkRk2N2AqUMIVmLL3oJgMq14R9KId0P/uSD100",
                room_id!("!SRstFdydzrGwJYtVfm:localhost"),
            ),
            (
                "Y74+l9jTo7N5UF+GQwdpgJGe4sn1+QtWITq7BxulHIE",
                room_id!("!SRstFdydzrGwJYtVfm:localhost"),
            ),
            (
                "HpJxQR57WbQGdY6w2Q+C16znVvbXGa+JvQdRoMpWbXg",
                room_id!("!SRstFdydzrGwJYtVfm:localhost"),
            ),
            (
                "Xetvi+ydFkZt8dpONGFbEusQb/Chc2V0XlLByZhsbgE",
                room_id!("!ZIwZcFqZVAYLAqVjfV:localhost"),
            ),
            (
                "wv/WN/39akyerIXczTaIpjAuLnwgXKRtbXFSEHiJqxo",
                room_id!("!ZIwZcFqZVAYLAqVjfV:localhost"),
            ),
            (
                "nA4gQwL//Cm8OdlyjABl/jChbPT/cP5V4Sd8iuE6H0s",
                room_id!("!ZIwZcFqZVAYLAqVjfV:localhost"),
            ),
            (
                "bAAgqFeRDTjfEqL6Qf/c9mk55zoNDCSlboAIRd6b0hw",
                room_id!("!ZIwZcFqZVAYLAqVjfV:localhost"),
            ),
            (
                "exPbsMMdGfAG2qmDdFtpAn+koVprfzS0Zip/RA9QRCE",
                room_id!("!ZIwZcFqZVAYLAqVjfV:localhost"),
            ),
            (
                "h+om7oSw/ZV94fcKaoe8FGXJwQXWOfKQfzbGgNWQILI",
                room_id!("!ZIwZcFqZVAYLAqVjfV:localhost"),
            ),
            (
                "ul3VXonpgk4lO2L3fEWubP/nxsTmLHqu5v8ZM9vHEcw",
                room_id!("!ZIwZcFqZVAYLAqVjfV:localhost"),
            ),
            (
                "JXY15UxC3az2mwg8uX4qwgxfvCM4aygiIWMcdNiVQoc",
                room_id!("!ZIwZcFqZVAYLAqVjfV:localhost"),
            ),
            (
                "OGB9lObr9kWUvha9tB5sMfOF/Mztk24JwQz/nwg3iFQ",
                room_id!("!OgRiTRMaUzLdpCeDBM:localhost"),
            ),
            (
                "SFkHcbxjUOYF7mUAYI/oEMDZFaXszQbCN6Jza7iemj0",
                room_id!("!OgRiTRMaUzLdpCeDBM:localhost"),
            ),
        ];

        // ensure we can load them all
        for (session_id, room_id) in &known_inbound_group_sessions {
            database
                .get_inbound_group_session(room_id, session_id)
                .await
                .expect("Should be able to load inbound group session")
                .unwrap();
        }

        let bob_sender_verified = database
            .get_inbound_group_session(
                room_id!("!ZIwZcFqZVAYLAqVjfV:localhost"),
                "exPbsMMdGfAG2qmDdFtpAn+koVprfzS0Zip/RA9QRCE",
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(bob_sender_verified.sender_data.to_type(), SenderDataType::SenderVerified);
        assert!(bob_sender_verified.backed_up());
        assert!(!bob_sender_verified.has_been_imported());

        let alice_unknown_device = database
            .get_inbound_group_session(
                room_id!("!SRstFdydzrGwJYtVfm:localhost"),
                "IrydwXkRk2N2AqUMIVmLL3oJgMq14R9KId0P/uSD100",
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(alice_unknown_device.sender_data.to_type(), SenderDataType::UnknownDevice);
        assert!(alice_unknown_device.backed_up());
        assert!(alice_unknown_device.has_been_imported());

        let carl_tofu_session = database
            .get_inbound_group_session(
                room_id!("!OgRiTRMaUzLdpCeDBM:localhost"),
                "OGB9lObr9kWUvha9tB5sMfOF/Mztk24JwQz/nwg3iFQ",
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(carl_tofu_session.sender_data.to_type(), SenderDataType::SenderUnverified);
        assert!(carl_tofu_session.backed_up());
        assert!(!carl_tofu_session.has_been_imported());

        // Load outbound sessions
        database
            .get_outbound_group_session(room_id!("!OgRiTRMaUzLdpCeDBM:localhost"))
            .await
            .unwrap()
            .unwrap();
        database
            .get_outbound_group_session(room_id!("!ZIwZcFqZVAYLAqVjfV:localhost"))
            .await
            .unwrap()
            .unwrap();
        database
            .get_outbound_group_session(room_id!("!SRstFdydzrGwJYtVfm:localhost"))
            .await
            .unwrap()
            .unwrap();

        let withheld_info = database
            .get_withheld_info(
                room_id!("!OgRiTRMaUzLdpCeDBM:localhost"),
                "SASgZ+EklvAF4QxJclMlDRlmL0fAMjAJJIKFMdb4Ht0",
            )
            .await
            .expect("This session should be withheld")
            .unwrap();

        assert_eq!(withheld_info.content.withheld_code(), WithheldCode::Unverified);

        let backup_keys = database.load_backup_keys().await.expect("backup key should be cached");
        assert_eq!(backup_keys.backup_version.unwrap(), "6");
        assert!(backup_keys.decryption_key.is_some());
    }

    /// Test that we migrate the secrets inbox properly.
    ///
    /// The format for the secrets inbox changed in version 17.  Previously, the
    /// secrets inbox stored a full `GossippedSecrets` struct.  In version 17,
    /// the secrets inbox now stores only the secret.
    #[async_test]
    async fn test_secrets_inbox_migration() {
        use std::ops::Deref;

        use matrix_sdk_crypto::{
            GossipRequest, GossippedSecret, SecretInfo,
            types::events::{
                olm_v1::{DecryptedSecretSendEvent, OlmV1Keys},
                secret_send::SecretSendContent,
            },
            vodozemac::Ed25519SecretKey,
        };
        use ruma::{TransactionId, events::secret::request::SecretName, owned_user_id};

        use crate::utils::{EncryptableStore, SqliteAsyncConnExt};

        // Create a database with version 16
        let tmpdir = tempdir().unwrap();
        let config = SqliteStoreConfig::new(tmpdir.path());
        let pool = config.build_pool_of_connections(super::DATABASE_NAME).unwrap();
        let conn = pool.get().await.unwrap();
        let version = super::initialize_store(&conn, 0).await.unwrap();
        let old_data_store = SqliteCryptoStore::create_raw(
            config.secret.clone(),
            pool,
            conn,
            config.pool_config(),
            config.runtime_config(),
        )
        .await
        .unwrap();
        super::run_migrations(&old_data_store, version, Some(16)).await.unwrap();
        old_data_store.write().await.unwrap().wal_checkpoint().await;

        // Store a secret using the old format
        let secret = GossippedSecret {
            secret_name: SecretName::CrossSigningMasterKey,
            gossip_request: GossipRequest {
                request_recipient: owned_user_id!("@alice:example.com"),
                request_id: TransactionId::new(),
                info: SecretInfo::SecretRequest(SecretName::CrossSigningMasterKey),
                sent_out: true,
            },
            event: DecryptedSecretSendEvent {
                sender: owned_user_id!("@alice:example.com"),
                recipient: owned_user_id!("@alice:example.com"),
                keys: OlmV1Keys { ed25519: Ed25519SecretKey::new().public_key() },
                recipient_keys: OlmV1Keys { ed25519: Ed25519SecretKey::new().public_key() },
                sender_device_keys: None,
                content: SecretSendContent::new(
                    "abc".into(),
                    "It is a secret to everybody".to_owned(),
                ),
            },
        };
        let value = old_data_store.serialize_json(&secret).unwrap();
        old_data_store
            .write()
            .await
            .unwrap()
            .prepare("INSERT INTO secrets (secret_name, data) VALUES (?1, ?2)", |mut stmt| {
                stmt.execute((SecretName::CrossSigningMasterKey.to_string(), value))
            })
            .await
            .unwrap();

        // After we open the store, the data will be migrated
        let store = SqliteCryptoStore::open_with_config(&config).await.unwrap();

        // and we should be able to read the secrets from the inbox
        let secrets =
            store.get_secrets_from_inbox(&SecretName::CrossSigningMasterKey).await.unwrap();
        assert_eq!(secrets.len(), 1);
        assert_eq!(secrets[0].deref(), "It is a secret to everybody");
    }

    /// Test that we migrate the key requests table properly.
    ///
    /// Version 18 added a new column, with a unique index on the column,
    /// meaning that it can now only store one request per requested secret/key.
    /// Test that when we migrate from an older version that has multiple
    /// requests for the same secret, it only keeps one.
    #[async_test]
    async fn test_key_requests_migration() {
        use matrix_sdk_crypto::{GossipRequest, SecretInfo};
        use ruma::{TransactionId, events::secret::request::SecretName, owned_user_id};

        use crate::utils::{EncryptableStore, SqliteAsyncConnExt};

        // Create a database with version 16
        let tmpdir = tempdir().unwrap();
        let config = SqliteStoreConfig::new(tmpdir.path());
        let pool = config.build_pool_of_connections(super::DATABASE_NAME).unwrap();
        let conn = pool.get().await.unwrap();
        let version = super::initialize_store(&conn, 0).await.unwrap();
        let old_data_store = SqliteCryptoStore::create_raw(
            config.secret.clone(),
            pool,
            conn,
            config.pool_config(),
            config.runtime_config(),
        )
        .await
        .unwrap();
        super::run_migrations(&old_data_store, version, Some(16)).await.unwrap();
        old_data_store.write().await.unwrap().wal_checkpoint().await;

        // Store a secret using the old format
        let recovery_request1 = GossipRequest {
            request_recipient: owned_user_id!("@alice:example.com"),
            request_id: TransactionId::new(),
            info: SecretInfo::SecretRequest(SecretName::RecoveryKey),
            sent_out: true,
        };
        let serialized_recovery_request1 =
            old_data_store.serialize_value(&recovery_request1).unwrap();
        let recovery_request2 = GossipRequest {
            request_recipient: owned_user_id!("@alice:example.com"),
            request_id: TransactionId::new(),
            info: SecretInfo::SecretRequest(SecretName::RecoveryKey),
            sent_out: true,
        };
        let serialized_recovery_request2 =
            old_data_store.serialize_value(&recovery_request2).unwrap();
        let msk_request = GossipRequest {
            request_recipient: owned_user_id!("@alice:example.com"),
            request_id: TransactionId::new(),
            info: SecretInfo::SecretRequest(SecretName::CrossSigningMasterKey),
            sent_out: true,
        };
        let serialized_msk_request = old_data_store.serialize_value(&msk_request).unwrap();
        let recovery_request1_clone = recovery_request1.clone();
        let recovery_request2_clone = recovery_request2.clone();
        let msk_request_clone = msk_request.clone();
        old_data_store
            .write()
            .await
            .unwrap()
            .prepare(
                "INSERT INTO key_requests (request_id, sent_out, data) VALUES (?1, ?2, ?3)",
                move |mut stmt| {
                    stmt.execute((
                        old_data_store.encode_key(
                            "key_requests",
                            recovery_request1_clone.request_id.as_bytes(),
                        ),
                        recovery_request1_clone.sent_out,
                        serialized_recovery_request1,
                    ))?;
                    stmt.execute((
                        old_data_store.encode_key(
                            "key_requests",
                            recovery_request2_clone.request_id.as_bytes(),
                        ),
                        recovery_request2_clone.sent_out,
                        serialized_recovery_request2,
                    ))?;
                    stmt.execute((
                        old_data_store
                            .encode_key("key_requests", msk_request_clone.request_id.as_bytes()),
                        msk_request_clone.sent_out,
                        serialized_msk_request,
                    ))
                },
            )
            .await
            .unwrap();

        // After we open the store, the data will be migrated
        let store = SqliteCryptoStore::open_with_config(&config).await.unwrap();

        // and we should be able to read one request for the recovery key and
        // one request for the MSK
        if let Some(GossipRequest {
            request_id,
            info: SecretInfo::SecretRequest(SecretName::RecoveryKey),
            ..
        }) = store
            .get_secret_request_by_info(&SecretInfo::SecretRequest(SecretName::RecoveryKey))
            .await
            .unwrap()
        {
            if request_id == recovery_request1.request_id {
                assert!(
                    store
                        .get_outgoing_secret_requests(&recovery_request2.request_id)
                        .await
                        .unwrap()
                        .is_none()
                );
            } else if request_id == recovery_request2.request_id {
                assert!(
                    store
                        .get_outgoing_secret_requests(&recovery_request1.request_id)
                        .await
                        .unwrap()
                        .is_none()
                );
            } else {
                panic!("unexpected record found");
            }
        } else {
            panic!("expected to get a secret request");
        }
        if let Some(GossipRequest {
            request_id,
            info: SecretInfo::SecretRequest(SecretName::CrossSigningMasterKey),
            ..
        }) = store
            .get_secret_request_by_info(&SecretInfo::SecretRequest(
                SecretName::CrossSigningMasterKey,
            ))
            .await
            .unwrap()
        {
            assert_eq!(request_id, msk_request.request_id);
        } else {
            panic!("expected to get a secret request");
        }
    }

    async fn get_store(
        name: &str,
        passphrase: Option<&str>,
        clear_data: bool,
    ) -> SqliteCryptoStore {
        let tmpdir_path = TMP_DIR.path().join(name);

        if clear_data {
            let _ = fs::remove_dir_all(&tmpdir_path).await;
        }

        SqliteCryptoStore::open(tmpdir_path.to_str().unwrap(), passphrase)
            .await
            .expect("Can't create a secret protected store")
    }

    cryptostore_integration_tests!();
    cryptostore_integration_tests_time!();
}

#[cfg(test)]
mod encrypted_tests {
    use std::sync::LazyLock;

    use matrix_sdk_crypto::{cryptostore_integration_tests, cryptostore_integration_tests_time};
    use tempfile::{TempDir, tempdir};
    use tokio::fs;

    use super::SqliteCryptoStore;

    static TMP_DIR: LazyLock<TempDir> = LazyLock::new(|| tempdir().unwrap());

    async fn get_store(
        name: &str,
        passphrase: Option<&str>,
        clear_data: bool,
    ) -> SqliteCryptoStore {
        let tmpdir_path = TMP_DIR.path().join(name);
        let pass = passphrase.unwrap_or("default_test_password");

        if clear_data {
            let _ = fs::remove_dir_all(&tmpdir_path).await;
        }

        SqliteCryptoStore::open(tmpdir_path.to_str().unwrap(), Some(pass))
            .await
            .expect("Can't create a secret protected store")
    }

    cryptostore_integration_tests!();
    cryptostore_integration_tests_time!();
}

#[cfg(test)]
mod close_reopen_tests {
    use std::sync::LazyLock;

    use matrix_sdk_crypto::store::CryptoStore;
    use matrix_sdk_test::async_test;
    use tempfile::{TempDir, tempdir};

    use super::SqliteCryptoStore;

    static TMP_DIR: LazyLock<TempDir> = LazyLock::new(|| tempdir().unwrap());

    async fn new_store(name: &str) -> SqliteCryptoStore {
        let tmpdir_path = TMP_DIR.path().join(name);
        SqliteCryptoStore::open(tmpdir_path, None).await.unwrap()
    }

    #[async_test]
    async fn test_close_completes_without_timeout() {
        let store = new_store("close_no_timeout").await;

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
        let store = new_store("reopen_restores").await;

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
        let store = new_store("close_idempotent").await;

        store.close().await.unwrap();
        // Second close should be a no-op.
        store.close().await.unwrap();

        let guard = store.connections.lock().await;
        assert!(guard.is_none());
    }

    #[async_test]
    async fn test_reopen_is_idempotent() {
        let store = new_store("reopen_idempotent").await;

        // Reopen on an active store should be a no-op.
        store.reopen().await.unwrap();

        let guard = store.connections.lock().await;
        assert!(guard.is_some());
    }

    #[async_test]
    async fn test_read_fails_when_closed() {
        let store = new_store("read_fails_closed").await;
        store.close().await.unwrap();

        let err = store.load_account().await;
        assert!(err.is_err(), "read should fail when closed");

        let err_msg = err.unwrap_err().to_string();
        assert!(err_msg.contains("closed"), "error should mention 'closed', got: {err_msg}");
    }

    #[async_test]
    async fn test_operations_work_after_reopen() {
        let store = new_store("ops_after_reopen").await;

        store.close().await.unwrap();
        store.reopen().await.unwrap();

        // A read operation should work immediately after reopen.
        let account = store.load_account().await;
        assert!(account.is_ok(), "load_account should succeed after reopen");
        // No account was saved, so this should be None.
        assert!(account.unwrap().is_none());
    }

    #[async_test]
    async fn test_multiple_close_reopen_cycles() {
        let store = new_store("multi_cycles").await;

        for _ in 0..5 {
            store.close().await.unwrap();
            store.reopen().await.unwrap();

            // After each cycle, the store should be fully operational.
            let account = store.load_account().await;
            assert!(account.is_ok(), "store should work after close/reopen cycle");
        }
    }

    #[async_test]
    async fn test_pool_is_fully_drained_after_close() {
        let store = new_store("pool_drained").await;

        // Do a few reads to exercise the pool.
        let _ = store.load_account().await;
        let _ = store.load_account().await;

        store.close().await.unwrap();

        // After close, the connections field should be None.
        let guard = store.connections.lock().await;
        assert!(guard.is_none(), "all connections should be released after close");
    }

    #[async_test]
    async fn test_close_waits_for_held_read_connection_to_drain() {
        let store = new_store("held_read_drain").await;

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
