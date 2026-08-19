//! Encrypted durable state for exact logical plugin instances.
//!
//! The SQLite file stores only opaque owner/row locators and `enc2:`
//! ciphertext. Package, capability, binding, logical key, revision, and value
//! have one source of truth inside the authenticated envelope. The opaque
//! columns are one-way derived indexes, not alternate copies of logical state.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use zeroclaw_config::secrets::SecretStore;
use zeroclaw_plugins::PluginCapability;
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::services::{
    PluginStateBackend, PluginStateError, PluginStateKey, PluginStateValue,
};
use zeroize::{Zeroize, Zeroizing};

const DATABASE_FILE: &str = "plugin-state.db";
const ENVELOPE_VERSION: u8 = 1;
const OWNER_DOMAIN: &[u8] = b"zeroclaw.plugin-state.owner.v1\0";
const LOCATOR_DOMAIN: &[u8] = b"zeroclaw.plugin-state.locator.v1\0";
/// Keyed tag over the package name alone, so `plugin remove` can purge every
/// row a package owns without enumerating its live capabilities and bindings,
/// and without storing the package name in plaintext.
const PACKAGE_DOMAIN: &[u8] = b"zeroclaw.plugin-state.package.v1\0";

/// Schema generation stamped in `PRAGMA user_version`.
///
/// * 1 — `owner`, `locator`, `ciphertext`.
/// * 2 — adds `package_tag` (purge index) and `value_bytes` (quota accounting
///   without decryption). Rows written by generation 1 carry `NULL` in both and
///   are resolved by decrypting only those rows.
const SCHEMA_VERSION: i64 = 2;

const DEFAULT_MAX_ENTRIES: usize = 1_024;
const DEFAULT_MAX_VALUE_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_TOTAL_VALUE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy)]
struct StateQuotas {
    max_entries: usize,
    max_value_bytes: usize,
    max_total_value_bytes: usize,
}

impl StateQuotas {
    const DEFAULT: Self = Self {
        max_entries: DEFAULT_MAX_ENTRIES,
        max_value_bytes: DEFAULT_MAX_VALUE_BYTES,
        max_total_value_bytes: DEFAULT_MAX_TOTAL_VALUE_BYTES,
    };
}

/// Runtime-owned handle to the one plugin-state database for an install.
#[derive(Clone)]
pub(crate) struct PluginStateStore {
    db_path: PathBuf,
    secret_store: SecretStore,
    connection: Arc<OnceLock<Mutex<Connection>>>,
    quotas: StateQuotas,
}

impl PluginStateStore {
    /// Derive storage from canonical install paths without opening or creating
    /// either the database or key until the first state operation.
    #[must_use]
    pub(crate) fn new(data_dir: &Path, config_dir: &Path) -> Self {
        Self {
            db_path: data_dir.join(DATABASE_FILE),
            // Plugin state is always encrypted even when an operator chooses
            // plaintext config. This reuses the install's one `.secret_key`.
            secret_store: SecretStore::new(config_dir, true),
            connection: Arc::new(OnceLock::new()),
            quotas: StateQuotas::DEFAULT,
        }
    }

    #[cfg(test)]
    fn with_quotas(data_dir: &Path, config_dir: &Path, quotas: StateQuotas) -> Self {
        Self {
            quotas,
            ..Self::new(data_dir, config_dir)
        }
    }

