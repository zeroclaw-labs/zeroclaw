//! Custom wa-rs storage backend using ZeroClaw's rusqlite

#[cfg(feature = "whatsapp-web")]
use async_trait::async_trait;
#[cfg(feature = "whatsapp-web")]
use parking_lot::Mutex;
#[cfg(feature = "whatsapp-web")]
use rusqlite::{Connection, params};
#[cfg(feature = "whatsapp-web")]
use std::path::Path;
#[cfg(feature = "whatsapp-web")]
use std::sync::Arc;

#[cfg(feature = "whatsapp-web")]
use bytes::Bytes;
#[cfg(feature = "whatsapp-web")]
use wacore::appstate::hash::HashState;
#[cfg(feature = "whatsapp-web")]
use wacore::appstate::processor::AppStateMutationMAC;
#[cfg(feature = "whatsapp-web")]
use wacore::store::Device as CoreDevice;
#[cfg(feature = "whatsapp-web")]
use wacore::store::traits::DeviceInfo;
#[cfg(feature = "whatsapp-web")]
use wacore::store::traits::DeviceStore as DeviceStoreTrait;
#[cfg(feature = "whatsapp-web")]
use wacore::store::traits::*;
#[cfg(feature = "whatsapp-web")]
// waproto 0.7 generates buffa messages, not prost; `encode_to_vec`/`decode`
// come from buffa's `Message` trait, re-exported by waproto.
use waproto::buffa::Message;

#[cfg(feature = "whatsapp-web")]
#[derive(Clone)]
pub struct RusqliteStore {
    /// Database file path
    db_path: String,
    /// SQLite connection (thread-safe via Mutex)
    conn: Arc<Mutex<Connection>>,
    /// Device ID for this session
    device_id: i32,
}

macro_rules! to_store_err {
    // For expressions returning Result<usize, E>
    (execute: $expr:expr) => {
        $expr.map(|_| ()).map_err(|e| {
            wacore::store::error::StoreError::Database(
                Box::new(e) as Box<dyn std::error::Error + Send + Sync>
            )
        })
    };
    // For other expressions
    ($expr:expr) => {
        $expr.map_err(|e| {
            wacore::store::error::StoreError::Database(
                Box::new(e) as Box<dyn std::error::Error + Send + Sync>
            )
        })
    };
}

/// Device ID used for single-session stores; every store created by
/// [`RusqliteStore::new`] persists its device under this row id.
#[cfg(feature = "whatsapp-web")]
const DEFAULT_DEVICE_ID: i32 = 1;

/// Used by [`DeviceStoreTrait::exists`]: "does the store hold a device row
/// to load?" — true as soon as the store is initialized, even before any
/// account is linked (the background saver persists the fresh, unregistered
/// device while the QR pairing is still pending).
#[cfg(feature = "whatsapp-web")]
const DEVICE_EXISTS_SQL: &str = "SELECT COUNT(*) FROM device WHERE id = ?1";

/// Used by [`persisted_device_exists`]: "does the store hold a *linked*
/// device?" — `pn` (the account JID) is NULL until pairing completes and is
/// only written by a successful login, so it distinguishes a linked session
/// from the unregistered row a starting channel persists pre-pairing.
#[cfg(feature = "whatsapp-web")]
const LINKED_DEVICE_EXISTS_SQL: &str =
    "SELECT COUNT(*) FROM device WHERE id = ?1 AND pn IS NOT NULL";

/// Channel-owned persisted-login probe for readiness reporting.
///
/// Reports whether the session database holds a device linked to a WhatsApp
/// account — a device row whose `pn` records the account JID written when a
/// QR pairing completes. Deliberately stricter than the store's `exists()`:
/// a channel that is running but still waiting for its QR scan has already
/// persisted an unregistered device row, and that must not read as
/// "authenticated". Usable without constructing a [`RusqliteStore`]: the
/// database is opened read-only and nothing is created on disk, so probing
/// an unpaired channel leaves no trace. Every negative case (file absent,
/// schema not yet initialized, no linked device row, unreadable database)
/// reports `false`.
#[cfg(feature = "whatsapp-web")]
pub fn persisted_device_exists<P: AsRef<Path>>(db_path: P) -> bool {
    let path = db_path.as_ref();
    if !path.is_file() {
        return false;
    }
    let Ok(conn) = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return false;
    };
    conn.query_row(
        LINKED_DEVICE_EXISTS_SQL,
        params![DEFAULT_DEVICE_ID],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
    .unwrap_or(false)
}

#[cfg(feature = "whatsapp-web")]
impl RusqliteStore {
    pub fn new<P: AsRef<Path>>(db_path: P) -> anyhow::Result<Self> {
        let db_path = db_path.as_ref().to_string_lossy().to_string();

        // Create parent directory if needed
        if let Some(parent) = Path::new(&db_path).parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&db_path)?;

