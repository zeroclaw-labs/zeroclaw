// Copyright 2023 The Matrix.org Foundation C.I.C.
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

#[cfg(feature = "event-cache-store")]
use std::sync::Arc;

#[cfg(feature = "event-cache-store")]
use matrix_sdk_base::event_cache::store::EventCacheStoreError;
#[cfg(feature = "media-store")]
use matrix_sdk_base::media::store::MediaStoreError;
#[cfg(feature = "state-store")]
use matrix_sdk_base::store::StoreError as StateStoreError;
#[cfg(feature = "crypto-store")]
use matrix_sdk_crypto::CryptoStoreError;
use thiserror::Error;
use tokio::io;

use crate::connection::{CreatePoolError, PoolError};

/// All the errors that can occur when opening an SQLite store.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum OpenStoreError {
    /// Failed to create the DB's parent directory.
    #[error("Failed to create the database's parent directory: {0}")]
    CreateDir(#[source] io::Error),

    /// Failed to create the DB pool.
    #[error(transparent)]
    CreatePool(#[from] CreatePoolError),

    /// Failed to load the database's version.
    #[error("Failed to load database version: {0}")]
    LoadVersion(#[source] rusqlite::Error),

    /// The version of the database is missing.
    #[error("Missing database version")]
    MissingVersion,

    /// The version of the database is invalid.
    #[error("Invalid database version")]
    InvalidVersion,

    /// Failed to apply migrations.
    #[error("Failed to run migrations: {0}")]
    Migration(#[from] Error),

    /// Failed to get a DB connection from the pool.
    #[error(transparent)]
    Pool(#[from] PoolError),

    /// Failed to initialize the store cipher.
    #[error("Failed to initialize the store cipher: {0}")]
    InitCipher(#[from] matrix_sdk_store_encryption::Error),

    /// Failed to load the store cipher from the DB.
    #[error("Failed to load the store cipher from the DB: {0}")]
    LoadCipher(#[source] rusqlite::Error),

    /// Failed to save the store cipher to the DB.
    #[error("Failed to save the store cipher to the DB: {0}")]
    SaveCipher(#[source] rusqlite::Error),
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(rusqlite::Error),

    #[error("Failed to compute the maximum variable number from {0}")]
    SqliteMaximumVariableNumber(i32),

    #[error(transparent)]
    Pool(PoolError),

    #[error(transparent)]
    Encode(rmp_serde::encode::Error),

    #[error(transparent)]
    Decode(rmp_serde::decode::Error),

    #[error(transparent)]
    DecodeString(#[from] std::string::FromUtf8Error),

    #[error(transparent)]
    DecodeStr(#[from] std::str::Utf8Error),

    #[error(transparent)]
    DecodeRuma(#[from] ruma::IdParseError),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Encryption(matrix_sdk_store_encryption::Error),

    #[error("can't save/load sessions or group sessions in the store before an account is stored")]
    AccountUnset,

    #[error(transparent)]
    Pickle(#[from] vodozemac::PickleError),

    #[error("An object failed to be decrypted while unpickling")]
    Unpickle,

    #[error("Redaction failed: {0}")]
    Redaction(#[source] ruma::canonical_json::CanonicalJsonFieldError),

    #[error("An update keyed by unique ID touched more than one entry")]
    InconsistentUpdate,

    #[error("The store contains invalid data: {details}")]
    InvalidData { details: String },

    #[error("The store is closed")]
    StoreClosed,
}

macro_rules! impl_from {
    ( $ty:ty => $enum:ident::$variant:ident ) => {
        impl From<$ty> for $enum {
            fn from(value: $ty) -> Self {
                Self::$variant(value)
            }
        }
    };
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        if let rusqlite::Error::SqliteFailure(ffi_error, message) = &error
            && ffi_error.code == rusqlite::ErrorCode::DatabaseBusy
        {
            // Report to sentry.
            tracing::error!(
                sentry = true,
                sqlite_message = message,
                "observed database busy error"
            );
        }
        Error::Sqlite(error)
    }
}

impl_from!(PoolError => Error::Pool);
impl_from!(rmp_serde::encode::Error => Error::Encode);
impl_from!(rmp_serde::decode::Error => Error::Decode);
impl_from!(matrix_sdk_store_encryption::Error => Error::Encryption);

#[cfg(feature = "crypto-store")]
impl From<Error> for CryptoStoreError {
    fn from(e: Error) -> Self {
        CryptoStoreError::backend(e)
    }
}

#[cfg(feature = "state-store")]
impl From<Error> for StateStoreError {
    fn from(e: Error) -> Self {
        match e {
            Error::Json(e) => StateStoreError::Json(e),
            Error::Encryption(e) => StateStoreError::Encryption(e),
            Error::Redaction(e) => StateStoreError::Redaction(e),
            e => StateStoreError::backend(e),
        }
    }
}

#[cfg(feature = "event-cache-store")]
impl From<Error> for EventCacheStoreError {
    fn from(e: Error) -> Self {
        match e {
            Error::Encryption(e) => EventCacheStoreError::Encryption(Arc::new(e)),
            e => EventCacheStoreError::backend(e),
        }
    }
}

#[cfg(feature = "media-store")]
impl From<Error> for MediaStoreError {
    fn from(e: Error) -> Self {
        match e {
            Error::Encryption(e) => MediaStoreError::Encryption(e),
            e => MediaStoreError::backend(e),
        }
    }
}

pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;