    fn connection(&self) -> Result<MutexGuard<'_, Connection>, PluginStateError> {
        if self.connection.get().is_none() {
            let connection = open_database(&self.db_path)?;
            let _ = self.connection.set(Mutex::new(connection));
        }
        self.connection
            .get()
            .ok_or(PluginStateError::Unavailable)?
            .lock()
            .map_err(|_| PluginStateError::Unavailable)
    }

    fn get_sync(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
    ) -> Result<Option<PluginStateValue>, PluginStateError> {
        if !self.db_path.exists() {
            return Ok(None);
        }
        self.verify_existing_ciphertext()?;
        let owner = self.owner_locator(scope)?;
        let locator = self.row_locator(&owner, key.as_str())?;
        let connection = self.connection()?;
        let row = connection
            .query_row(
                "SELECT owner, ciphertext FROM plugin_state WHERE locator = ?1",
                params![locator.as_slice()],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|_| PluginStateError::Unavailable)?;
        row.map(|(stored_owner, ciphertext)| {
            if stored_owner.as_slice() != owner {
                return Err(PluginStateError::Unavailable);
            }
            self.open_value(scope, key, &locator, &ciphertext)
        })
        .transpose()
    }

    fn put_sync(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
        value: &[u8],
        expected_revision: Option<u64>,
    ) -> Result<u64, PluginStateError> {
        if value.len() > self.quotas.max_value_bytes {
            return Err(PluginStateError::QuotaExceeded);
        }
        if self.db_path.exists() {
            self.verify_existing_ciphertext()?;
        }
        let owner = self.owner_locator_for_write(scope)?;
        let locator = self.row_locator(&owner, key.as_str())?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| PluginStateError::Unavailable)?;

        let existing = transaction
            .query_row(
                "SELECT owner, ciphertext FROM plugin_state WHERE locator = ?1",
                params![locator.as_slice()],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|_| PluginStateError::Unavailable)?;
        let current_revision = existing
            .as_ref()
            .map(|(stored_owner, ciphertext)| {
                if stored_owner.as_slice() != owner {
                    return Err(PluginStateError::Unavailable);
                }
                self.open_envelope(scope, key, &locator, ciphertext)
            })
            .transpose()?
            .map(|envelope| envelope.revision);
        if current_revision != expected_revision {
            return Err(PluginStateError::Conflict);
        }

        enforce_quotas(&transaction, self, scope, &owner, &locator, value.len())?;
        let revision = current_revision
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(PluginStateError::Unavailable)?;
        let ciphertext = self.seal(scope, key, revision, value)?;
        let package_tag = self.package_tag(scope.id().package())?;
        let value_bytes =
            i64::try_from(value.len()).map_err(|_| PluginStateError::QuotaExceeded)?;
        transaction
            .execute(
                "INSERT INTO plugin_state (owner, locator, package_tag, value_bytes, ciphertext) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(locator) DO UPDATE SET owner = excluded.owner, \
                 package_tag = excluded.package_tag, value_bytes = excluded.value_bytes, \
                 ciphertext = excluded.ciphertext",
                params![
                    owner.as_slice(),
                    locator.as_slice(),
                    package_tag.as_slice(),
                    value_bytes,
                    ciphertext.as_str()
                ],
            )
            .map_err(|_| PluginStateError::Unavailable)?;
        transaction
            .commit()
            .map_err(|_| PluginStateError::Unavailable)?;
        Ok(revision)
    }

    fn delete_sync(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
        expected_revision: u64,
    ) -> Result<(), PluginStateError> {
        if !self.db_path.exists() {
            return Err(PluginStateError::NotFound);
        }
        // Deliberately NOT gated on `verify_existing_ciphertext`. Delete is the
        // repair path, so gating it on the very probe it exists to recover from
        // makes a damaged store unrecoverable — most visibly when the unreadable
        // row is the only row, where nothing else can satisfy the probe.
        //
        // Skipping the probe is safe: `owner` and `locator` are derived from the
        // caller's own scope and key under the current install key, and the
        // delete is constrained by both columns below. Under a wrong key the
        // derivation simply matches no row and the call reports `NotFound`, so
        // this cannot be used to destroy data the caller could not already name.
        let owner = self.owner_locator(scope)?;
        let locator = self.row_locator(&owner, key.as_str())?;
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| PluginStateError::Unavailable)?;
        let (stored_owner, ciphertext) = transaction
            .query_row(
                "SELECT owner, ciphertext FROM plugin_state WHERE locator = ?1",
                params![locator.as_slice()],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(|_| PluginStateError::Unavailable)?
            .ok_or(PluginStateError::NotFound)?;
        if stored_owner.as_slice() != owner {
            return Err(PluginStateError::Unavailable);
        }
        // A row the owner can no longer open is still the owner's row to remove:
        // the locator was derived from this instance's own scope and key, so the
        // caller cannot reach another instance's data here. Allowing the delete
        // is the documented repair path for a row left unreadable by a restored
        // backup or a rotated key; without it a single bad row is permanent.
        match self.open_envelope(scope, key, &locator, &ciphertext) {
            Ok(envelope) => {
                if envelope.revision != expected_revision {
                    return Err(PluginStateError::Conflict);
                }
            }
            Err(_) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "removing an unreadable plugin state row; it could not be \
                     authenticated with the current install key"
                );
            }
        }
        let deleted = transaction
            .execute(
                "DELETE FROM plugin_state WHERE owner = ?1 AND locator = ?2",
                params![owner.as_slice(), locator.as_slice()],
            )
            .map_err(|_| PluginStateError::Unavailable)?;
        if deleted != 1 {
            return Err(PluginStateError::Conflict);
        }
        transaction
            .commit()
            .map_err(|_| PluginStateError::Unavailable)
    }

    fn seal(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
        revision: u64,
        value: &[u8],
    ) -> Result<String, PluginStateError> {
        let envelope = StateEnvelope {
            format: ENVELOPE_VERSION,
            package: scope.id().package().to_string(),
            capability: scope.id().capability(),
            binding: scope.id().binding().to_string(),
            key: key.as_str().to_string(),
            revision,
            value: base64::engine::general_purpose::STANDARD.encode(value),
        };
        let plaintext = Zeroizing::new(
            serde_json::to_string(&envelope).map_err(|_| PluginStateError::Unavailable)?,
        );
        let ciphertext = self
            .secret_store
            .encrypt(&plaintext)
            .map_err(|_| PluginStateError::Unavailable)?;
        if !SecretStore::is_secure_encrypted(&ciphertext) {
            return Err(PluginStateError::Unavailable);
        }
        Ok(ciphertext)
    }

    fn open_envelope(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
        locator: &[u8; 32],
        ciphertext: &str,
    ) -> Result<StateEnvelope, PluginStateError> {
        let owner = self.owner_locator(scope)?;
        let envelope = self.open_indexed_envelope(&owner, locator, ciphertext)?;
        let identity_matches = envelope.package == scope.id().package()
            && envelope.capability == scope.id().capability()
            && envelope.binding == scope.id().binding()
            && envelope.key == key.as_str();
        if !identity_matches {
            return Err(PluginStateError::Unavailable);
        }
        Ok(envelope)
    }

    fn open_value(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
        locator: &[u8; 32],
        ciphertext: &str,
    ) -> Result<PluginStateValue, PluginStateError> {
        let envelope = self.open_envelope(scope, key, locator, ciphertext)?;
        let value = base64::engine::general_purpose::STANDARD
            .decode(&envelope.value)
            .map_err(|_| PluginStateError::Unavailable)?;
        if value.len() > self.quotas.max_value_bytes {
            return Err(PluginStateError::Unavailable);
        }
        Ok(PluginStateValue::new(envelope.revision, value))
    }

    fn owner_locator(&self, scope: &PluginInstanceScope) -> Result<[u8; 32], PluginStateError> {
        self.owner_locator_from_parts(
            scope.id().package(),
            scope.id().capability(),
            scope.id().binding(),
        )
    }

    fn owner_locator_for_write(
        &self,
        scope: &PluginInstanceScope,
    ) -> Result<[u8; 32], PluginStateError> {
        let identity = encoded_identity(
            scope.id().package(),
            scope.id().capability(),
            scope.id().binding(),
        )?;
        match self.secret_store.keyed_digest(OWNER_DOMAIN, &identity) {
            Ok(locator) => Ok(locator),
            Err(_) if !self.db_path.exists() => self
                .secret_store
                .keyed_digest_or_create(OWNER_DOMAIN, &identity)
                .map_err(|_| PluginStateError::Unavailable),
            Err(_) => Err(PluginStateError::Unavailable),
        }
    }

    fn owner_locator_from_parts(
        &self,
        package: &str,
        capability: PluginCapability,
        binding: &str,
    ) -> Result<[u8; 32], PluginStateError> {
        let identity = encoded_identity(package, capability, binding)?;
        self.secret_store
            .keyed_digest(OWNER_DOMAIN, &identity)
            .map_err(|_| PluginStateError::Unavailable)
    }

    fn row_locator(&self, owner: &[u8; 32], key: &str) -> Result<[u8; 32], PluginStateError> {
        let mut material = Zeroizing::new(Vec::with_capacity(owner.len() + key.len() + 1));
        material.extend_from_slice(owner);
        material.push(0);
        material.extend_from_slice(key.as_bytes());
        self.secret_store
            .keyed_digest(LOCATOR_DOMAIN, &material)
            .map_err(|_| PluginStateError::Unavailable)
    }

    /// Keyed tag over the package name, used only as a purge index.
    ///
    /// Like the owner and row locators this is an HMAC under the install key, so
    /// it identifies a package to this install without revealing the name to a
    /// reader of the database file.
    fn package_tag(&self, package: &str) -> Result<[u8; 32], PluginStateError> {
        self.secret_store
            .keyed_digest(PACKAGE_DOMAIN, package.as_bytes())
            .map_err(|_| PluginStateError::Unavailable)
    }

    /// Delete every row a package owns, across all of its capabilities and
    /// bindings. Returns the number of rows removed.
    ///
    /// This is the teardown for `plugin remove`: because an instance identity is
    /// derived from `(package, capability, binding)` and deliberately excludes
    /// version and payload digest, a later install of the same package name
    /// would otherwise inherit the previous plugin's secrets.
    /// Fails closed when the install key cannot open stored rows. Reporting a
    /// successful purge under a replaced key would delete nothing while telling
    /// the operator their secrets were destroyed.
    pub(crate) fn purge_package(&self, package: &str) -> Result<u64, PluginStateError> {
        if !self.db_path.exists() {
            return Ok(0);
        }
        self.verify_existing_ciphertext()?;
        let package_tag = self.package_tag(package)?;
        let connection = self.connection()?;
        let mut removed = connection
            .execute(
                "DELETE FROM plugin_state WHERE package_tag = ?1",
                params![package_tag.as_slice()],
            )
            .map_err(|_| PluginStateError::Unavailable)? as u64;

        // Generation-1 rows predate `package_tag` and carry NULL, so the indexed
        // delete above cannot see them. Their owning package lives only inside
        // the sealed envelope, so match them by opening each one. Skipping this
        // would leave exactly the secrets this call exists to destroy.
        let mut untagged = connection
            .prepare(
                "SELECT owner, locator, ciphertext FROM plugin_state WHERE package_tag IS NULL",
            )
            .map_err(|_| PluginStateError::Unavailable)?;
        let mut doomed: Vec<[u8; 32]> = Vec::new();
        let mut rows = untagged
            .query([])
            .map_err(|_| PluginStateError::Unavailable)?;
        while let Some(row) = rows.next().map_err(|_| PluginStateError::Unavailable)? {
            let owner: Vec<u8> = row.get(0).map_err(|_| PluginStateError::Unavailable)?;
            let locator: Vec<u8> = row.get(1).map_err(|_| PluginStateError::Unavailable)?;
            let ciphertext: String = row.get(2).map_err(|_| PluginStateError::Unavailable)?;
            let (Ok(owner), Ok(locator)) =
                (<[u8; 32]>::try_from(owner), <[u8; 32]>::try_from(locator))
            else {
                continue;
            };
            if let Ok(envelope) = self.open_indexed_envelope(&owner, &locator, &ciphertext)
                && envelope.package == package
            {
                doomed.push(locator);
            }
        }
        drop(rows);
        drop(untagged);
        for locator in doomed {
            removed += connection
                .execute(
                    "DELETE FROM plugin_state WHERE locator = ?1",
                    params![locator.as_slice()],
                )
                .map_err(|_| PluginStateError::Unavailable)? as u64;
        }
        Ok(removed)
    }

    fn open_indexed_envelope(
        &self,
        owner: &[u8; 32],
        locator: &[u8; 32],
        ciphertext: &str,
    ) -> Result<StateEnvelope, PluginStateError> {
        if !SecretStore::is_secure_encrypted(ciphertext) {
            return Err(PluginStateError::Unavailable);
        }
        let plaintext = Zeroizing::new(
            self.secret_store
                .decrypt(ciphertext)
                .map_err(|_| PluginStateError::Unavailable)?,
        );
        let envelope: StateEnvelope =
            serde_json::from_str(&plaintext).map_err(|_| PluginStateError::Unavailable)?;
        let envelope_owner = self.owner_locator_from_parts(
            &envelope.package,
            envelope.capability,
            &envelope.binding,
        )?;
        let state_key = PluginStateKey::parse(envelope.key.as_str())?;
        let value_bytes = decoded_value_len(&envelope.value)?;
        if envelope.format != ENVELOPE_VERSION
            || envelope.revision == 0
            || envelope_owner != *owner
            || self.row_locator(owner, state_key.as_str())? != *locator
            || value_bytes > self.quotas.max_value_bytes
        {
            return Err(PluginStateError::Unavailable);
        }
        Ok(envelope)
    }

    /// Prove the current install key can still open stored rows before deriving
    /// lookups. This detects a replaced or lost key instead of interpreting
    /// every existing row as absent — which would silently look like "no state"
    /// and invite a caller to reinitialize over data it cannot see.
    ///
    /// The probe samples several rows and succeeds as soon as **one** opens.
    /// Requiring one specific sampled row to open meant a single unreadable row
    /// — a restored backup, a rotated key, a bit flip — failed every instance in
    /// the install, including instances whose own rows were intact. Only when
    /// rows exist and none of the sampled rows open is the key treated as wrong.
    fn verify_existing_ciphertext(&self) -> Result<(), PluginStateError> {
        /// Enough rows that one corrupt row cannot condemn a populated store,
        /// small enough that the check stays a fixed cost per operation.
        const PROBE_ROWS: i64 = 8;

        let connection = self.connection()?;
        let mut statement = connection
            .prepare("SELECT owner, locator, ciphertext FROM plugin_state LIMIT ?1")
            .map_err(|_| PluginStateError::Unavailable)?;
        let mut rows = statement
            .query(params![PROBE_ROWS])
            .map_err(|_| PluginStateError::Unavailable)?;
        let mut sampled = 0_usize;
        while let Some(row) = rows.next().map_err(|_| PluginStateError::Unavailable)? {
            sampled += 1;
            let owner: Vec<u8> = row.get(0).map_err(|_| PluginStateError::Unavailable)?;
            let locator: Vec<u8> = row.get(1).map_err(|_| PluginStateError::Unavailable)?;
            let ciphertext: String = row.get(2).map_err(|_| PluginStateError::Unavailable)?;
            let (Ok(owner), Ok(locator)) =
                (<[u8; 32]>::try_from(owner), <[u8; 32]>::try_from(locator))
            else {
                continue;
            };
            if self
                .open_indexed_envelope(&owner, &locator, &ciphertext)
                .is_ok()
            {
                return Ok(());
            }
        }
        if sampled == 0 {
            return Ok(());
        }
        Err(PluginStateError::Unavailable)
    }
}