        // Enable WAL mode for better concurrency
        to_store_err!(conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        ))?;

        let store = Self {
            db_path,
            conn: Arc::new(Mutex::new(conn)),
            device_id: DEFAULT_DEVICE_ID,
        };

        store.init_schema()?;

        Ok(store)
    }

    /// Initialize all database tables
    fn init_schema(&self) -> anyhow::Result<()> {
        let mut conn = self.conn.lock();

        let needs_raw_id = {
            let mut stmt = conn.prepare("PRAGMA table_info(device_registry)")?;
            let mut has_raw_id = false;
            let mut table_exists = false;
            let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
            for r in rows {
                table_exists = true;
                if r? == "raw_id" {
                    has_raw_id = true;
                    break;
                }
            }
            table_exists && !has_raw_id
        };

        let device_migrations: Vec<(&'static str, &'static str)> = {
            let mut existing: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut stmt = conn.prepare("PRAGMA table_info(device)")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
            for r in rows {
                existing.insert(r?);
            }
            const ALL: &[(&str, &str)] = &[
                ("next_pre_key_id", "INTEGER NOT NULL DEFAULT 0"),
                ("first_unupload_pre_key_id", "INTEGER NOT NULL DEFAULT 0"),
                ("server_has_prekeys", "INTEGER NOT NULL DEFAULT 0"),
                ("nct_salt", "BLOB"),
                ("server_cert_chain", "BLOB"),
                ("login_counter", "INTEGER NOT NULL DEFAULT 0"),
                ("lid_migrated", "INTEGER NOT NULL DEFAULT 0"),
                (
                    "last_signed_pre_key_rotation_ms",
                    "INTEGER NOT NULL DEFAULT 0",
                ),
                ("read_receipts_disabled", "INTEGER NOT NULL DEFAULT 0"),
            ];
            // If the table doesn't exist yet (existing is empty), the
            // CREATE TABLE inside the transaction will define every column,
            // so we want an empty migration list. The same
            // empty-set check that `needs_raw_id` relies on applies here.
            if existing.is_empty() {
                Vec::new()
            } else {
                ALL.iter()
                    .copied()
                    .filter(|(col, _)| !existing.contains(*col))
                    .collect()
            }
        };

        let tx = to_store_err!(conn.transaction())?;

        to_store_err!(tx.execute_batch(
            "-- Main device table
            CREATE TABLE IF NOT EXISTS device (
                id INTEGER PRIMARY KEY,
                lid TEXT,
                pn TEXT,
                registration_id INTEGER NOT NULL,
                noise_key BLOB NOT NULL,
                identity_key BLOB NOT NULL,
                signed_pre_key BLOB NOT NULL,
                signed_pre_key_id INTEGER NOT NULL,
                signed_pre_key_signature BLOB NOT NULL,
                adv_secret_key BLOB NOT NULL,
                account BLOB,
                push_name TEXT NOT NULL,
                app_version_primary INTEGER NOT NULL,
                app_version_secondary INTEGER NOT NULL,
                app_version_tertiary INTEGER NOT NULL,
                app_version_last_fetched_ms INTEGER NOT NULL,
                edge_routing_info BLOB,
                props_hash TEXT,
                next_pre_key_id INTEGER NOT NULL DEFAULT 0,
                first_unupload_pre_key_id INTEGER NOT NULL DEFAULT 0,
                server_has_prekeys INTEGER NOT NULL DEFAULT 0,
                nct_salt BLOB,
                server_cert_chain BLOB,
                login_counter INTEGER NOT NULL DEFAULT 0,
                lid_migrated INTEGER NOT NULL DEFAULT 0,
                last_signed_pre_key_rotation_ms INTEGER NOT NULL DEFAULT 0,
                read_receipts_disabled INTEGER NOT NULL DEFAULT 0
            );

            -- Signal identity keys
            CREATE TABLE IF NOT EXISTS identities (
                address TEXT NOT NULL,
                key BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (address, device_id)
            );

            -- Signal protocol sessions
            CREATE TABLE IF NOT EXISTS sessions (
                address TEXT NOT NULL,
                record BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (address, device_id)
            );

            -- Pre-keys for key exchange
            CREATE TABLE IF NOT EXISTS prekeys (
                id INTEGER NOT NULL,
                key BLOB NOT NULL,
                uploaded INTEGER NOT NULL DEFAULT 0,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (id, device_id)
            );

            -- Signed pre-keys
            CREATE TABLE IF NOT EXISTS signed_prekeys (
                id INTEGER NOT NULL,
                record BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (id, device_id)
            );

            -- Sender keys for group messaging
            CREATE TABLE IF NOT EXISTS sender_keys (
                address TEXT NOT NULL,
                record BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (address, device_id)
            );

            -- App state sync keys
            CREATE TABLE IF NOT EXISTS app_state_keys (
                key_id BLOB NOT NULL,
                key_data BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (key_id, device_id)
            );

            -- App state versions
            CREATE TABLE IF NOT EXISTS app_state_versions (
                name TEXT NOT NULL,
                state_data BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (name, device_id)
            );

            -- App state mutation MACs
            CREATE TABLE IF NOT EXISTS app_state_mutation_macs (
                name TEXT NOT NULL,
                version INTEGER NOT NULL,
                index_mac BLOB NOT NULL,
                value_mac BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (name, index_mac, device_id)
            );

            -- LID to phone number mapping
            CREATE TABLE IF NOT EXISTS lid_pn_mapping (
                lid TEXT NOT NULL,
                phone_number TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                learning_source TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (lid, device_id)
            );

            -- SKDM recipients tracking
            CREATE TABLE IF NOT EXISTS skdm_recipients (
                group_jid TEXT NOT NULL,
                device_jid TEXT NOT NULL,
                device_id INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (group_jid, device_jid, device_id)
            );

            -- Device registry for multi-device
            -- `raw_id` (NULL on legacy rows) is the ADV identity index added
            -- in wacore 0.6 — used to detect identity changes that require
            -- full session/sender-key invalidation per WA Web parity.
            CREATE TABLE IF NOT EXISTS device_registry (
                user_id TEXT NOT NULL,
                devices_json TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                phash TEXT,
                raw_id INTEGER,
                device_id INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (user_id, device_id)
            );

            -- Per-device sender-key tracking (wacore 0.6: replaces the
            -- skdm_recipients / sender_key_status pair). Each row records
            -- whether a known group device has a valid sender key (1) or
            -- needs a fresh SKDM (0).
            CREATE TABLE IF NOT EXISTS sender_key_devices (
                group_jid TEXT NOT NULL,
                device_jid TEXT NOT NULL,
                has_key INTEGER NOT NULL,
                device_id INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (group_jid, device_jid, device_id)
            );

            -- Sent message retry store (wacore 0.6: WA Web getMessageTable
            -- parity). Stores serialized payloads keyed by (chat, message_id)
            -- so retry-receipts can re-encrypt + resend the original message.
            CREATE TABLE IF NOT EXISTS sent_messages (
                chat_jid TEXT NOT NULL,
                message_id TEXT NOT NULL,
                payload BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (chat_jid, message_id, device_id)
            );

            -- MessageContextInfo.messageSecret keyed by the outbound message
            -- it belongs to. Needed to decrypt add-ons (reactions, edits,
            -- poll votes, msmsg bot replies) that reference one of our own
            -- sent messages. `expires_at` is an absolute unix-seconds
            -- retention deadline (0 = never); `message_ts` is the parent
            -- message's event time (0 = unknown), used to enforce the
            -- edit-processing window.
            CREATE TABLE IF NOT EXISTS msg_secrets (
                chat TEXT NOT NULL,
                sender TEXT NOT NULL,
                msg_id TEXT NOT NULL,
                secret BLOB NOT NULL,
                expires_at INTEGER NOT NULL DEFAULT 0,
                message_ts INTEGER NOT NULL DEFAULT 0,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (chat, sender, msg_id, device_id)
            );

            -- device_id first so the prune's `device_id = ? AND expires_at <= ?`
            -- range scan stays localized to one account in a shared DB.
            CREATE INDEX IF NOT EXISTS idx_msg_secrets_expires
                ON msg_secrets (device_id, expires_at);

            -- Base keys for collision detection
            CREATE TABLE IF NOT EXISTS base_keys (
                address TEXT NOT NULL,
                message_id TEXT NOT NULL,
                base_key BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (address, message_id, device_id)
            );

            -- Sender key status for lazy deletion
            CREATE TABLE IF NOT EXISTS sender_key_status (
                group_jid TEXT NOT NULL,
                participant TEXT NOT NULL,
                device_id INTEGER NOT NULL,
                marked_at INTEGER NOT NULL,
                PRIMARY KEY (group_jid, participant, device_id)
            );

            -- Trusted contact tokens
            CREATE TABLE IF NOT EXISTS tc_tokens (
                jid TEXT NOT NULL,
                token BLOB NOT NULL,
                token_timestamp INTEGER NOT NULL,
                sender_timestamp INTEGER,
                device_id INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (jid, device_id)
            );

            -- Index supporting `delete_expired_sent_messages`
            -- (WHERE device_id = ? AND created_at < ?). Without it the cleanup
            -- pass would full-scan `sent_messages`, which grows unbounded until
            -- the periodic cleanup hook lands. `IF NOT EXISTS` keeps re-init
            -- idempotent across restarts.
            CREATE INDEX IF NOT EXISTS idx_sent_messages_device_created
                ON sent_messages(device_id, created_at);",
        ))?;

        if needs_raw_id {
            to_store_err!(execute: tx.execute(
                "ALTER TABLE device_registry ADD COLUMN raw_id INTEGER",
                [],
            ))?;
        }

        for (col, ty) in &device_migrations {
            to_store_err!(execute: tx.execute(
                &format!("ALTER TABLE device ADD COLUMN {col} {ty}"),
                [],
            ))?;
        }

        to_store_err!(tx.commit())?;
        Ok(())
    }
}

#[cfg(feature = "whatsapp-web")]
#[async_trait]
impl SignalStore for RusqliteStore {
    // --- Identity Operations ---

    async fn put_identity(&self, address: &str, key: [u8; 32]) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "INSERT OR REPLACE INTO identities (address, key, device_id)
             VALUES (?1, ?2, ?3)",
            params![address, key.to_vec(), self.device_id],
        ))
    }

    async fn load_identity(&self, address: &str) -> wacore::store::error::Result<Option<[u8; 32]>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT key FROM identities WHERE address = ?1 AND device_id = ?2",
            params![address, self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        );

        match result {
            Ok(key) => {
                if key.len() != 32 {
                    return Err(wacore::store::error::StoreError::Validation(format!(
                        "identity key has invalid length {}, expected 32",
                        key.len()
                    )));
                }
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&key);
                Ok(Some(arr))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    async fn delete_identity(&self, address: &str) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "DELETE FROM identities WHERE address = ?1 AND device_id = ?2",
            params![address, self.device_id],
        ))
    }

    // --- Session Operations ---

    async fn get_session(&self, address: &str) -> wacore::store::error::Result<Option<Bytes>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT record FROM sessions WHERE address = ?1 AND device_id = ?2",
            params![address, self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        );

        match result {
            Ok(record) => Ok(Some(Bytes::from(record))),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    async fn put_session(&self, address: &str, session: &[u8]) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "INSERT OR REPLACE INTO sessions (address, record, device_id)
             VALUES (?1, ?2, ?3)",
            params![address, session, self.device_id],
        ))
    }

    async fn delete_session(&self, address: &str) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "DELETE FROM sessions WHERE address = ?1 AND device_id = ?2",
            params![address, self.device_id],
        ))
    }

    // --- PreKey Operations ---

    async fn store_prekey(
        &self,
        id: u32,
        record: &[u8],
        uploaded: bool,
    ) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "INSERT OR REPLACE INTO prekeys (id, key, uploaded, device_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, record, uploaded, self.device_id],
        ))
    }

    async fn load_prekey(&self, id: u32) -> wacore::store::error::Result<Option<Bytes>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT key FROM prekeys WHERE id = ?1 AND device_id = ?2",
            params![id, self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        );

        match result {
            Ok(key) => Ok(Some(Bytes::from(key))),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    /// Get the maximum pre-key ID currently stored, or 0 if none exist.
    /// Added in wacore 0.6: used for migrating `next_pre_key_id` counter when
    /// initializing fresh devices that share storage with the legacy schema.
    async fn get_max_prekey_id(&self) -> wacore::store::error::Result<u32> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT MAX(id) FROM prekeys WHERE device_id = ?1",
            params![self.device_id],
            |row| row.get::<_, Option<i64>>(0),
        );

        match result {
            // MAX returns NULL on empty table → Some(None); on non-empty → Some(Some(n))
            Ok(Some(id)) => Ok(u32::try_from(id).unwrap_or(0)),
            Ok(None) => Ok(0),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(0),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    async fn remove_prekey(&self, id: u32) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "DELETE FROM prekeys WHERE id = ?1 AND device_id = ?2",
            params![id, self.device_id],
        ))
    }

    /// Flag the pre-keys in `ids` as uploaded to the server.
    ///
    /// UPDATE-only by contract: a key consumed (and deleted) between the
    /// upload snapshot and this call must stay deleted, so an upsert here
    /// would resurrect a spent key and hand it out twice.
    async fn mark_prekeys_uploaded(&self, ids: &[u32]) -> wacore::store::error::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }

        let conn = self.conn.lock();
        for id in ids {
            to_store_err!(execute: conn.execute(
                "UPDATE prekeys SET uploaded = 1 WHERE id = ?1 AND device_id = ?2",
                params![id, self.device_id],
            ))?;
        }
        Ok(())
    }

    // --- Signed PreKey Operations ---

    async fn store_signed_prekey(
        &self,
        id: u32,
        record: &[u8],
    ) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "INSERT OR REPLACE INTO signed_prekeys (id, record, device_id)
             VALUES (?1, ?2, ?3)",
            params![id, record, self.device_id],
        ))
    }

    async fn load_signed_prekey(&self, id: u32) -> wacore::store::error::Result<Option<Vec<u8>>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT record FROM signed_prekeys WHERE id = ?1 AND device_id = ?2",
            params![id, self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    async fn load_all_signed_prekeys(&self) -> wacore::store::error::Result<Vec<(u32, Vec<u8>)>> {
        let conn = self.conn.lock();
        let mut stmt = to_store_err!(
            conn.prepare("SELECT id, record FROM signed_prekeys WHERE device_id = ?1")
        )?;

        let rows = to_store_err!(stmt.query_map(params![self.device_id], |row| {
            Ok((row.get::<_, u32>(0)?, row.get::<_, Vec<u8>>(1)?))
        }))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(to_store_err!(row)?);
        }

        Ok(result)
    }

    async fn remove_signed_prekey(&self, id: u32) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "DELETE FROM signed_prekeys WHERE id = ?1 AND device_id = ?2",
            params![id, self.device_id],
        ))
    }

    // --- Sender Key Operations ---

    async fn put_sender_key(
        &self,
        address: &str,
        record: &[u8],
    ) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "INSERT OR REPLACE INTO sender_keys (address, record, device_id)
             VALUES (?1, ?2, ?3)",
            params![address, record, self.device_id],
        ))
    }

    async fn get_sender_key(&self, address: &str) -> wacore::store::error::Result<Option<Vec<u8>>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT record FROM sender_keys WHERE address = ?1 AND device_id = ?2",
            params![address, self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    async fn delete_sender_key(&self, address: &str) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "DELETE FROM sender_keys WHERE address = ?1 AND device_id = ?2",
            params![address, self.device_id],
        ))
    }
}

