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

#![cfg_attr(
    not(any(
        feature = "state-store",
        feature = "crypto-store",
        feature = "event-cache-store",
        feature = "media-store"
    )),
    allow(dead_code, unused_imports)
)]

mod connection;
#[cfg(feature = "crypto-store")]
mod crypto_store;
mod error;
#[cfg(feature = "event-cache-store")]
mod event_cache_store;
#[cfg(feature = "media-store")]
mod media_store;
#[cfg(feature = "state-store")]
mod state_store;
mod utils;
use std::{
    cmp::max,
    fmt,
    path::{Path, PathBuf},
};

use deadpool::managed::PoolConfig;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[cfg(feature = "crypto-store")]
pub use self::crypto_store::SqliteCryptoStore;
pub use self::error::OpenStoreError;
#[cfg(feature = "event-cache-store")]
pub use self::event_cache_store::SqliteEventCacheStore;
#[cfg(feature = "media-store")]
pub use self::media_store::SqliteMediaStore;
#[cfg(feature = "state-store")]
pub use self::state_store::{DATABASE_NAME as STATE_STORE_DATABASE_NAME, SqliteStateStore};

#[cfg(feature = "uniffi")]
uniffi::setup_scaffolding!();

#[cfg(test)]
matrix_sdk_test_utils::init_tracing_for_tests!();

/// An enum used to store the secret that gives access to a store
#[derive(Clone, Debug, PartialEq, Zeroize, ZeroizeOnDrop)]
pub enum Secret {
    // Cryptographic key used to open the store
    Key(Zeroizing<Vec<u8>>),
    // Passphrase used to open the store, ideally human chosen
    PassPhrase(Zeroizing<String>),
    // Randomly generated passphrase, for which the store caches a
    // cheaply-derivable copy of its cipher and skips derivation on later opens
    HighEntropyPassPhrase {
        key: Zeroizing<Vec<u8>>,
        #[zeroize(skip)]
        base64_variant: Base64Variant,
    },
}

/// Enum controlling how the high-entropy passphrase used to be created on the
/// client side.
///
/// This allows us to replicate how a random key was converted into a passphrase
/// to migrate from said passphrase to the plain key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum Base64Variant {
    /// Unpadded base64 was used to create the high-entropy passphrase.
    Unpadded,
    /// Standard padded base64 was used to create the high-entropy passphrase.
    Padded,
}

/// A configuration structure used for opening a store.
#[derive(Clone)]
pub struct SqliteStoreConfig {
    /// Path to the database, without the file name.
    path: PathBuf,
    /// Secret to open the store, if any
    secret: Option<Secret>,
    /// The pool configuration for [`deadpool`].
    pool_config: PoolConfig,
    /// The runtime configuration to apply when opening an SQLite connection.
    runtime_config: RuntimeConfig,
}

impl fmt::Debug for SqliteStoreConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SqliteStoreConfig")
            .field("path", &self.path)
            .field("pool_config", &self.pool_config)
            .field("runtime_config", &self.runtime_config)
            .finish_non_exhaustive()
    }
}

/// The minimum size of the connections pool.
///
/// We need at least 2 connections: one connection for write operations, and one
/// connection for read operations.
const POOL_MINIMUM_SIZE: usize = 2;

impl SqliteStoreConfig {
    /// Create a new [`SqliteStoreConfig`] with a path representing the
    /// directory containing the store database.
    pub fn new<P>(path: P) -> Self
    where
        P: AsRef<Path>,
    {
        Self {
            path: path.as_ref().to_path_buf(),
            pool_config: PoolConfig::new(max(POOL_MINIMUM_SIZE, num_cpus::get_physical() * 4)),
            runtime_config: RuntimeConfig::default(),
            secret: None,
        }
    }

    /// Similar to [`SqliteStoreConfig::new`], but with defaults tailored for a
    /// low memory usage environment.
    ///
    /// The following defaults are set:
    ///
    /// * The `pool_max_size` is set to the number of physical CPU, so one
    ///   connection per physical thread,
    /// * The `cache_size` is set to 500Kib,
    /// * The `journal_size_limit` is set to 2Mib.
    pub fn with_low_memory_config<P>(path: P) -> Self
    where
        P: AsRef<Path>,
    {
        Self::new(path)
            // Maximum one connection per physical thread.
            .pool_max_size(num_cpus::get_physical())
            // Cache size is 500Kib.
            .cache_size(500_000)
            // Journal size limit is 2Mib.
            .journal_size_limit(2_000_000)
    }