#[async_trait]
impl PluginStateBackend for PluginStateStore {
    async fn get(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
    ) -> Result<Option<PluginStateValue>, PluginStateError> {
        let store = self.clone();
        let scope = scope.clone();
        let key = key.clone();
        tokio::task::spawn_blocking(move || store.get_sync(&scope, &key))
            .await
            .map_err(|_| PluginStateError::Unavailable)?
    }

    async fn put(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
        value: &[u8],
        expected_revision: Option<u64>,
    ) -> Result<u64, PluginStateError> {
        let store = self.clone();
        let scope = scope.clone();
        let key = key.clone();
        let value = Zeroizing::new(value.to_vec());
        tokio::task::spawn_blocking(move || {
            store.put_sync(&scope, &key, value.as_slice(), expected_revision)
        })
        .await
        .map_err(|_| PluginStateError::Unavailable)?
    }

    async fn delete(
        &self,
        scope: &PluginInstanceScope,
        key: &PluginStateKey,
        expected_revision: u64,
    ) -> Result<(), PluginStateError> {
        let store = self.clone();
        let scope = scope.clone();
        let key = key.clone();
        tokio::task::spawn_blocking(move || store.delete_sync(&scope, &key, expected_revision))
            .await
            .map_err(|_| PluginStateError::Unavailable)?
    }
}