#[cfg(feature = "whatsapp-web")]
#[async_trait]
impl AppSyncStore for RusqliteStore {
    async fn get_sync_key(
        &self,
        key_id: &[u8],
    ) -> wacore::store::error::Result<Option<AppStateSyncKey>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT key_data FROM app_state_keys WHERE key_id = ?1 AND device_id = ?2",
            params![key_id, self.device_id],
            |row| {
                let key_data: Vec<u8> = row.get(0)?;
                serde_json::from_slice(&key_data)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
            },
        );

        match result {
            Ok(key) => Ok(Some(key)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    async fn set_sync_key(
        &self,
        key_id: &[u8],
        key: AppStateSyncKey,
    ) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        let key_data = to_store_err!(serde_json::to_vec(&key))?;

        to_store_err!(execute: conn.execute(
            "INSERT OR REPLACE INTO app_state_keys (key_id, key_data, device_id)
             VALUES (?1, ?2, ?3)",
            params![key_id, key_data, self.device_id],
        ))
    }

    async fn get_version(&self, name: &str) -> wacore::store::error::Result<HashState> {
        let conn = self.conn.lock();
        // No stored version yet (fresh app-state table) means version 0, not an
        // error — matches InMemoryStore and the diesel SqliteStore. Surfacing an
        // error here breaks the critical app-state sync on first pairing.
        match conn.query_row(
            "SELECT state_data FROM app_state_versions WHERE name = ?1 AND device_id = ?2",
            params![name, self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        ) {
            Ok(state_data) => to_store_err!(serde_json::from_slice(&state_data)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(HashState::default()),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    async fn set_version(&self, name: &str, state: HashState) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        let state_data = to_store_err!(serde_json::to_vec(&state))?;

        to_store_err!(execute: conn.execute(
            "INSERT OR REPLACE INTO app_state_versions (name, state_data, device_id)
             VALUES (?1, ?2, ?3)",
            params![name, state_data, self.device_id],
        ))
    }

    async fn put_mutation_macs(
        &self,
        name: &str,
        version: u64,
        mutations: &[AppStateMutationMAC],
    ) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();

        for mutation in mutations {
            to_store_err!(execute: conn.execute(
                "INSERT OR REPLACE INTO app_state_mutation_macs
                 (name, version, index_mac, value_mac, device_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![name, i64::try_from(version).unwrap_or(i64::MAX), mutation.index_mac, mutation.value_mac, self.device_id],
            ))?;
        }

        Ok(())
    }

    async fn get_mutation_mac(
        &self,
        name: &str,
        index_mac: &[u8],
    ) -> wacore::store::error::Result<Option<Vec<u8>>> {
        let conn = self.conn.lock();

        let result = conn.query_row(
            "SELECT value_mac FROM app_state_mutation_macs
             WHERE name = ?1 AND index_mac = ?2 AND device_id = ?3",
            params![name, index_mac, self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        );

        match result {
            Ok(mac) => Ok(Some(mac)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    async fn delete_mutation_macs(
        &self,
        name: &str,
        index_macs: &[Vec<u8>],
    ) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();

        for index_mac in index_macs {
            to_store_err!(execute: conn.execute(
                "DELETE FROM app_state_mutation_macs
                 WHERE name = ?1 AND index_mac = ?2 AND device_id = ?3",
                params![name, index_mac, self.device_id],
            ))?;
        }

        Ok(())
    }

    /// Drop every mutation MAC for one collection.
    ///
    /// Called on snapshot re-sync so the MAC store is rebuilt from the
    /// snapshot and stays consistent with the ltHash baseline; a leftover
    /// entry would corrupt the next patch's ltHash.
    async fn clear_mutation_macs(&self, name: &str) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "DELETE FROM app_state_mutation_macs WHERE name = ?1 AND device_id = ?2",
            params![name, self.device_id],
        ))
    }

    /// Get the most recently stored app state sync key ID.
    /// Added in wacore 0.6: used to seed app-state sync requests with the
    /// freshest key identifier we hold rather than scanning the table on each
    /// request.
    async fn get_latest_sync_key_id(&self) -> wacore::store::error::Result<Option<Vec<u8>>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT key_id FROM app_state_keys
             WHERE device_id = ?1
             ORDER BY key_id DESC
             LIMIT 1",
            params![self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        );

        match result {
            Ok(key_id) => Ok(Some(key_id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }
}

#[cfg(feature = "whatsapp-web")]
#[async_trait]
impl MsgSecretStore for RusqliteStore {
    /// Batched upsert. On key conflict the merge is deterministic and matches
    /// `wacore::store::traits::merge_msg_secret_expiry` /
    /// `merge_msg_secret_message_ts`: the later deadline wins with `0`
    /// ("never") beating any finite one, so a redelivery or edit re-persist
    /// can never shorten a retention window; and the later non-zero parent
    /// timestamp wins, so an unknown `0` never clobbers a known one.
    async fn put_msg_secrets(
        &self,
        entries: Vec<MsgSecretEntry>,
    ) -> wacore::store::error::Result<usize> {
        if entries.is_empty() {
            return Ok(0);
        }

        let mut conn = self.conn.lock();
        let tx = to_store_err!(conn.transaction())?;

        let mut stored = 0usize;
        for entry in &entries {
            // Plain variant, not `execute:` — that one discards the row
            // count, and the trait contract returns how many were stored.
            stored += to_store_err!(tx.execute(
                "INSERT INTO msg_secrets
                     (chat, sender, msg_id, secret, expires_at, message_ts, device_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(chat, sender, msg_id, device_id) DO UPDATE SET
                     secret = excluded.secret,
                     expires_at = CASE
                         WHEN msg_secrets.expires_at = 0 OR excluded.expires_at = 0 THEN 0
                         ELSE MAX(msg_secrets.expires_at, excluded.expires_at)
                     END,
                     message_ts = MAX(msg_secrets.message_ts, excluded.message_ts)",
                params![
                    entry.chat,
                    entry.sender,
                    entry.msg_id,
                    entry.secret,
                    entry.expires_at,
                    entry.message_ts,
                    self.device_id,
                ],
            ))?;
        }

        to_store_err!(tx.commit())?;
        Ok(stored)
    }

    async fn get_msg_secret(
        &self,
        chat: &str,
        sender: &str,
        msg_id: &str,
    ) -> wacore::store::error::Result<Option<Vec<u8>>> {
        Ok(self
            .get_msg_secret_with_ts(chat, sender, msg_id)
            .await?
            .map(|(secret, _)| secret))
    }

    /// Overridden rather than left to the trait default: this backend does
    /// persist `message_ts`, so the receive path gets the real parent event
    /// time instead of the default's `0`, and can enforce the edit window.
    async fn get_msg_secret_with_ts(
        &self,
        chat: &str,
        sender: &str,
        msg_id: &str,
    ) -> wacore::store::error::Result<Option<(Vec<u8>, i64)>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT secret, message_ts FROM msg_secrets
             WHERE chat = ?1 AND sender = ?2 AND msg_id = ?3 AND device_id = ?4",
            params![chat, sender, msg_id, self.device_id],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
        );

        match result {
            Ok(found) => Ok(Some(found)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    /// Prune rows whose deadline has passed. `expires_at = 0` means "never",
    /// so those rows are kept regardless of the cutoff.
    async fn delete_expired_msg_secrets(
        &self,
        cutoff_timestamp: i64,
    ) -> wacore::store::error::Result<u32> {
        let conn = self.conn.lock();
        // Plain variant, not `execute:` — the caller throttles its cleanup
        // sweep on how many rows this actually removed.
        let deleted = to_store_err!(conn.execute(
            "DELETE FROM msg_secrets
             WHERE expires_at != 0 AND expires_at <= ?1 AND device_id = ?2",
            params![cutoff_timestamp, self.device_id],
        ))?;
        Ok(deleted as u32)
    }
}

#[cfg(feature = "whatsapp-web")]
#[async_trait]
impl ProtocolStore for RusqliteStore {
    async fn get_sender_key_devices(
        &self,
        group_jid: &str,
    ) -> wacore::store::error::Result<Vec<(String, bool)>> {
        let conn = self.conn.lock();
        let mut stmt = to_store_err!(conn.prepare(
            "SELECT device_jid, has_key FROM sender_key_devices
             WHERE group_jid = ?1 AND device_id = ?2"
        ))?;

        let rows = to_store_err!(stmt.query_map(params![group_jid, self.device_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        }))?;

        let mut result = Vec::new();
        for row in rows {
            let (device_jid, has_key) = to_store_err!(row)?;
            result.push((device_jid, has_key != 0));
        }

        Ok(result)
    }

    async fn set_sender_key_status(
        &self,
        group_jid: &str,
        entries: &[(&str, bool)],
    ) -> wacore::store::error::Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock();
        let now = chrono::Utc::now().timestamp();

        // Wrap the per-entry upserts in a transaction so a panic or connection
        // drop mid-batch can't leave some (group, device) pairs flipped and
        // others not — partial state would silently break SKDM resend logic.
        let tx = to_store_err!(conn.transaction())?;

        for (device_jid, has_key) in entries {
            to_store_err!(execute: tx.execute(
                "INSERT INTO sender_key_devices
                 (group_jid, device_jid, has_key, device_id, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(group_jid, device_jid, device_id) DO UPDATE SET
                   has_key = excluded.has_key,
                   updated_at = excluded.updated_at",
                params![
                    group_jid,
                    device_jid,
                    if *has_key { 1_i64 } else { 0_i64 },
                    self.device_id,
                    now,
                ],
            ))?;
        }

        to_store_err!(tx.commit())?;
        Ok(())
    }

    async fn clear_sender_key_devices(&self, group_jid: &str) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "DELETE FROM sender_key_devices WHERE group_jid = ?1 AND device_id = ?2",
            params![group_jid, self.device_id],
        ))
    }

    async fn delete_sender_key_device_rows(
        &self,
        device_jids: &[&str],
    ) -> wacore::store::error::Result<()> {
        if device_jids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock();
        for device_jid in device_jids {
            to_store_err!(execute: conn.execute(
                "DELETE FROM sender_key_devices
                 WHERE device_jid = ?1 AND device_id = ?2",
                params![device_jid, self.device_id],
            ))?;
        }
        Ok(())
    }

    async fn clear_all_sender_key_devices(&self) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "DELETE FROM sender_key_devices WHERE device_id = ?1",
            params![self.device_id],
        ))
    }

    // --- LID-PN Mapping ---

    async fn get_lid_mapping(
        &self,
        lid: &str,
    ) -> wacore::store::error::Result<Option<LidPnMappingEntry>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT lid, phone_number, created_at, learning_source, updated_at
             FROM lid_pn_mapping WHERE lid = ?1 AND device_id = ?2",
            params![lid, self.device_id],
            |row| {
                Ok(LidPnMappingEntry {
                    lid: row.get(0)?,
                    phone_number: row.get(1)?,
                    created_at: row.get(2)?,
                    learning_source: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            },
        );

        match result {
            Ok(entry) => Ok(Some(entry)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    async fn get_pn_mapping(
        &self,
        phone: &str,
    ) -> wacore::store::error::Result<Option<LidPnMappingEntry>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT lid, phone_number, created_at, learning_source, updated_at
             FROM lid_pn_mapping WHERE phone_number = ?1 AND device_id = ?2
             ORDER BY updated_at DESC LIMIT 1",
            params![phone, self.device_id],
            |row| {
                Ok(LidPnMappingEntry {
                    lid: row.get(0)?,
                    phone_number: row.get(1)?,
                    created_at: row.get(2)?,
                    learning_source: row.get(3)?,
                    updated_at: row.get(4)?,
                })
            },
        );

        match result {
            Ok(entry) => Ok(Some(entry)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    async fn put_lid_mapping(&self, entry: &LidPnMappingEntry) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "INSERT OR REPLACE INTO lid_pn_mapping
             (lid, phone_number, created_at, learning_source, updated_at, device_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                entry.lid,
                entry.phone_number,
                entry.created_at,
                entry.learning_source,
                entry.updated_at,
                self.device_id,
            ],
        ))
    }

    async fn get_all_lid_mappings(&self) -> wacore::store::error::Result<Vec<LidPnMappingEntry>> {
        let conn = self.conn.lock();
        let mut stmt = to_store_err!(conn.prepare(
            "SELECT lid, phone_number, created_at, learning_source, updated_at
             FROM lid_pn_mapping WHERE device_id = ?1"
        ))?;

        let rows = to_store_err!(stmt.query_map(params![self.device_id], |row| {
            Ok(LidPnMappingEntry {
                lid: row.get(0)?,
                phone_number: row.get(1)?,
                created_at: row.get(2)?,
                learning_source: row.get(3)?,
                updated_at: row.get(4)?,
            })
        }))?;

        let mut result = Vec::new();
        for row in rows {
            result.push(to_store_err!(row)?);
        }

        Ok(result)
    }

    // --- Base Key Collision Detection ---

    async fn save_base_key(
        &self,
        address: &str,
        message_id: &str,
        base_key: &[u8],
    ) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().timestamp();

        to_store_err!(execute: conn.execute(
            "INSERT OR REPLACE INTO base_keys (address, message_id, base_key, device_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![address, message_id, base_key, self.device_id, now],
        ))
    }

    async fn has_same_base_key(
        &self,
        address: &str,
        message_id: &str,
        current_base_key: &[u8],
    ) -> wacore::store::error::Result<bool> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT base_key FROM base_keys
             WHERE address = ?1 AND message_id = ?2 AND device_id = ?3",
            params![address, message_id, self.device_id],
            |row| {
                let saved_key: Vec<u8> = row.get(0)?;
                Ok(saved_key == current_base_key)
            },
        );

        match result {
            Ok(same) => Ok(same),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    async fn delete_base_key(
        &self,
        address: &str,
        message_id: &str,
    ) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "DELETE FROM base_keys WHERE address = ?1 AND message_id = ?2 AND device_id = ?3",
            params![address, message_id, self.device_id],
        ))
    }

    // --- Device Registry ---

    async fn update_device_list(
        &self,
        record: DeviceListRecord,
    ) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        let devices_json = to_store_err!(serde_json::to_string(&record.devices))?;
        let now = chrono::Utc::now().timestamp();

        // raw_id is a wacore 0.6 addition for ADV identity-change detection.
        // Stored as nullable INTEGER on the new `raw_id` column added by the
        // schema migration; older rows with a NULL value behave as if no
        // raw_id was ever recorded for the user (matching upstream behavior).
        to_store_err!(execute: conn.execute(
            "INSERT OR REPLACE INTO device_registry
             (user_id, devices_json, timestamp, phash, raw_id, device_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                record.user,
                devices_json,
                record.timestamp,
                record.phash,
                record.raw_id.map(|r| r as i64),
                self.device_id,
                now,
            ],
        ))
    }

    async fn get_devices(
        &self,
        user: &str,
    ) -> wacore::store::error::Result<Option<DeviceListRecord>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT user_id, devices_json, timestamp, phash, raw_id
             FROM device_registry WHERE user_id = ?1 AND device_id = ?2",
            params![user, self.device_id],
            |row| {
                // Helper to convert errors to rusqlite::Error
                fn to_rusqlite_err<E: std::error::Error + Send + Sync + 'static>(
                    e: E,
                ) -> rusqlite::Error {
                    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
                }

                let devices_json: String = row.get(1)?;
                let devices: Vec<DeviceInfo> =
                    serde_json::from_str(&devices_json).map_err(to_rusqlite_err)?;
                let raw_id: Option<i64> = row.get(4)?;
                Ok(DeviceListRecord {
                    user: row.get(0)?,
                    devices,
                    timestamp: row.get(2)?,
                    phash: row.get(3)?,
                    raw_id: raw_id.map(|r| r as u32),
                })
            },
        );

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    /// Delete a device list record, forcing a network re-fetch on next query.
    /// Added in wacore 0.6.
    async fn delete_devices(&self, user: &str) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "DELETE FROM device_registry WHERE user_id = ?1 AND device_id = ?2",
            params![user, self.device_id],
        ))
    }

    // --- TcToken Storage ---

    async fn get_tc_token(&self, jid: &str) -> wacore::store::error::Result<Option<TcTokenEntry>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT token, token_timestamp, sender_timestamp FROM tc_tokens
             WHERE jid = ?1 AND device_id = ?2",
            params![jid, self.device_id],
            |row| {
                Ok(TcTokenEntry {
                    token: row.get(0)?,
                    token_timestamp: row.get(1)?,
                    sender_timestamp: row.get(2)?,
                })
            },
        );

        match result {
            Ok(entry) => Ok(Some(entry)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    async fn put_tc_token(
        &self,
        jid: &str,
        entry: &TcTokenEntry,
    ) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().timestamp();

        to_store_err!(execute: conn.execute(
            "INSERT OR REPLACE INTO tc_tokens
             (jid, token, token_timestamp, sender_timestamp, device_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                jid,
                entry.token,
                entry.token_timestamp,
                entry.sender_timestamp,
                self.device_id,
                now,
            ],
        ))
    }

    /// Advance only the sender-side bucket, atomically preserving a received
    /// token written by the concurrent privacy-notification path.
    async fn touch_tc_token_sender_timestamp(
        &self,
        jid: &str,
        sender_timestamp: i64,
    ) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().timestamp();
        to_store_err!(execute: conn.execute(
            "INSERT INTO tc_tokens
                 (jid, token, token_timestamp, sender_timestamp, device_id, updated_at)
             VALUES (?1, X'', ?2, ?2, ?3, ?4)
             ON CONFLICT(jid, device_id) DO UPDATE SET
                 sender_timestamp = CASE
                     WHEN tc_tokens.sender_timestamp IS NULL
                         THEN excluded.sender_timestamp
                     ELSE MAX(tc_tokens.sender_timestamp, excluded.sender_timestamp)
                 END,
                 updated_at = excluded.updated_at",
            params![jid, sender_timestamp, self.device_id, now],
        ))
    }

    /// Store the newer received token in one statement while leaving the
    /// sender-side bucket untouched.
    async fn store_received_tc_token(
        &self,
        jid: &str,
        token: &[u8],
        token_timestamp: i64,
    ) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().timestamp();
        to_store_err!(execute: conn.execute(
            "INSERT INTO tc_tokens
                 (jid, token, token_timestamp, sender_timestamp, device_id, updated_at)
             VALUES (?1, ?2, ?3, NULL, ?4, ?5)
             ON CONFLICT(jid, device_id) DO UPDATE SET
                 token = excluded.token,
                 token_timestamp = excluded.token_timestamp,
                 updated_at = excluded.updated_at
             WHERE length(tc_tokens.token) = 0
                OR excluded.token_timestamp >= tc_tokens.token_timestamp",
            params![jid, token, token_timestamp, self.device_id, now],
        ))
    }

    async fn delete_tc_token(&self, jid: &str) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        to_store_err!(execute: conn.execute(
            "DELETE FROM tc_tokens WHERE jid = ?1 AND device_id = ?2",
            params![jid, self.device_id],
        ))
    }

    async fn get_all_tc_token_jids(&self) -> wacore::store::error::Result<Vec<String>> {
        let conn = self.conn.lock();
        let mut stmt =
            to_store_err!(conn.prepare("SELECT jid FROM tc_tokens WHERE device_id = ?1"))?;

        let rows = to_store_err!(
            stmt.query_map(params![self.device_id], |row| { row.get::<_, String>(0) })
        )?;

        let mut result = Vec::new();
        for row in rows {
            result.push(to_store_err!(row)?);
        }

        Ok(result)
    }

    async fn delete_expired_tc_tokens(
        &self,
        token_cutoff: i64,
        sender_cutoff: i64,
    ) -> wacore::store::error::Result<u32> {
        let conn = self.conn.lock();
        // Both halves must be dead before the row goes. Recent sender state is
        // never dropped just because the received token expired, so the two
        // cutoffs are ANDed rather than either one deleting the row alone. An
        // empty token counts as absent, as does a NULL sender bucket.
        let deleted = conn
            .execute(
                "DELETE FROM tc_tokens
                  WHERE device_id = ?3
                    AND (token_timestamp < ?1 OR length(token) = 0)
                    AND (sender_timestamp IS NULL OR sender_timestamp < ?2)",
                params![token_cutoff, sender_cutoff, self.device_id],
            )
            .map_err(|e| {
                wacore::store::error::StoreError::Database(
                    Box::new(e) as Box<dyn std::error::Error + Send + Sync>
                )
            })?;

        let deleted = u32::try_from(deleted).map_err(|_| {
            wacore::store::error::StoreError::Validation(format!(
                "Affected row count overflowed u32: {deleted}"
            ))
        })?;

        Ok(deleted)
    }

    async fn store_sent_message(
        &self,
        chat_jid: &str,
        message_id: &str,
        payload: &[u8],
    ) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();
        let now = chrono::Utc::now().timestamp();
        to_store_err!(execute: conn.execute(
            "INSERT OR REPLACE INTO sent_messages
             (chat_jid, message_id, payload, device_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![chat_jid, message_id, payload, self.device_id, now],
        ))
    }

    async fn take_sent_message(
        &self,
        chat_jid: &str,
        message_id: &str,
    ) -> wacore::store::error::Result<Option<Vec<u8>>> {
        let mut conn = self.conn.lock();
        // Atomic SELECT+DELETE under an immediate transaction matches upstream's
        // SqliteStore::take_sent_message: prevents two concurrent retry-receipts
        // from each consuming and re-encrypting the same payload.
        let tx = to_store_err!(conn.transaction())?;

        let payload: Option<Vec<u8>> = match tx.query_row(
            "SELECT payload FROM sent_messages
             WHERE chat_jid = ?1 AND message_id = ?2 AND device_id = ?3",
            params![chat_jid, message_id, self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        ) {
            Ok(p) => Some(p),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => {
                return Err(wacore::store::error::StoreError::Database(Box::new(e)));
            }
        };

        if payload.is_some() {
            to_store_err!(execute: tx.execute(
                "DELETE FROM sent_messages
                 WHERE chat_jid = ?1 AND message_id = ?2 AND device_id = ?3",
                params![chat_jid, message_id, self.device_id],
            ))?;
        }

        to_store_err!(tx.commit())?;
        Ok(payload)
    }

    /// Delete sent messages older than `cutoff_timestamp` (unix seconds).
    /// TODO(wacore-0.6): wire to a periodic cleanup cron in the daemon. The
    /// current implementation is correct but the table will grow unbounded
    /// until the cron is hooked up.
    async fn delete_expired_sent_messages(
        &self,
        cutoff_timestamp: i64,
    ) -> wacore::store::error::Result<u32> {
        let conn = self.conn.lock();
        let deleted = conn
            .execute(
                "DELETE FROM sent_messages WHERE created_at < ?1 AND device_id = ?2",
                params![cutoff_timestamp, self.device_id],
            )
            .map_err(|e| {
                wacore::store::error::StoreError::Database(
                    Box::new(e) as Box<dyn std::error::Error + Send + Sync>
                )
            })?;
        u32::try_from(deleted).map_err(|_| {
            wacore::store::error::StoreError::Validation(format!(
                "Affected row count overflowed u32: {deleted}"
            ))
        })
    }
}

