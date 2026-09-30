//! `channels/{list,relink,bind}`: channel operations the core serves but
//! cannot perform itself.
//!
//! Listing readiness, relinking a QR-pairing session and binding an identity
//! reach channel internals, and the channels crate depends on this one, so
//! the operations are a capability: the process registers a
//! [`ChannelControl`] with the daemon, which puts it in the RPC context. A
//! process that runs no channels registers none, and the methods say so.

use std::sync::Arc;

use parking_lot::RwLock;
use serde_json::Value;
use zeroclaw_api::jsonrpc::JsonRpcError;
use zeroclaw_config::pairing::PairingGuard;
use zeroclaw_config::schema::Config;

/// The channel operations behind `channels/*`. Each returns the body the
/// matching dashboard route serves, or the JSON-RPC error for its refusal.
#[async_trait::async_trait]
pub trait ChannelControl: Send + Sync {
    /// Every configured channel alias with its owning agent and readiness.
    fn list(&self, config: &Config, pairing: &PairingGuard) -> Value;

    /// Clear a QR-pairing channel's persisted login; `channel` is the
    /// composite `<type>.<alias>` name `list` reports.
    fn relink(&self, config: &Config, channel: &str) -> Result<Value, JsonRpcError>;

    /// Authorize `identity` on one channel alias's peer allowlist, saving
    /// and swapping `config`. The caller holds the config write lock and has
    /// rechecked its authority under it; `config_write_guard` is that guard.
    ///
    /// Before anything is saved, `authorize_write` is called with the
    /// concrete config path the bind will write and the verb its effect
    /// needs, `Create` when the peer group is new and `Update` otherwise, and
    /// a refusal stops it. An identity that is already bound writes nothing
    /// and is not checked.
    async fn bind(
        &self,
        config: &Arc<RwLock<Config>>,
        config_write_guard: &tokio::sync::OwnedMutexGuard<()>,
        channel_type: &str,
        alias: &str,
        identity: &str,
        authorize_write: &(
             dyn for<'p> Fn(&'p str, zeroclaw_api::grants::Verb) -> Result<(), JsonRpcError>
                 + Send
                 + Sync
         ),
    ) -> Result<Value, JsonRpcError>;
}