#[derive(Serialize, Deserialize)]
struct StateEnvelope {
    format: u8,
    package: String,
    capability: PluginCapability,
    binding: String,
    key: String,
    revision: u64,
    value: String,
}

impl Drop for StateEnvelope {
    fn drop(&mut self) {
        self.package.zeroize();
        self.binding.zeroize();
        self.key.zeroize();
        self.value.zeroize();
    }
}

/// Purge every durable state row owned by `package`.
///
/// Teardown for `zeroclaw plugin remove`. Instance identity is derived from
/// `(package, capability, binding)` and deliberately excludes version and
/// payload digest, so without this a later install of the same package name —
/// possibly a different publisher — would inherit the removed plugin's secrets.
///
/// Returns the number of rows removed. A missing database is not an error.
pub fn purge_plugin_state(
    data_dir: &Path,
    config_dir: &Path,
    package: &str,
) -> Result<u64, PluginStateError> {
    PluginStateStore::new(data_dir, config_dir).purge_package(package)
}

fn open_database(path: &Path) -> Result<Connection, PluginStateError> {
    // Different agent/plugin registries can lazily open this install-owned
    // database at the same time. Serialize first-open PRAGMAs and schema setup
    // so concurrent stores do not race `journal_mode` or DDL before SQLite's
    // busy timeout is active.
    static DATABASE_OPEN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _open_guard = DATABASE_OPEN_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|error| error.into_inner());

    let parent = path.parent().ok_or(PluginStateError::Unavailable)?;
    std::fs::create_dir_all(parent).map_err(|_| PluginStateError::Unavailable)?;
    let connection = Connection::open(path).map_err(|_| PluginStateError::Unavailable)?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|_| PluginStateError::Unavailable)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| PluginStateError::Unavailable)?;
    }
    connection
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             CREATE TABLE IF NOT EXISTS plugin_state (
                 owner BLOB NOT NULL,
                 locator BLOB PRIMARY KEY NOT NULL,
                 package_tag BLOB,
                 value_bytes INTEGER,
                 ciphertext TEXT NOT NULL CHECK (ciphertext LIKE 'enc2:%')
             ) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS plugin_state_owner ON plugin_state(owner);",
        )
        .map_err(|_| PluginStateError::Unavailable)?;
    // The `package_tag` index is created by the migration and never here. On a
    // generation-1 table the column does not exist yet, so `CREATE INDEX` would
    // fail this whole batch with "no such column", leaving every state
    // operation returning `Unavailable` and the migration itself unreachable.
    migrate_schema(&connection)?;
    Ok(connection)
}

/// Bring a generation-1 database up to the current schema.
///
/// Generation 1 predates `package_tag` and `value_bytes`. Both are added as
/// nullable so existing ciphertext is never rewritten here: a `NULL` row is
/// resolved by decrypting that single row, and is repaired in place the next
/// time the owning instance writes the key.
fn migrate_schema(connection: &Connection) -> Result<(), PluginStateError> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|_| PluginStateError::Unavailable)?;
    if version >= SCHEMA_VERSION {
        return Ok(());
    }
    let mut existing = connection
        .prepare("PRAGMA table_info(plugin_state)")
        .map_err(|_| PluginStateError::Unavailable)?;
    let columns: Vec<String> = existing
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|_| PluginStateError::Unavailable)?
        .collect::<Result<_, _>>()
        .map_err(|_| PluginStateError::Unavailable)?;
    drop(existing);
    for (column, ddl) in [
        (
            "package_tag",
            "ALTER TABLE plugin_state ADD COLUMN package_tag BLOB",
        ),
        (
            "value_bytes",
            "ALTER TABLE plugin_state ADD COLUMN value_bytes INTEGER",
        ),
    ] {
        if !columns.iter().any(|name| name == column) {
            connection
                .execute_batch(ddl)
                .map_err(|_| PluginStateError::Unavailable)?;
        }
    }
    connection
        .execute_batch(&format!(
            "CREATE INDEX IF NOT EXISTS plugin_state_package ON plugin_state(package_tag);
             PRAGMA user_version = {SCHEMA_VERSION};"
        ))
        .map_err(|_| PluginStateError::Unavailable)?;
    Ok(())
}