#[cfg(feature = "whatsapp-web")]
#[async_trait]
impl DeviceStoreTrait for RusqliteStore {
    async fn save(&self, device: &CoreDevice) -> wacore::store::error::Result<()> {
        let conn = self.conn.lock();

        // Serialize KeyPairs to bytes
        let noise_key = {
            let mut bytes = Vec::new();
            let priv_key = device.noise_key.private_key.serialize();
            bytes.extend_from_slice(priv_key.as_slice());
            bytes.extend_from_slice(device.noise_key.public_key.public_key_bytes());
            bytes
        };

        let identity_key = {
            let mut bytes = Vec::new();
            let priv_key = device.identity_key.private_key.serialize();
            bytes.extend_from_slice(priv_key.as_slice());
            bytes.extend_from_slice(device.identity_key.public_key.public_key_bytes());
            bytes
        };

        let signed_pre_key = {
            let mut bytes = Vec::new();
            let priv_key = device.signed_pre_key.private_key.serialize();
            bytes.extend_from_slice(priv_key.as_slice());
            bytes.extend_from_slice(device.signed_pre_key.public_key.public_key_bytes());
            bytes
        };

        // Safety: device account data is stored to DB only; to_store_err! converts
        // rusqlite errors without logging parameter values.
        let account = device.account.as_ref().map(|a| a.as_ref().encode_to_vec());

        let server_cert_chain_blob = device
            .server_cert_chain
            .as_ref()
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|e| wacore::store::error::StoreError::Serialization(Box::new(e)))?;