    /// Override the path.
    pub fn path<P>(mut self, path: P) -> Self
    where
        P: AsRef<Path>,
    {
        self.path = path.as_ref().to_path_buf();
        self
    }

    /// Define the passphrase if the store is encoded.
    ///
    /// Assumed to be possibly human-chosen, so an expensive derivation is run
    /// over it on every open. If it is randomly generated, use
    /// [`SqliteStoreConfig::high_entropy_passphrase`] instead.
    pub fn passphrase(mut self, passphrase: Option<&str>) -> Self {
        self.secret =
            passphrase.map(|passphrase| Secret::PassPhrase(Zeroizing::new(passphrase.to_owned())));
        self
    }

    /// Define the passphrase if the store is encoded, declaring that it was
    /// randomly generated rather than chosen by a human.
    ///
    /// Do NOT use this with human-chosen passphrases, as doing so would remove
    /// their brute-force protection.
    ///
    /// This migrates a passphrase-based store whose passphrase was created by
    /// base64-encoding a randomly generated key to a key-based setup.
    ///
    /// Once this function has been called, [`SqliteStoreConfig::passphrase`]
    /// can no longer be used with the passphrase.
    ///
    /// [`SqliteStoreConfig::key`] can be used with the original key, before it
    /// was base64-encoded.
    pub fn high_entropy_passphrase(
        mut self,
        passphrase: Option<&[u8]>,
        base64_variant: Base64Variant,
    ) -> Self {
        if let Some(passphrase) = passphrase {
            let key = Zeroizing::new(passphrase.to_vec());
            self.secret = Some(Secret::HighEntropyPassPhrase { key, base64_variant });
        }

        self
    }

    /// Define the key if the store is encoded.
    ///
    /// Assumed to be high entropy so no derivation is run over it.
    pub fn key(mut self, key: Option<&[u8]>) -> Self {
        if let Some(key) = key {
            let key = Zeroizing::new(key.to_vec());
            self.secret = Some(Secret::Key(key));
        }

        self
    }

    /// Define the maximum pool size for [`deadpool`].
    ///
    /// See [`deadpool::managed::PoolConfig::max_size`] to learn more.
    pub fn pool_max_size(mut self, max_size: usize) -> Self {
        self.pool_config.max_size = max(POOL_MINIMUM_SIZE, max_size);
        self
    }

    /// Optimize the database.
    ///
    /// The SQLite documentation recommends to run this regularly and after any
    /// schema change. The easiest is to do it consistently when the store is
    /// constructed, after eventual migrations.
    ///
    /// See [`PRAGMA optimize`] to learn more.
    ///
    /// The default value is `true`.
    ///
    /// [`PRAGMA optimize`]: https://www.sqlite.org/pragma.html#pragma_optimize
    pub fn optimize(mut self, optimize: bool) -> Self {
        self.runtime_config.optimize = optimize;
        self
    }

    /// Define the maximum size in **bytes** the SQLite cache can use.
    ///
    /// See [`PRAGMA cache_size`] to learn more.
    ///
    /// The default value is 2Mib.
    ///
    /// [`PRAGMA cache_size`]: https://www.sqlite.org/pragma.html#pragma_cache_size
    pub fn cache_size(mut self, cache_size: u32) -> Self {
        self.runtime_config.cache_size = cache_size;
        self
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
    /// The default value is 10Mib.
    ///
    /// [size_wal_file]: https://www.sqlite.org/wal.html#avoiding_excessively_large_wal_files
    /// [`PRAGMA journal_size_limit`]: https://www.sqlite.org/pragma.html#pragma_journal_size_limit
    pub fn journal_size_limit(mut self, limit: u32) -> Self {
        self.runtime_config.journal_size_limit = limit;
        self
    }

    /// Returns the pool configuration.
    pub(crate) fn pool_config(&self) -> PoolConfig {
        self.pool_config
    }

    /// Returns the runtime configuration.
    pub(crate) fn runtime_config(&self) -> RuntimeConfig {
        self.runtime_config
    }

    /// Build a pool of active connections to a particular database.
    pub fn build_pool_of_connections(
        &self,
        database_name: &str,
    ) -> Result<connection::Pool, connection::CreatePoolError> {
        let path = self.path.join(database_name);
        let manager = connection::Manager::new(path);

        connection::Pool::builder(manager)
            .config(self.pool_config)
            .runtime(connection::RUNTIME)
            .build()
            .map_err(connection::CreatePoolError::Build)
    }
}

/// This type represents values to set at runtime when a database is opened.
///
/// This configuration is applied by
/// [`utils::SqliteAsyncConnExt::apply_runtime_config`].
#[derive(Clone, Copy, Debug)]
struct RuntimeConfig {
    /// If `true`, [`utils::SqliteAsyncConnExt::optimize`] will be called.
    optimize: bool,