fn enforce_quotas(
    transaction: &Transaction<'_>,
    store: &PluginStateStore,
    scope: &PluginInstanceScope,
    owner: &[u8; 32],
    replaced_locator: &[u8; 32],
    new_value_bytes: usize,
) -> Result<(), PluginStateError> {
    // Sizes are accounted from the plaintext `value_bytes` column rather than by
    // decrypting every sibling row. A row's value length is not secret — the
    // quota bounds already make it inferable — and decrypting the whole owner on
    // each write made a full store cost O(n^2) with three `.secret_key` reads
    // per row, all inside this write transaction.
    let (known_entries, known_total) = transaction
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(value_bytes), 0) FROM plugin_state \
             WHERE owner = ?1 AND locator <> ?2 AND value_bytes IS NOT NULL",
            params![owner.as_slice(), replaced_locator.as_slice()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(|_| PluginStateError::Unavailable)?;
    let mut entries = usize::try_from(known_entries)
        .map_err(|_| PluginStateError::Unavailable)?
        .checked_add(1)
        .ok_or(PluginStateError::QuotaExceeded)?;
    let mut total = usize::try_from(known_total)
        .map_err(|_| PluginStateError::Unavailable)?
        .checked_add(new_value_bytes)
        .ok_or(PluginStateError::QuotaExceeded)?;

    // Generation-1 rows carry no recorded length. Only those are opened, so the
    // decrypting path costs nothing once a store has been written by this
    // generation.
    let mut legacy = transaction
        .prepare(
            "SELECT locator, ciphertext FROM plugin_state \
             WHERE owner = ?1 AND locator <> ?2 AND value_bytes IS NULL",
        )
        .map_err(|_| PluginStateError::Unavailable)?;
    let mut rows = legacy
        .query(params![owner.as_slice(), replaced_locator.as_slice()])
        .map_err(|_| PluginStateError::Unavailable)?;
    while let Some(row) = rows.next().map_err(|_| PluginStateError::Unavailable)? {
        let locator: Vec<u8> = row.get(0).map_err(|_| PluginStateError::Unavailable)?;
        let locator: [u8; 32] = locator
            .try_into()
            .map_err(|_| PluginStateError::Unavailable)?;
        let ciphertext: String = row.get(1).map_err(|_| PluginStateError::Unavailable)?;
        let envelope = open_owned_envelope(store, scope, owner, &locator, &ciphertext)?;
        let value_bytes = decoded_value_len(&envelope.value)?;
        total = total
            .checked_add(value_bytes)
            .ok_or(PluginStateError::QuotaExceeded)?;
        entries = entries
            .checked_add(1)
            .ok_or(PluginStateError::QuotaExceeded)?;
    }
    if entries > store.quotas.max_entries || total > store.quotas.max_total_value_bytes {
        return Err(PluginStateError::QuotaExceeded);
    }
    Ok(())
}

fn open_owned_envelope(
    store: &PluginStateStore,
    scope: &PluginInstanceScope,
    owner: &[u8; 32],
    locator: &[u8; 32],
    ciphertext: &str,
) -> Result<StateEnvelope, PluginStateError> {
    let envelope = store.open_indexed_envelope(owner, locator, ciphertext)?;
    let scope_matches = envelope.package == scope.id().package()
        && envelope.capability == scope.id().capability()
        && envelope.binding == scope.id().binding();
    if !scope_matches {
        return Err(PluginStateError::Unavailable);
    }
    Ok(envelope)
}

fn encoded_identity(
    package: &str,
    capability: PluginCapability,
    binding: &str,
) -> Result<Zeroizing<Vec<u8>>, PluginStateError> {
    serde_json::to_vec(&(package, capability, binding))
        .map(Zeroizing::new)
        .map_err(|_| PluginStateError::Unavailable)
}

