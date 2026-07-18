//! Microsoft Teams bot channel (Azure Bot Service / Bot Framework).
//!
//! Inbound: Teams POSTs Bot Framework activities to a channel-hosted HTTP
//! listener (the operator registers its public URL as the Azure Bot
//! messaging endpoint). Outbound: proactive POSTs to the Bot Connector API
//! at the `service_url` carried by each inbound activity.
//!
//! This module is currently a wiring stub: config schema, feature flag, and
//! orchestrator plumbing land first; the listener, Connector client, and
//! JWT validation land in follow-up PRs (see
//! `docs/msteams-channel-design.md`).

pub mod auth;

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::MSTeamsConfig;

/// Resolves this alias's `MSTeamsConfig` from canonical config state at
/// use-time. No snapshot is stored on the channel (see AGENTS.md
/// "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH"): credentials, `allow_dms`,
/// `mention_only`, and `allow_from` are all read through this resolver so
/// a config reload is observed on the next message.
pub type ConfigResolver = Arc<dyn Fn() -> Option<MSTeamsConfig> + Send + Sync>;

/// Microsoft Teams channel handle.
pub struct MsTeamsChannel {
    /// The alias key under `[channels.msteams.<alias>]` this handle is
    /// bound to.
    alias: String,
    /// Resolves the alias's config block from canonical state at use-time.
    config_resolver: ConfigResolver,
    /// Resolves inbound external peers from canonical state at message-time.
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
}

impl MsTeamsChannel {
    pub fn new(
        alias: impl Into<String>,
        config_resolver: ConfigResolver,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self {
            alias: alias.into(),
            config_resolver,
            peer_resolver,
        }
    }

    /// Current config for this alias, resolved from canonical state.
    fn config(&self) -> Option<MSTeamsConfig> {
        (self.config_resolver)()
    }

    /// Current external peers for this alias, resolved from canonical state.
    fn external_peers(&self) -> Vec<String> {
        (self.peer_resolver)()
    }
}

impl ::zeroclaw_api::attribution::Attributable for MsTeamsChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::MsTeams,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for MsTeamsChannel {
    fn name(&self) -> &str {
        "msteams"
    }

    async fn send(&self, _message: &SendMessage) -> Result<()> {
        let _ = (self.config(), self.external_peers());
        anyhow::bail!(
            "Microsoft Teams channel '{}' is not implemented yet: outbound \
             Connector API support lands in a follow-up PR",
            self.alias,
        )
    }

    async fn listen(&self, _tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        anyhow::bail!(
            "Microsoft Teams channel '{}' is not implemented yet: the inbound \
             activity listener lands in a follow-up PR",
            self.alias,
        )
    }

    async fn health_check(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::attribution::Attributable;

    fn stub_channel() -> MsTeamsChannel {
        MsTeamsChannel::new(
            "default",
            Arc::new(|| Some(MSTeamsConfig::default())),
            Arc::new(Vec::new),
        )
    }

    #[test]
    fn name_and_attribution() {
        let ch = stub_channel();
        assert_eq!(ch.name(), "msteams");
        assert_eq!(Attributable::alias(&ch), "default");
        assert!(matches!(
            ch.role(),
            zeroclaw_api::attribution::Role::Channel(
                zeroclaw_api::attribution::ChannelKind::MsTeams
            )
        ));
    }

    #[test]
    fn resolvers_read_canonical_state() {
        let ch = stub_channel();
        assert!(ch.config().is_some());
        assert!(ch.external_peers().is_empty());
    }

    #[tokio::test]
    async fn stub_send_and_listen_bail() {
        let ch = stub_channel();
        let err = ch.send(&SendMessage::new("hi", "conv")).await.unwrap_err();
        assert!(err.to_string().contains("not implemented"));

        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let err = ch.listen(tx).await.unwrap_err();
        assert!(err.to_string().contains("not implemented"));

        assert!(!ch.health_check().await);
    }
}