        to_store_err!(execute: conn.execute(
            "INSERT OR REPLACE INTO device (
                id, lid, pn, registration_id, noise_key, identity_key,
                signed_pre_key, signed_pre_key_id, signed_pre_key_signature,
                adv_secret_key, account, push_name, app_version_primary,
                app_version_secondary, app_version_tertiary, app_version_last_fetched_ms,
                edge_routing_info, props_hash,
                next_pre_key_id, first_unupload_pre_key_id,
                server_has_prekeys, nct_salt, server_cert_chain,
                login_counter, lid_migrated,
                last_signed_pre_key_rotation_ms, read_receipts_disabled
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27
            )",
            params![
                self.device_id,
                device.lid.as_ref().map(|j| j.to_string()),
                device.pn.as_ref().map(|j| j.to_string()),
                device.registration_id,
                noise_key,
                identity_key,
                signed_pre_key,
                device.signed_pre_key_id,
                device.signed_pre_key_signature.to_vec(),
                device.adv_secret_key.to_vec(),
                account,
                &device.push_name,
                device.app_version_primary,
                device.app_version_secondary,
                device.app_version_tertiary,
                device.app_version_last_fetched_ms,
                device.edge_routing_info.clone(),
                device.props_hash.clone(),
                device.next_pre_key_id,
                device.first_unupload_pre_key_id,
                device.server_has_prekeys as i64,
                device.nct_salt.clone(),
                server_cert_chain_blob,
                device.login_counter,
                device.lid_migrated as i64,
                device.last_signed_pre_key_rotation_ms,
                device.read_receipts_disabled as i64,
            ],
        ))
    }

    async fn load(&self) -> wacore::store::error::Result<Option<CoreDevice>> {
        let conn = self.conn.lock();
        let result = conn.query_row(
            "SELECT * FROM device WHERE id = ?1",
            params![self.device_id],
            |row| {
                // Helper to convert errors to rusqlite::Error
                fn to_rusqlite_err<E: std::error::Error + Send + Sync + 'static>(
                    e: E,
                ) -> rusqlite::Error {
                    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
                }

                fn fixed_blob<const N: usize>(
                    name: &str,
                    bytes: Vec<u8>,
                ) -> Result<[u8; N], rusqlite::Error> {
                    if bytes.len() != N {
                        return Err(rusqlite::Error::FromSqlConversionFailure(
                            0,
                            rusqlite::types::Type::Blob,
                            Box::new(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("{name} must be {N} bytes, got {}", bytes.len()),
                            )),
                        ));
                    }

                    let mut value = [0u8; N];
                    value.copy_from_slice(&bytes);
                    Ok(value)
                }

                use wacore::libsignal::protocol::{KeyPair, PrivateKey, PublicKey};

                // Deserialize KeyPairs from fixed-size blobs.
                let noise_key_bytes: [u8; 64] = fixed_blob("noise_key", row.get("noise_key")?)?;
                let identity_key_bytes: [u8; 64] =
                    fixed_blob("identity_key", row.get("identity_key")?)?;
                let signed_pre_key_bytes: [u8; 64] =
                    fixed_blob("signed_pre_key", row.get("signed_pre_key")?)?;

                let noise_key = KeyPair::new(
                    PublicKey::from_djb_public_key_bytes(&noise_key_bytes[32..64])
                        .map_err(to_rusqlite_err)?,
                    PrivateKey::deserialize(&noise_key_bytes[0..32]).map_err(to_rusqlite_err)?,
                );

                let identity_key = KeyPair::new(
                    PublicKey::from_djb_public_key_bytes(&identity_key_bytes[32..64])
                        .map_err(to_rusqlite_err)?,
                    PrivateKey::deserialize(&identity_key_bytes[0..32]).map_err(to_rusqlite_err)?,
                );

                let signed_pre_key = KeyPair::new(
                    PublicKey::from_djb_public_key_bytes(&signed_pre_key_bytes[32..64])
                        .map_err(to_rusqlite_err)?,
                    PrivateKey::deserialize(&signed_pre_key_bytes[0..32])
                        .map_err(to_rusqlite_err)?,
                );

                let lid_str: Option<String> = row.get("lid")?;
                let pn_str: Option<String> = row.get("pn")?;
                let signature = fixed_blob::<64>(
                    "signed_pre_key_signature",
                    row.get("signed_pre_key_signature")?,
                )?;
                let adv_secret = fixed_blob::<32>("adv_secret_key", row.get("adv_secret_key")?)?;
                let account_bytes: Option<Vec<u8>> = row.get("account")?;

                let account = if let Some(bytes) = account_bytes {
                    // buffa's `decode` wants `&mut impl Buf`; `decode_from_slice`
                    // is the convenience form. 0.7 also stores the account behind
                    // an `Arc` on `Device`.
                    Some(std::sync::Arc::new(
                        waproto::whatsapp::ADVSignedDeviceIdentity::decode_from_slice(&bytes)
                            .map_err(to_rusqlite_err)?,
                    ))
                } else {
                    None
                };

                let server_cert_chain: Option<wacore::store::device::CachedServerCertChain> = {
                    let bytes: Option<Vec<u8>> = row.get("server_cert_chain")?;
                    match bytes {
                        Some(b) => Some(serde_json::from_slice(&b).map_err(to_rusqlite_err)?),
                        None => None,
                    }
                };
                let server_has_prekeys_int: i64 = row.get("server_has_prekeys")?;
                let lid_migrated_int: i64 = row.get("lid_migrated")?;
                let read_receipts_disabled_int: i64 = row.get("read_receipts_disabled")?;

                Ok(CoreDevice {
                    lid: lid_str.and_then(|s| s.parse().ok()),
                    pn: pn_str.and_then(|s| s.parse().ok()),
                    registration_id: row.get("registration_id")?,
                    noise_key,
                    identity_key,
                    signed_pre_key,
                    signed_pre_key_id: row.get("signed_pre_key_id")?,
                    signed_pre_key_signature: signature,
                    adv_secret_key: adv_secret,
                    account,
                    push_name: row.get("push_name")?,
                    app_version_primary: row.get("app_version_primary")?,
                    app_version_secondary: row.get("app_version_secondary")?,
                    app_version_tertiary: row.get("app_version_tertiary")?,
                    app_version_last_fetched_ms: row.get("app_version_last_fetched_ms")?,
                    edge_routing_info: row.get("edge_routing_info")?,
                    props_hash: row.get("props_hash")?,
                    next_pre_key_id: row.get("next_pre_key_id")?,
                    first_unupload_pre_key_id: row.get("first_unupload_pre_key_id")?,
                    server_has_prekeys: server_has_prekeys_int != 0,
                    nct_salt: row.get("nct_salt")?,
                    server_cert_chain,
                    login_counter: row.get("login_counter")?,
                    lid_migrated: lid_migrated_int != 0,
                    last_signed_pre_key_rotation_ms: row.get("last_signed_pre_key_rotation_ms")?,
                    read_receipts_disabled: read_receipts_disabled_int != 0,
                    ..Default::default()
                })
            },
        );

        match result {
            Ok(device) => Ok(Some(device)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(wacore::store::error::StoreError::Database(Box::new(e))),
        }
    }

    async fn exists(&self) -> wacore::store::error::Result<bool> {
        let conn = self.conn.lock();
        let count: i64 =
            to_store_err!(
                conn.query_row(DEVICE_EXISTS_SQL, params![self.device_id], |row| row.get(0),)
            )?;

        Ok(count > 0)
    }

    async fn create(&self) -> wacore::store::error::Result<i32> {
        // Device already created in constructor, just return the ID
        Ok(self.device_id)
    }

    async fn snapshot_db(
        &self,
        name: &str,
        extra_content: Option<&[u8]>,
    ) -> wacore::store::error::Result<()> {
        let snapshot_path = format!("{}.snapshot.{}", self.db_path, name);
        let content_path = format!("{}.extra", snapshot_path);

        // Serialize replacement and sidecar creation with the database copy so
        // concurrent requests cannot pair artifacts from different snapshots.
        let conn = self.conn.lock();
        for path in [&snapshot_path, &content_path] {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(wacore::store::error::StoreError::Database(Box::new(error)));
                }
            }
        }
        // VACUUM INTO reads through SQLite, so committed pages still in the
        // WAL are included in the standalone snapshot.
        to_store_err!(execute: conn.execute("VACUUM INTO ?1", params![snapshot_path.as_str()]))?;

        // If extra_content is provided, save it alongside.
        if let Some(content) = extra_content {
            to_store_err!(std::fs::write(&content_path, content))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "whatsapp-web")]
    use wacore::store::traits::{LidPnMappingEntry, ProtocolStore, TcTokenEntry};

    #[cfg(feature = "whatsapp-web")]
    #[test]
    fn rusqlite_store_creates_database() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = RusqliteStore::new(tmp.path()).unwrap();
        assert_eq!(store.device_id, 1);
    }

    #[cfg(feature = "whatsapp-web")]
    fn msg_secret(msg_id: &str, expires_at: i64, message_ts: i64) -> MsgSecretEntry {
        // 0.7 narrows these: the JID/id fields are `Arc<str>` and the secret is
        // a fixed `[u8; MESSAGE_SECRET_SIZE]` rather than a `Vec<u8>`.
        MsgSecretEntry {
            chat: "chat@s.whatsapp.net".into(),
            sender: "sender@s.whatsapp.net".into(),
            msg_id: msg_id.into(),
            secret: [7u8; 32],
            expires_at,
            message_ts,
        }
    }

    /// A redelivery or edit re-persist must never shorten a retention window,
    /// and `0` ("never") must beat any finite deadline in either direction.
    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn msg_secret_upsert_never_shortens_the_retention_window() {
        let tmp = tempfile::tempdir().unwrap();
        let store = RusqliteStore::new(tmp.path().join("session.db")).unwrap();

        store
            .put_msg_secrets(vec![msg_secret("ID", 1_000, 0)])
            .await
            .unwrap();

        // An earlier deadline loses to the one already stored.
        store
            .put_msg_secrets(vec![msg_secret("ID", 500, 0)])
            .await
            .unwrap();
        assert_eq!(
            store.delete_expired_msg_secrets(600).await.unwrap(),
            0,
            "a 500 deadline must not have replaced the stored 1000"
        );

        // 0 means never, so it wins over the stored finite deadline.
        store
            .put_msg_secrets(vec![msg_secret("ID", 0, 0)])
            .await
            .unwrap();
        assert_eq!(
            store.delete_expired_msg_secrets(i64::MAX).await.unwrap(),
            0,
            "a never-expires row must survive any cutoff"
        );
    }

    /// The parent message's event time is immutable; a later write that does
    /// not know it (`0`) must not erase the value already stored.
    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn msg_secret_upsert_keeps_a_known_parent_timestamp() {
        let tmp = tempfile::tempdir().unwrap();
        let store = RusqliteStore::new(tmp.path().join("session.db")).unwrap();

        store
            .put_msg_secrets(vec![msg_secret("ID", 0, 1_700_000_000)])
            .await
            .unwrap();
        store
            .put_msg_secrets(vec![msg_secret("ID", 0, 0)])
            .await
            .unwrap();

        let (secret, message_ts) = store
            .get_msg_secret_with_ts("chat@s.whatsapp.net", "sender@s.whatsapp.net", "ID")
            .await
            .unwrap()
            .expect("secret must still be present");
        assert_eq!(secret, vec![7u8; 32]);
        assert_eq!(
            message_ts, 1_700_000_000,
            "an unknown (0) parent time must not clobber a known one"
        );
    }

    /// Only deadlines that have actually passed are pruned; `0` never expires.
    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn expired_msg_secrets_prune_only_passed_deadlines() {
        let tmp = tempfile::tempdir().unwrap();
        let store = RusqliteStore::new(tmp.path().join("session.db")).unwrap();

        store
            .put_msg_secrets(vec![
                msg_secret("NEVER", 0, 0),
                msg_secret("PAST", 1_000, 0),
                msg_secret("FUTURE", 9_000, 0),
            ])
            .await
            .unwrap();

        assert_eq!(store.delete_expired_msg_secrets(5_000).await.unwrap(), 1);

        let chat = "chat@s.whatsapp.net";
        let sender = "sender@s.whatsapp.net";
        assert!(
            store
                .get_msg_secret(chat, sender, "NEVER")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .get_msg_secret(chat, sender, "FUTURE")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .get_msg_secret(chat, sender, "PAST")
                .await
                .unwrap()
                .is_none()
        );
    }

    /// `mark_prekeys_uploaded` is UPDATE-only by contract: a key consumed
    /// between the upload snapshot and the callback must stay deleted, or it
    /// would be handed out to a second peer.
    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn mark_prekeys_uploaded_never_resurrects_a_deleted_key() {
        let tmp = tempfile::tempdir().unwrap();
        let store = RusqliteStore::new(tmp.path().join("session.db")).unwrap();

        store.store_prekey(1, &[1u8; 32], false).await.unwrap();
        store.store_prekey(2, &[2u8; 32], false).await.unwrap();
        store.remove_prekey(2).await.unwrap();

        store.mark_prekeys_uploaded(&[1, 2]).await.unwrap();

        assert!(store.load_prekey(1).await.unwrap().is_some());
        assert!(
            store.load_prekey(2).await.unwrap().is_none(),
            "a consumed pre-key must not be resurrected by the uploaded flag"
        );
    }

    #[cfg(feature = "whatsapp-web")]
    #[test]
    fn persisted_device_probe_is_read_only() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("absent-session.db");

        assert!(!persisted_device_exists(&path));
        assert!(
            !path.exists(),
            "probing an absent session must not create the database"
        );
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn persisted_device_probe_requires_linked_account() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.db");
        let store = RusqliteStore::new(&path).unwrap();

        // Initialized schema, no device row at all: no session, no login.
        assert!(!DeviceStoreTrait::exists(&store).await.unwrap());
        assert!(!persisted_device_exists(&path));

        // The pre-pairing state a starting channel persists: a device row
        // exists (store `exists()` is true so the channel resumes its keys),
        // but no account is linked yet — the probe must NOT report a login.
        DeviceStoreTrait::save(&store, &CoreDevice::new())
            .await
            .unwrap();
        assert!(DeviceStoreTrait::exists(&store).await.unwrap());
        assert!(
            !persisted_device_exists(&path),
            "an unregistered device row (pn IS NULL) is not a persisted login"
        );

        // Pairing completes: the login writes the account JID into `pn`.
        let mut device = CoreDevice::new();
        device.pn = Some(wacore_binary::jid::Jid::pn("15551234567"));
        DeviceStoreTrait::save(&store, &device).await.unwrap();
        assert!(persisted_device_exists(&path));
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn device_load_rejects_malformed_fixed_blobs_without_unwinding() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = RusqliteStore::new(tmp.path()).unwrap();
        let device = CoreDevice::new();

        DeviceStoreTrait::save(&store, &device).await.unwrap();

        for (column, expected_len) in [
            ("noise_key", 64),
            ("identity_key", 64),
            ("signed_pre_key", 64),
            ("signed_pre_key_signature", 64),
            ("adv_secret_key", 32),
        ] {
            DeviceStoreTrait::save(&store, &device).await.unwrap();
            {
                let conn = store.conn.lock();
                conn.execute(
                    &format!("UPDATE device SET {column} = ?1 WHERE id = ?2"),
                    params![vec![0u8; expected_len - 1], store.device_id],
                )
                .unwrap();
            }

            let result = DeviceStoreTrait::load(&store).await;
            assert!(
                matches!(result, Err(wacore::store::error::StoreError::Database(_))),
                "malformed {column} should return a database error"
            );
        }
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn snapshot_db_includes_committed_wal_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.db");
        let store = RusqliteStore::new(&path).unwrap();

        {
            let conn = store.conn.lock();
            conn.execute_batch(
                "PRAGMA wal_autocheckpoint = 0;
                 CREATE TABLE snapshot_probe (value TEXT NOT NULL);",
            )
            .unwrap();
            conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
                .unwrap();
            conn.execute(
                "INSERT INTO snapshot_probe (value) VALUES (?1)",
                params!["wal-only"],
            )
            .unwrap();
        }

        // Copying the main file directly omits the committed row still held
        // in the live WAL, which is the failure mode this snapshot fixes.
        let main_only_path = tmp.path().join("main-only.db");
        std::fs::copy(&path, &main_only_path).unwrap();
        let main_only = Connection::open(&main_only_path).unwrap();
        let main_count: i64 = main_only
            .query_row("SELECT COUNT(*) FROM snapshot_probe", [], |row| row.get(0))
            .unwrap();
        assert_eq!(main_count, 0);

        DeviceStoreTrait::snapshot_db(&store, "wal", Some(b"extra"))
            .await
            .unwrap();

        let snapshot_path = format!("{}.snapshot.wal", path.to_string_lossy());
        {
            let snapshot = Connection::open(&snapshot_path).unwrap();
            let value: String = snapshot
                .query_row("SELECT value FROM snapshot_probe", [], |row| row.get(0))
                .unwrap();
            assert_eq!(value, "wal-only");
        }
        assert_eq!(
            std::fs::read(format!("{snapshot_path}.extra")).unwrap(),
            b"extra"
        );

        DeviceStoreTrait::snapshot_db(&store, "wal", None)
            .await
            .unwrap();
        assert!(
            !Path::new(&format!("{snapshot_path}.extra")).exists(),
            "snapshot without extra content must remove a stale sidecar"
        );
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn mutation_macs_round_trip_raw_bytes_and_clear_stays_scoped() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = RusqliteStore::new(tmp.path()).unwrap();

        // Bytes chosen so a JSON re-encoding would differ from the raw value
        // (NUL + high bytes). Guards against regressing to JSON-wrapped MACs:
        // `get_mutation_mac`'s result is fed verbatim into the app-state LTHash,
        // so a non-raw value corrupts the running hash (snapshot MAC mismatch).
        let index_mac = vec![0x00u8, 0x7f, 0x80, 0xff, 0x10, 0x22];
        let value_mac = vec![0xdeu8, 0xad, 0xbe, 0xef, 0x00, 0x99];
        let mac = AppStateMutationMAC {
            index_mac: index_mac.clone(),
            value_mac: value_mac.clone(),
        };

        AppSyncStore::put_mutation_macs(&store, "critical_block", 1, std::slice::from_ref(&mac))
            .await
            .unwrap();

        // Must return the raw value_mac verbatim, not a JSON encoding of it.
        let got = AppSyncStore::get_mutation_mac(&store, "critical_block", &index_mac)
            .await
            .unwrap();
        assert_eq!(got, Some(value_mac.clone()));

        // Unknown index → None.
        let missing = AppSyncStore::get_mutation_mac(&store, "critical_block", &[1, 2, 3])
            .await
            .unwrap();
        assert_eq!(missing, None);

        let other_index = vec![0x44, 0x55];
        let other_mac = AppStateMutationMAC {
            index_mac: other_index.clone(),
            value_mac: vec![0x66, 0x77],
        };
        AppSyncStore::put_mutation_macs(
            &store,
            "regular_high",
            1,
            std::slice::from_ref(&other_mac),
        )
        .await
        .unwrap();

        let other_device = RusqliteStore {
            device_id: 2,
            ..store.clone()
        };
        AppSyncStore::put_mutation_macs(
            &other_device,
            "critical_block",
            1,
            std::slice::from_ref(&mac),
        )
        .await
        .unwrap();

        AppSyncStore::clear_mutation_macs(&store, "critical_block")
            .await
            .unwrap();
        assert_eq!(
            AppSyncStore::get_mutation_mac(&store, "critical_block", &index_mac)
                .await
                .unwrap(),
            None,
            "the named collection must be cleared for this device"
        );
        assert_eq!(
            AppSyncStore::get_mutation_mac(&store, "regular_high", &other_index)
                .await
                .unwrap(),
            Some(other_mac.value_mac),
            "another collection on this device must survive"
        );
        assert_eq!(
            AppSyncStore::get_mutation_mac(&other_device, "critical_block", &index_mac)
                .await
                .unwrap(),
            Some(value_mac.clone()),
            "the same collection on another device must survive"
        );

        AppSyncStore::put_mutation_macs(&store, "critical_block", 2, &[mac])
            .await
            .unwrap();

        // Delete removes the entry.
        AppSyncStore::delete_mutation_macs(
            &store,
            "critical_block",
            std::slice::from_ref(&index_mac),
        )
        .await
        .unwrap();
        let after_delete = AppSyncStore::get_mutation_mac(&store, "critical_block", &index_mac)
            .await
            .unwrap();
        assert_eq!(after_delete, None);
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn lid_mapping_round_trip_preserves_learning_source_and_updated_at() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = RusqliteStore::new(tmp.path()).unwrap();
        let entry = LidPnMappingEntry {
            lid: "100000012345678".to_string(),
            phone_number: "15551234567".to_string(),
            created_at: 1_700_000_000,
            updated_at: 1_700_000_100,
            learning_source: "usync".to_string(),
        };

        ProtocolStore::put_lid_mapping(&store, &entry)
            .await
            .unwrap();

        let loaded = ProtocolStore::get_lid_mapping(&store, &entry.lid)
            .await
            .unwrap()
            .expect("expected lid mapping to be present");
        assert_eq!(loaded.learning_source, entry.learning_source);
        assert_eq!(loaded.updated_at, entry.updated_at);

        let loaded_by_pn = ProtocolStore::get_pn_mapping(&store, &entry.phone_number)
            .await
            .unwrap()
            .expect("expected pn mapping to be present");
        assert_eq!(loaded_by_pn.learning_source, entry.learning_source);
        assert_eq!(loaded_by_pn.updated_at, entry.updated_at);
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn delete_expired_tc_tokens_requires_both_halves_to_be_dead() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = RusqliteStore::new(tmp.path()).unwrap();

        let expired = TcTokenEntry {
            token: vec![1, 2, 3],
            token_timestamp: 10,
            sender_timestamp: None,
        };
        let fresh = TcTokenEntry {
            token: vec![4, 5, 6],
            token_timestamp: 1000,
            sender_timestamp: Some(1000),
        };
        let expired_token_live_sender = TcTokenEntry {
            token: vec![7, 8, 9],
            token_timestamp: 10,
            sender_timestamp: Some(1000),
        };
        let empty_token_expired_sender = TcTokenEntry {
            token: Vec::new(),
            token_timestamp: 1000,
            sender_timestamp: Some(10),
        };
        let live_token_expired_sender = TcTokenEntry {
            token: vec![10, 11, 12],
            token_timestamp: 1000,
            sender_timestamp: Some(10),
        };

        ProtocolStore::put_tc_token(&store, "15550000001", &expired)
            .await
            .unwrap();
        ProtocolStore::put_tc_token(&store, "15550000002", &fresh)
            .await
            .unwrap();
        ProtocolStore::put_tc_token(&store, "15550000003", &expired_token_live_sender)
            .await
            .unwrap();
        ProtocolStore::put_tc_token(&store, "15550000004", &empty_token_expired_sender)
            .await
            .unwrap();
        ProtocolStore::put_tc_token(&store, "15550000005", &live_token_expired_sender)
            .await
            .unwrap();

        let deleted = ProtocolStore::delete_expired_tc_tokens(&store, 100, 100)
            .await
            .unwrap();
        assert_eq!(deleted, 2);
        assert!(
            ProtocolStore::get_tc_token(&store, "15550000001")
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            ProtocolStore::get_tc_token(&store, "15550000002")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            ProtocolStore::get_tc_token(&store, "15550000003")
                .await
                .unwrap()
                .is_some(),
            "a live sender bucket must preserve an expired received token row"
        );
        assert!(
            ProtocolStore::get_tc_token(&store, "15550000004")
                .await
                .unwrap()
                .is_none(),
            "an empty token with an expired sender bucket has no live state"
        );
        assert!(
            ProtocolStore::get_tc_token(&store, "15550000005")
                .await
                .unwrap()
                .is_some(),
            "a live received token must preserve an expired sender bucket row"
        );
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn tc_token_writers_atomically_preserve_each_others_fields() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = RusqliteStore::new(tmp.path()).unwrap();
        const JID: &str = "15550000006";

        ProtocolStore::touch_tc_token_sender_timestamp(&store, JID, 5_000)
            .await
            .unwrap();
        ProtocolStore::store_received_tc_token(&store, JID, &[1, 2, 3], 4_000)
            .await
            .unwrap();
        ProtocolStore::touch_tc_token_sender_timestamp(&store, JID, 3_000)
            .await
            .unwrap();
        ProtocolStore::store_received_tc_token(&store, JID, &[9, 9, 9], 2_000)
            .await
            .unwrap();

        let merged = ProtocolStore::get_tc_token(&store, JID)
            .await
            .unwrap()
            .expect("both writer paths must converge on one row");
        assert_eq!(merged.token, vec![1, 2, 3]);
        assert_eq!(merged.token_timestamp, 4_000);
        assert_eq!(merged.sender_timestamp, Some(5_000));

        ProtocolStore::store_received_tc_token(&store, JID, &[4, 5, 6], 6_000)
            .await
            .unwrap();
        let newer = ProtocolStore::get_tc_token(&store, JID)
            .await
            .unwrap()
            .expect("the merged row must remain present");
        assert_eq!(newer.token, vec![4, 5, 6]);
        assert_eq!(newer.token_timestamp, 6_000);
        assert_eq!(newer.sender_timestamp, Some(5_000));
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn device_save_load_round_trips_all_persisted_wacore_07_fields() {
        use wacore::store::Device as CoreDevice;
        use wacore::store::device::{CachedNoiseCert, CachedServerCertChain};
        use wacore::store::traits::DeviceStore as DeviceStoreTrait;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        // First boot: populate every post-0.5 persisted device field with a
        // non-default value and persist it.
        {
            let store = RusqliteStore::new(&path).unwrap();
            let mut device = CoreDevice::new();
            device.next_pre_key_id = 42;
            device.first_unupload_pre_key_id = 17;
            device.server_has_prekeys = true;
            device.nct_salt = Some(vec![0xDE, 0xAD, 0xBE, 0xEF]);
            device.server_cert_chain = Some(CachedServerCertChain {
                intermediate: CachedNoiseCert {
                    key: [1u8; 32],
                    not_before: 1_700_000_000,
                    not_after: 1_800_000_000,
                },
                leaf: CachedNoiseCert {
                    key: [2u8; 32],
                    not_before: 1_700_000_000,
                    not_after: 1_800_000_000,
                },
            });
            device.login_counter = 7;
            device.lid_migrated = true;
            device.last_signed_pre_key_rotation_ms = 1_750_000_000_000;
            device.read_receipts_disabled = true;
            DeviceStoreTrait::save(&store, &device).await.unwrap();
        }

        // Second boot: reopen the on-disk database and confirm the
        // values survived the restart.
        let store = RusqliteStore::new(&path).unwrap();
        let loaded = DeviceStoreTrait::load(&store)
            .await
            .unwrap()
            .expect("device row should exist after save");

        assert_eq!(loaded.next_pre_key_id, 42);
        assert_eq!(loaded.first_unupload_pre_key_id, 17);
        assert!(loaded.server_has_prekeys);
        assert_eq!(
            loaded.nct_salt.as_deref(),
            Some(&[0xDE, 0xAD, 0xBE, 0xEF][..])
        );
        let cert = loaded
            .server_cert_chain
            .as_ref()
            .expect("server_cert_chain should round-trip");
        assert_eq!(cert.intermediate.key, [1u8; 32]);
        assert_eq!(cert.leaf.key, [2u8; 32]);
        assert_eq!(cert.intermediate.not_before, 1_700_000_000);
        assert_eq!(cert.leaf.not_after, 1_800_000_000);
        assert_eq!(loaded.login_counter, 7);
        assert!(loaded.lid_migrated);
        assert_eq!(loaded.last_signed_pre_key_rotation_ms, 1_750_000_000_000);
        assert!(loaded.read_receipts_disabled);
    }

    #[cfg(feature = "whatsapp-web")]
    #[tokio::test]
    async fn pre_06_device_table_gets_new_columns_on_open() {
        use wacore::store::Device as CoreDevice;
        use wacore::store::traits::DeviceStore as DeviceStoreTrait;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        // Hand-create a legacy pre-0.6 device table (18 columns, no
        // wacore-0.6 fields) to simulate an existing on-disk database
        // from a daemon that ran against whatsapp-rust 0.5.
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE device (
                    id INTEGER PRIMARY KEY,
                    lid TEXT,
                    pn TEXT,
                    registration_id INTEGER NOT NULL,
                    noise_key BLOB NOT NULL,
                    identity_key BLOB NOT NULL,
                    signed_pre_key BLOB NOT NULL,
                    signed_pre_key_id INTEGER NOT NULL,
                    signed_pre_key_signature BLOB NOT NULL,
                    adv_secret_key BLOB NOT NULL,
                    account BLOB,
                    push_name TEXT NOT NULL,
                    app_version_primary INTEGER NOT NULL,
                    app_version_secondary INTEGER NOT NULL,
                    app_version_tertiary INTEGER NOT NULL,
                    app_version_last_fetched_ms INTEGER NOT NULL,
                    edge_routing_info BLOB,
                    props_hash TEXT
                );",
            )
            .unwrap();
        }

        // Opening the store must add every post-0.5 column idempotently; a
        // subsequent save+load round-trip must succeed.
        let store = RusqliteStore::new(&path).unwrap();
        let mut device = CoreDevice::new();
        device.next_pre_key_id = 99;
        device.first_unupload_pre_key_id = 50;
        device.login_counter = 3;
        device.lid_migrated = true;
        device.last_signed_pre_key_rotation_ms = 1_750_000_000_000;
        device.read_receipts_disabled = true;
        DeviceStoreTrait::save(&store, &device).await.unwrap();

        let loaded = DeviceStoreTrait::load(&store)
            .await
            .unwrap()
            .expect("device row should exist after save");
        assert_eq!(loaded.next_pre_key_id, 99);
        assert_eq!(loaded.first_unupload_pre_key_id, 50);
        assert_eq!(loaded.login_counter, 3);
        assert!(loaded.lid_migrated);
        assert_eq!(loaded.last_signed_pre_key_rotation_ms, 1_750_000_000_000);
        assert!(loaded.read_receipts_disabled);

        // Re-opening a second time must be a no-op (idempotent ALTER).
        drop(store);
        let store2 = RusqliteStore::new(&path).unwrap();
        let loaded2 = DeviceStoreTrait::load(&store2).await.unwrap().unwrap();
        assert_eq!(loaded2.next_pre_key_id, 99);
    }
}