fn decoded_value_len(encoded: &str) -> Result<usize, PluginStateError> {
    let decoded = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| PluginStateError::Unavailable)?,
    );
    Ok(decoded.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_plugins::{PluginManifest, PluginPermission};

    fn scope(binding: &str) -> PluginInstanceScope {
        let manifest = PluginManifest {
            name: "state-fixture".to_string(),
            version: "0.0.0-test".to_string(),
            description: None,
            author: None,
            wasm_path: Some("fixture.wasm".to_string()),
            wasm_sha256: None,
            capabilities: vec![PluginCapability::Channel],
            permissions: vec![PluginPermission::StateRead, PluginPermission::StateWrite],
            config_schema: None,
            signature: None,
            publisher_key: None,
        };
        PluginInstanceScope::from_manifest(
            &manifest,
            PluginCapability::Channel,
            binding,
            [PluginPermission::StateRead, PluginPermission::StateWrite],
        )
        .unwrap()
    }

    fn key(name: &str) -> PluginStateKey {
        PluginStateKey::parse(name).unwrap()
    }

    fn store(root: &TempDir) -> PluginStateStore {
        PluginStateStore::new(&root.path().join("data"), root.path())
    }

    #[tokio::test]
    async fn absent_reads_do_not_create_a_database_or_install_key() {
        let root = TempDir::new().unwrap();
        let state_store = store(&root);
        let instance_scope = scope("fresh");
        let state_key = key("missing");

        assert!(
            state_store
                .get(&instance_scope, &state_key)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            state_store.delete(&instance_scope, &state_key, 1).await,
            Err(PluginStateError::NotFound)
        );
        assert!(!state_store.db_path.exists());
        assert!(!root.path().join(".secret_key").exists());
    }

    #[tokio::test]
    async fn state_is_encrypted_isolated_and_persistent_across_restart() {
        let root = TempDir::new().unwrap();
        let first = store(&root);
        let primary = scope("primary");
        let secondary = scope("secondary");
        let state_key = key("refresh-token");
        let plaintext = b"state-value-must-not-appear-on-disk";

        assert_eq!(
            first.put(&primary, &state_key, plaintext, None).await,
            Ok(1)
        );
        let value = first.get(&primary, &state_key).await.unwrap().unwrap();
        assert_eq!(value.revision(), 1);
        assert_eq!(value.value(), plaintext);
        assert!(first.get(&secondary, &state_key).await.unwrap().is_none());

        for path in std::fs::read_dir(root.path().join("data"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
        {
            let raw = std::fs::read(path).unwrap();
            for forbidden in [
                plaintext.as_slice(),
                b"refresh-token".as_slice(),
                b"state-fixture".as_slice(),
                b"primary".as_slice(),
            ] {
                assert!(
                    !raw.windows(forbidden.len())
                        .any(|window| window == forbidden),
                    "database or journal exposed logical plugin state"
                );
            }
        }
        let connection = Connection::open(&first.db_path).unwrap();
        let (owner_len, locator_len, ciphertext): (usize, usize, String) = connection
            .query_row(
                "SELECT length(owner), length(locator), ciphertext FROM plugin_state",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!((owner_len, locator_len), (32, 32));
        assert!(SecretStore::is_secure_encrypted(&ciphertext));
        drop(first);

        let restarted = store(&root);
        let value = restarted.get(&primary, &state_key).await.unwrap().unwrap();
        assert_eq!(value.value(), plaintext);
        assert_eq!(value.revision(), 1);
    }

    #[tokio::test]
    async fn concurrent_first_writes_share_the_install_key_and_database() {
        let root = TempDir::new().unwrap();
        let first = store(&root);
        let second = store(&root);
        let instance_scope = scope("concurrent");
        let first_key = key("first");
        let second_key = key("second");

        let (first_revision, second_revision) = tokio::join!(
            first.put(&instance_scope, &first_key, b"one", None),
            second.put(&instance_scope, &second_key, b"two", None),
        );
        assert_eq!(first_revision, Ok(1));
        assert_eq!(second_revision, Ok(1));

        let reopened = store(&root);
        assert_eq!(
            reopened
                .get(&instance_scope, &first_key)
                .await
                .unwrap()
                .unwrap()
                .value(),
            b"one"
        );
        assert_eq!(
            reopened
                .get(&instance_scope, &second_key)
                .await
                .unwrap()
                .unwrap()
                .value(),
            b"two"
        );
    }

    #[tokio::test]
    async fn put_and_delete_require_exact_cas_revisions() {
        let root = TempDir::new().unwrap();
        let store = store(&root);
        let scope = scope("cas");
        let key = key("cursor");

        assert_eq!(store.put(&scope, &key, b"one", None).await, Ok(1));
        assert_eq!(
            store.put(&scope, &key, b"duplicate", None).await,
            Err(PluginStateError::Conflict)
        );
        assert_eq!(
            store.put(&scope, &key, b"wrong", Some(9)).await,
            Err(PluginStateError::Conflict)
        );
        assert_eq!(store.put(&scope, &key, b"two", Some(1)).await, Ok(2));
        assert_eq!(
            store.delete(&scope, &key, 1).await,
            Err(PluginStateError::Conflict)
        );
        assert_eq!(store.delete(&scope, &key, 2).await, Ok(()));
        assert!(store.get(&scope, &key).await.unwrap().is_none());
        assert_eq!(
            store.delete(&scope, &key, 2).await,
            Err(PluginStateError::NotFound)
        );
    }

    #[tokio::test]
    async fn fixed_entry_value_and_total_quotas_fail_closed() {
        let root = TempDir::new().unwrap();
        let store = PluginStateStore::with_quotas(
            &root.path().join("data"),
            root.path(),
            StateQuotas {
                max_entries: 2,
                max_value_bytes: 4,
                max_total_value_bytes: 6,
            },
        );
        let scope = scope("quota");
        assert_eq!(
            store.put(&scope, &key("oversized"), b"12345", None).await,
            Err(PluginStateError::QuotaExceeded)
        );
        assert_eq!(store.put(&scope, &key("first"), b"1234", None).await, Ok(1));
        assert_eq!(store.put(&scope, &key("second"), b"12", None).await, Ok(1));
        assert_eq!(
            store.put(&scope, &key("third"), b"1", None).await,
            Err(PluginStateError::QuotaExceeded)
        );
        assert_eq!(
            store.put(&scope, &key("second"), b"123", Some(1)).await,
            Err(PluginStateError::QuotaExceeded)
        );
    }

    #[tokio::test]
    async fn swapped_ciphertexts_and_tampering_are_rejected() {
        let root = TempDir::new().unwrap();
        let state_store = store(&root);
        let instance_scope = scope("tamper");
        let first = key("first");
        let second = key("second");
        state_store
            .put(&instance_scope, &first, b"one", None)
            .await
            .unwrap();
        state_store
            .put(&instance_scope, &second, b"two", None)
            .await
            .unwrap();

        let owner = state_store.owner_locator(&instance_scope).unwrap();
        let first_locator = state_store.row_locator(&owner, first.as_str()).unwrap();
        let second_locator = state_store.row_locator(&owner, second.as_str()).unwrap();
        let mut connection = Connection::open(&state_store.db_path).unwrap();
        let transaction = connection.transaction().unwrap();
        let first_ciphertext: String = transaction
            .query_row(
                "SELECT ciphertext FROM plugin_state WHERE locator = ?1",
                [first_locator.as_slice()],
                |row| row.get(0),
            )
            .unwrap();
        let second_ciphertext: String = transaction
            .query_row(
                "SELECT ciphertext FROM plugin_state WHERE locator = ?1",
                [second_locator.as_slice()],
                |row| row.get(0),
            )
            .unwrap();
        transaction
            .execute(
                "UPDATE plugin_state SET ciphertext = ?1 WHERE locator = ?2",
                params![second_ciphertext, first_locator.as_slice()],
            )
            .unwrap();
        transaction
            .execute(
                "UPDATE plugin_state SET ciphertext = ?1 WHERE locator = ?2",
                params![first_ciphertext, second_locator.as_slice()],
            )
            .unwrap();
        transaction.commit().unwrap();
        assert!(matches!(
            state_store.get(&instance_scope, &first).await,
            Err(PluginStateError::Unavailable)
        ));
        assert!(matches!(
            state_store.get(&instance_scope, &second).await,
            Err(PluginStateError::Unavailable)
        ));

        let tamper_root = TempDir::new().unwrap();
        let tamper_store = store(&tamper_root);
        let tamper_scope = scope("tamper-bit");
        let fresh = key("fresh");
        tamper_store
            .put(&tamper_scope, &fresh, b"three", None)
            .await
            .unwrap();
        let fresh_owner = tamper_store.owner_locator(&tamper_scope).unwrap();
        let fresh_locator = tamper_store
            .row_locator(&fresh_owner, fresh.as_str())
            .unwrap();
        let connection = Connection::open(&tamper_store.db_path).unwrap();
        connection
            .execute(
                "UPDATE plugin_state SET ciphertext = substr(ciphertext, 1, length(ciphertext) - 1) || \
                 CASE substr(ciphertext, -1) WHEN '0' THEN '1' ELSE '0' END WHERE locator = ?1",
                [fresh_locator.as_slice()],
            )
            .unwrap();
        assert!(matches!(
            tamper_store.get(&tamper_scope, &fresh).await,
            Err(PluginStateError::Unavailable)
        ));
    }

    #[tokio::test]
    async fn owner_and_locator_swap_is_detected_by_the_sealed_identity() {
        let root = TempDir::new().unwrap();
        let store = store(&root);
        let source = scope("source");
        let target = scope("target");
        let key = key("session");
        store.put(&source, &key, b"private", None).await.unwrap();

        let source_owner = store.owner_locator(&source).unwrap();
        let source_locator = store.row_locator(&source_owner, key.as_str()).unwrap();
        let target_owner = store.owner_locator(&target).unwrap();
        let target_locator = store.row_locator(&target_owner, key.as_str()).unwrap();
        let connection = Connection::open(&store.db_path).unwrap();
        connection
            .execute(
                "UPDATE plugin_state SET owner = ?1, locator = ?2 WHERE locator = ?3",
                params![
                    target_owner.as_slice(),
                    target_locator.as_slice(),
                    source_locator.as_slice()
                ],
            )
            .unwrap();

        assert!(matches!(
            store.get(&target, &key).await,
            Err(PluginStateError::Unavailable)
        ));
        assert!(matches!(
            store.get(&source, &key).await,
            Err(PluginStateError::Unavailable)
        ));
    }

    #[tokio::test]
    async fn missing_or_replaced_install_key_cannot_access_existing_state() {
        let root = TempDir::new().unwrap();
        let state_store = store(&root);
        let instance_scope = scope("key-loss");
        let key = key("credential");
        state_store
            .put(&instance_scope, &key, b"opaque", None)
            .await
            .unwrap();
        drop(state_store);

        let key_path = root.path().join(".secret_key");
        let backup = root.path().join(".secret_key.backup");
        std::fs::rename(&key_path, &backup).unwrap();
        let reopened = store(&root);
        assert!(matches!(
            reopened.get(&instance_scope, &key).await,
            Err(PluginStateError::Unavailable)
        ));
        assert!(
            !key_path.exists(),
            "failed read must preserve key-loss recovery without creating a replacement"
        );

        std::fs::rename(&backup, &key_path).unwrap();
        let replacement_root = TempDir::new().unwrap();
        SecretStore::new(replacement_root.path(), true)
            .encrypt("replacement")
            .unwrap();
        std::fs::copy(replacement_root.path().join(".secret_key"), &key_path).unwrap();
        let replaced = store(&root);
        assert!(matches!(
            replaced.get(&instance_scope, &key).await,
            Err(PluginStateError::Unavailable)
        ));
        assert_eq!(
            replaced
                .put(&instance_scope, &key, b"new-generation", None)
                .await,
            Err(PluginStateError::Unavailable)
        );
    }

    fn scope_for(package: &str, binding: &str) -> PluginInstanceScope {
        let manifest = PluginManifest {
            name: package.to_string(),
            version: "0.0.0-test".to_string(),
            description: None,
            author: None,
            wasm_path: Some("fixture.wasm".to_string()),
            wasm_sha256: None,
            capabilities: vec![PluginCapability::Channel],
            permissions: vec![PluginPermission::StateRead, PluginPermission::StateWrite],
            config_schema: None,
            signature: None,
            publisher_key: None,
        };
        PluginInstanceScope::from_manifest(
            &manifest,
            PluginCapability::Channel,
            binding,
            [PluginPermission::StateRead, PluginPermission::StateWrite],
        )
        .unwrap()
    }

    /// Overwrite one row's ciphertext with authentic-looking but unopenable
    /// data, simulating a restored backup or a bit flip.
    fn corrupt_row(store: &PluginStateStore, scope: &PluginInstanceScope, key: &PluginStateKey) {
        let owner = store.owner_locator(scope).unwrap();
        let locator = store.row_locator(&owner, key.as_str()).unwrap();
        let connection = Connection::open(&store.db_path).unwrap();
        connection
            .execute(
                "UPDATE plugin_state SET ciphertext = ?1 WHERE locator = ?2",
                params!["enc2:deadbeef", locator.as_slice()],
            )
            .unwrap();
    }

    #[tokio::test]
    async fn removing_a_package_purges_only_its_own_state() {
        let root = TempDir::new().unwrap();
        let store = store(&root);
        let key = key("token");

        let doomed_a = scope_for("mailsync", "default");
        let doomed_b = scope_for("mailsync", "second");
        let survivor = scope_for("calendarsync", "default");
        store
            .put(&doomed_a, &key, b"refresh-a", None)
            .await
            .unwrap();
        store
            .put(&doomed_b, &key, b"refresh-b", None)
            .await
            .unwrap();
        store
            .put(&survivor, &key, b"unrelated", None)
            .await
            .unwrap();

        assert_eq!(store.purge_package("mailsync").unwrap(), 2);

        // A later install of the same package name must not inherit secrets.
        assert!(store.get(&doomed_a, &key).await.unwrap().is_none());
        assert!(store.get(&doomed_b, &key).await.unwrap().is_none());
        assert_eq!(
            store
                .get(&survivor, &key)
                .await
                .unwrap()
                .map(|value| value.value().to_vec()),
            Some(b"unrelated".to_vec())
        );
        // Purging an absent package is a no-op rather than an error.
        assert_eq!(store.purge_package("mailsync").unwrap(), 0);
    }

    #[tokio::test]
    async fn one_unreadable_row_does_not_disable_other_instances() {
        let root = TempDir::new().unwrap();
        let store = store(&root);
        let key = key("session");
        let broken = scope_for("broken-plugin", "default");
        let healthy = scope_for("healthy-plugin", "default");

        for index in 0..6 {
            let filler = scope_for("healthy-plugin", &format!("binding-{index}"));
            store.put(&filler, &key, b"intact", None).await.unwrap();
        }
        store.put(&broken, &key, b"doomed", None).await.unwrap();
        store.put(&healthy, &key, b"intact", None).await.unwrap();
        corrupt_row(&store, &broken, &key);

        // The corrupt row belongs to one instance; every other instance keeps
        // working rather than the whole install failing closed.
        assert_eq!(
            store
                .get(&healthy, &key)
                .await
                .unwrap()
                .map(|value| value.value().to_vec()),
            Some(b"intact".to_vec())
        );
        store
            .put(&healthy, &key, b"still-writable", Some(1))
            .await
            .unwrap();
        // The damaged instance still fails closed on its own row.
        assert!(matches!(
            store.get(&broken, &key).await,
            Err(PluginStateError::Unavailable)
        ));
    }

    #[tokio::test]
    async fn an_owner_can_delete_its_own_unreadable_row() {
        let root = TempDir::new().unwrap();
        let store = store(&root);
        let key = key("session");
        let owner = scope_for("broken-plugin", "default");
        let bystander = scope_for("healthy-plugin", "default");
        store.put(&owner, &key, b"doomed", None).await.unwrap();
        store.put(&bystander, &key, b"intact", None).await.unwrap();
        corrupt_row(&store, &owner, &key);

        // Repair path: a row that can no longer be authenticated is still the
        // owner's to remove, otherwise the damage is permanent.
        store.delete(&owner, &key, 1).await.unwrap();
        assert!(store.get(&owner, &key).await.unwrap().is_none());
        store
            .put(&owner, &key, b"reinitialized", None)
            .await
            .unwrap();
        assert_eq!(
            store
                .get(&bystander, &key)
                .await
                .unwrap()
                .map(|value| value.value().to_vec()),
            Some(b"intact".to_vec())
        );
    }

    /// Rewrite the table into the generation-1 shape (no `package_tag`, no
    /// `value_bytes`, `user_version = 1`) preserving existing ciphertext.
    fn downgrade_to_generation_1(db_path: &Path) {
        let connection = Connection::open(db_path).unwrap();
        connection
            .execute_batch(
                "DROP INDEX IF EXISTS plugin_state_package;
                 CREATE TABLE plugin_state_v1 (
                     owner BLOB NOT NULL,
                     locator BLOB PRIMARY KEY NOT NULL,
                     ciphertext TEXT NOT NULL CHECK (ciphertext LIKE 'enc2:%')
                 ) WITHOUT ROWID;
                 INSERT INTO plugin_state_v1 (owner, locator, ciphertext)
                     SELECT owner, locator, ciphertext FROM plugin_state;
                 DROP TABLE plugin_state;
                 ALTER TABLE plugin_state_v1 RENAME TO plugin_state;
                 CREATE INDEX IF NOT EXISTS plugin_state_owner ON plugin_state(owner);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
    }

    #[tokio::test]
    async fn a_generation_1_database_still_opens_reads_and_purges() {
        let root = TempDir::new().unwrap();
        let data_dir = root.path().join("data");
        let key = key("token");
        let doomed = scope_for("mailsync", "default");
        let survivor = scope_for("calendarsync", "default");

        let first = PluginStateStore::new(&data_dir, root.path());
        first.put(&doomed, &key, b"refresh", None).await.unwrap();
        first
            .put(&survivor, &key, b"unrelated", None)
            .await
            .unwrap();
        let db_path = first.db_path.clone();
        drop(first);
        downgrade_to_generation_1(&db_path);

        // Opening a generation-1 database must migrate rather than fail. Creating
        // the package_tag index before the ALTER made every operation return
        // Unavailable and left the migration unreachable.
        let reopened = PluginStateStore::new(&data_dir, root.path());
        assert_eq!(
            reopened
                .get(&doomed, &key)
                .await
                .unwrap()
                .map(|value| value.value().to_vec()),
            Some(b"refresh".to_vec())
        );
        let version: i64 = {
            let connection = reopened.connection().unwrap();
            connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(version, SCHEMA_VERSION);

        // Rows migrated from generation 1 carry a NULL package_tag, so the
        // indexed delete alone would silently leave them behind.
        assert_eq!(reopened.purge_package("mailsync").unwrap(), 1);
        assert!(reopened.get(&doomed, &key).await.unwrap().is_none());
        assert_eq!(
            reopened
                .get(&survivor, &key)
                .await
                .unwrap()
                .map(|value| value.value().to_vec()),
            Some(b"unrelated".to_vec())
        );
    }

    #[tokio::test]
    async fn purging_under_a_replaced_install_key_fails_instead_of_reporting_success() {
        let root = TempDir::new().unwrap();
        let original = store(&root);
        let instance = scope_for("mailsync", "default");
        original
            .put(&instance, &key("token"), b"refresh", None)
            .await
            .unwrap();
        drop(original);

        let replacement_root = TempDir::new().unwrap();
        SecretStore::new(replacement_root.path(), true)
            .encrypt("replacement")
            .unwrap();
        std::fs::copy(
            replacement_root.path().join(".secret_key"),
            root.path().join(".secret_key"),
        )
        .unwrap();

        // Under a replaced key the computed tag matches nothing. Reporting a
        // successful purge would tell an operator their secrets were destroyed
        // while every row survived.
        let replaced = store(&root);
        assert_eq!(
            replaced.purge_package("mailsync"),
            Err(PluginStateError::Unavailable)
        );
    }

    #[tokio::test]
    async fn a_sole_unreadable_row_is_still_repairable() {
        let root = TempDir::new().unwrap();
        let store = store(&root);
        let key = key("session");
        let solo = scope_for("solo-plugin", "default");
        store.put(&solo, &key, b"doomed", None).await.unwrap();
        corrupt_row(&store, &solo, &key);

        // With one row in the table there is nothing else that can satisfy the
        // key probe, so gating delete on that probe would make the documented
        // repair path unreachable in exactly the case it exists for.
        store.delete(&solo, &key, 1).await.unwrap();
        assert!(store.get(&solo, &key).await.unwrap().is_none());
        store.put(&solo, &key, b"reinit", None).await.unwrap();
        assert_eq!(
            store
                .get(&solo, &key)
                .await
                .unwrap()
                .map(|value| value.value().to_vec()),
            Some(b"reinit".to_vec())
        );
    }

    #[tokio::test]
    async fn quota_accounting_counts_rows_written_before_the_value_bytes_column() {
        let root = TempDir::new().unwrap();
        let quotas = StateQuotas {
            max_entries: 8,
            max_value_bytes: 64,
            max_total_value_bytes: 24,
        };
        let store = PluginStateStore::with_quotas(&root.path().join("data"), root.path(), quotas);
        let instance = scope_for("legacy-plugin", "default");
        store
            .put(&instance, &key("first"), b"0123456789", None)
            .await
            .unwrap();

        // Simulate a generation-1 row: recorded length erased, ciphertext intact.
        let connection = Connection::open(&store.db_path).unwrap();
        connection
            .execute("UPDATE plugin_state SET value_bytes = NULL", [])
            .unwrap();
        drop(connection);

        // The unrecorded row must still count toward the total, so the quota
        // cannot be bypassed by rows that predate the column.
        assert_eq!(
            store
                .put(&instance, &key("second"), b"0123456789abcdef", None)
                .await,
            Err(PluginStateError::QuotaExceeded)
        );
        store
            .put(&instance, &key("second"), b"0123", None)
            .await
            .unwrap();
    }
}
