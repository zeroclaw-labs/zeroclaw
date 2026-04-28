//! Configuration schema, secrets, and related types for ZeroClaw.

pub mod api_error;
pub mod autonomy;
pub mod comment_writer;
pub mod cost;
pub mod domain_matcher;
pub mod helpers;
pub mod migration;
pub mod pairing;
pub mod platform;
pub mod policy;
pub mod provider_aliases;
pub mod providers;
pub mod scattered_types;
pub mod schema;
pub mod secrets;
pub mod traits;
pub mod typed_value;
pub mod workspace;

/// Shim module so `Configurable` derive macro's generated `crate::config::*` paths resolve.
/// The macro was written assuming it runs inside the root crate where `mod config` exists.
pub mod config {
    pub use crate::helpers::*;
    pub use crate::traits::*;
}

/// Shim module so `Configurable` derive macro's generated `crate::security::*` paths resolve.
pub mod security {
    pub use crate::policy::SecurityPolicy;
    pub use crate::secrets::SecretStore;
}
