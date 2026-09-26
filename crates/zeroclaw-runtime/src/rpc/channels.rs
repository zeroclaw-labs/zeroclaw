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
    /// and swapping `config` under `config_write_lock`.
    async fn bind(
        &self,
        config: &Arc<RwLock<Config>>,
        config_write_lock: &Arc<tokio::sync::Mutex<()>>,
        channel_type: &str,
        alias: &str,
        identity: &str,
    ) -> Result<Value, JsonRpcError>;
}