    /// Regardless of the value, [`utils::SqliteAsyncConnExt::cache_size`] will
    /// always be called with this value.
    cache_size: u32,

    /// Regardless of the value,
    /// [`utils::SqliteAsyncConnExt::journal_size_limit`] will always be called
    /// with this value.
    journal_size_limit: u32,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            // Optimize is always applied.
            optimize: true,
            // A cache of 2Mib.
            cache_size: 2_000_000,
            // A limit of 10Mib.
            journal_size_limit: 10_000_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        ops::Not,
        path::{Path, PathBuf},
    };

    use zeroize::Zeroizing;

    use super::{POOL_MINIMUM_SIZE, Secret, SqliteStoreConfig};

    #[test]
    fn test_new() {
        let store_config = SqliteStoreConfig::new(Path::new("foo"));

        assert_eq!(store_config.pool_config.max_size, num_cpus::get_physical() * 4);
        assert!(store_config.runtime_config.optimize);
        assert_eq!(store_config.runtime_config.cache_size, 2_000_000);
        assert_eq!(store_config.runtime_config.journal_size_limit, 10_000_000);
    }

    #[test]
    fn test_with_low_memory_config() {
        let store_config = SqliteStoreConfig::with_low_memory_config(Path::new("foo"));

        assert_eq!(store_config.pool_config.max_size, num_cpus::get_physical());
        assert!(store_config.runtime_config.optimize);
        assert_eq!(store_config.runtime_config.cache_size, 500_000);
        assert_eq!(store_config.runtime_config.journal_size_limit, 2_000_000);
    }

    #[test]
    fn test_store_config_when_passphrase() {
        let store_config = SqliteStoreConfig::new(Path::new("foo"))
            .passphrase(Some("bar"))
            .pool_max_size(42)
            .optimize(false)
            .cache_size(43)
            .journal_size_limit(44);

        assert_eq!(store_config.path, PathBuf::from("foo"));
        assert_eq!(store_config.secret, Some(Secret::PassPhrase("bar".to_owned().into())));
        assert_eq!(store_config.pool_config.max_size, 42);
        assert!(store_config.runtime_config.optimize.not());
        assert_eq!(store_config.runtime_config.cache_size, 43);
        assert_eq!(store_config.runtime_config.journal_size_limit, 44);
    }

    #[test]
    fn test_store_config_when_key() {
        let store_config = SqliteStoreConfig::new(Path::new("foo"))
            .key(Some(&[
                143, 27, 202, 78, 96, 55, 13, 149, 247, 8, 33, 120, 204, 92, 171, 66, 19, 238, 61,
                107, 132, 211, 40, 244, 71, 190, 99, 14, 173, 225, 6, 156,
            ]))
            .pool_max_size(42)
            .optimize(false)
            .cache_size(43)
            .journal_size_limit(44);

        assert_eq!(store_config.path, PathBuf::from("foo"));
        assert_eq!(
            store_config.secret,
            Some(Secret::Key(Zeroizing::new(vec![
                143, 27, 202, 78, 96, 55, 13, 149, 247, 8, 33, 120, 204, 92, 171, 66, 19, 238, 61,
                107, 132, 211, 40, 244, 71, 190, 99, 14, 173, 225, 6, 156,
            ])))
        );
        assert_eq!(store_config.pool_config.max_size, 42);
        assert!(store_config.runtime_config.optimize.not());
        assert_eq!(store_config.runtime_config.cache_size, 43);
        assert_eq!(store_config.runtime_config.journal_size_limit, 44);
    }

    #[test]
    fn test_store_config_path() {
        let store_config = SqliteStoreConfig::new(Path::new("foo")).path(Path::new("bar"));

        assert_eq!(store_config.path, PathBuf::from("bar"));
    }

    #[test]
    fn test_pool_size_has_a_minimum() {
        let store_config = SqliteStoreConfig::new(Path::new("foo")).pool_max_size(1);

        assert_eq!(store_config.pool_config.max_size, POOL_MINIMUM_SIZE);
    }
}
