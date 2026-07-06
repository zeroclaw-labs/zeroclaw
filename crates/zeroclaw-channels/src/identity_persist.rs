//! Shared paired-identity persistence for QR-pairing channels.
//!
//! When a QR pairing completes, the linked account identity becomes an
//! authorized external peer. The canonical home for that authorization is
//! `peer_groups.<channel_type>_<alias>.external_peers` in `config.toml` —
//! the same state the channels' `peer_resolver` closures read at
//! message-time. This module is the single writer for that shape: WeChat
//! and WhatsApp Web both persist through it, so the two channels can never
//! drift into different on-disk layouts (and no channel grows a local
//! allowlist cache).
//!
//! Writes go through the shared `Arc<RwLock<Config>>` handle the
//! orchestrator wires into each channel — mutate canonical in-memory state
//! under the lock, then persist a snapshot with `Config::save()`. Channels
//! constructed without the handle (tests, one-shot CLI runs) skip
//! persistence with a warning: pairing still works for the process
//! lifetime, it just isn't durable.

use std::sync::Arc;
use zeroclaw_config::schema::Config;

/// Merge `identity` into `peer_groups.<channel_type>_<alias>.external_peers`
/// on the canonical in-memory config. Returns `true` when the config
/// changed (the caller persists a snapshot), `false` when the identity was
/// already authorized.
///
/// The peer-group shape is the one WeChat pairing established: group key
/// `<channel_type>_<alias>`, `channel` ref `<channel_type>.<alias>`, and
/// the identity appended to `external_peers`. Existing group entries
/// (agents, other peers) are preserved.
pub(crate) fn merge_external_peer(
    cfg: &mut Config,
    channel_type: &str,
    alias: &str,
    identity: &str,
) -> anyhow::Result<bool> {
    use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};
    use zeroclaw_config::providers::ChannelRef;

    let normalized = identity.trim();
    if normalized.is_empty() {
        anyhow::bail!("Cannot persist empty {channel_type} identity");
    }
    // Existence comes from the canonical channel registry
    // (`Config::channels_by_alias()` walks every configured
    // `[channels.<type>.<alias>]` block regardless of type), so this writer
    // holds no channel-type list of its own and a future QR-pairing channel
    // needs no edit here.
    let configured = cfg
        .channels_by_alias()
        .iter()
        .any(|info| info.channel_type == channel_type && info.alias == alias);
    if !configured {
        anyhow::bail!(
            "Missing [channels.{channel_type}.{alias}] section in config.toml — \
             configure the channel before pairing"
        );
    }
    let group = cfg
        .peer_groups
        .entry(format!("{channel_type}_{alias}"))
        .or_insert_with(|| PeerGroupConfig {
            channel: ChannelRef::new(format!("{channel_type}.{alias}")),
            ..PeerGroupConfig::default()
        });
    if group
        .external_peers
        .iter()
        .any(|peer| peer.as_str() == normalized)
    {
        return Ok(false);
    }
    group
        .external_peers
        .push(PeerUsername::new(normalized.to_string()));
    Ok(true)
}

/// Persist a paired identity as an authorized external peer.
///
/// Mutates the shared canonical config under the write lock via
/// [`merge_external_peer`], then saves a snapshot to `config.toml`.
/// Idempotent: an already-authorized identity returns without writing, so
/// callers may invoke this on every connect/reconnect. `persist = None`
/// (no handle wired) warns and succeeds without persisting.
pub(crate) async fn persist_external_peer(
    persist: Option<&Arc<parking_lot::RwLock<Config>>>,
    channel_type: &str,
    alias: &str,
    identity: &str,
) -> anyhow::Result<()> {
    use anyhow::Context;

    let Some(config) = persist else {
        // The raw identity is a durable personal identifier (e.g. a phone
        // number) and must not reach the log sink; channel_type/alias give
        // the operator enough to locate the unwired constructor path.
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "channel_type": channel_type,
                    "alias": alias,
                })),
            "paired identity not persisted (no persistence handle wired)"
        );
        return Ok(());
    };
    let snapshot = {
        let mut cfg = config.write();
        if !merge_external_peer(&mut cfg, channel_type, alias, identity)? {
            return Ok(());
        }
        cfg.clone()
    };
    snapshot
        .save()
        .await
        .with_context(|| format!("Failed to persist {channel_type} peer to config.toml"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_whatsapp(alias: &str) -> Config {
        let mut config = Config::default();
        config.channels.whatsapp.insert(
            alias.to_string(),
            zeroclaw_config::schema::WhatsAppConfig {
                enabled: true,
                ..Default::default()
            },
        );
        config
    }

    #[test]
    fn merge_creates_group_in_the_wechat_shape() {
        let mut config = config_with_whatsapp("admin");

        let changed = merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567")
            .expect("merge succeeds");
        assert!(changed);

        let group = config
            .peer_groups
            .get("whatsapp_admin")
            .expect("group created under <type>_<alias>");
        assert_eq!(group.channel.as_str(), "whatsapp.admin");
        assert_eq!(
            group
                .external_peers
                .iter()
                .map(|p| p.as_str().to_string())
                .collect::<Vec<_>>(),
            vec!["+15551234567".to_string()]
        );
    }

    #[test]
    fn merge_is_idempotent_and_additive() {
        let mut config = config_with_whatsapp("admin");

        assert!(merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567").unwrap());
        assert!(
            !merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567").unwrap(),
            "an already-authorized identity is not re-added"
        );
        assert!(
            merge_external_peer(&mut config, "whatsapp", "admin", "+15559876543").unwrap(),
            "a second identity extends the same group"
        );
        assert_eq!(
            config
                .peer_groups
                .get("whatsapp_admin")
                .unwrap()
                .external_peers
                .len(),
            2
        );
    }

    #[test]
    fn merge_preserves_existing_group_membership() {
        use zeroclaw_config::multi_agent::{AgentAlias, PeerGroupConfig, PeerUsername};
        use zeroclaw_config::providers::ChannelRef;

        let mut config = config_with_whatsapp("admin");
        config.peer_groups.insert(
            "whatsapp_admin".to_string(),
            PeerGroupConfig {
                channel: ChannelRef::new("whatsapp.admin".to_string()),
                agents: vec![AgentAlias::new("rowan".to_string())],
                external_peers: vec![PeerUsername::new("+15550000000".to_string())],
                ..Default::default()
            },
        );

        assert!(merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567").unwrap());
        let group = config.peer_groups.get("whatsapp_admin").unwrap();
        assert_eq!(group.agents.len(), 1, "agent bindings survive the merge");
        assert_eq!(group.external_peers.len(), 2);
    }

    #[test]
    fn merge_rejects_empty_identity_and_unconfigured_channel() {
        let mut config = config_with_whatsapp("admin");
        assert!(merge_external_peer(&mut config, "whatsapp", "admin", "  ").is_err());
        assert!(
            merge_external_peer(&mut config, "whatsapp", "ghost", "+15551234567").is_err(),
            "an alias with no [channels.whatsapp.ghost] block is rejected"
        );
        assert!(
            merge_external_peer(&mut config, "telegram", "admin", "someone").is_err(),
            "a type/alias pair with no configured block is rejected"
        );
        assert!(
            config.peer_groups.is_empty(),
            "failed merges must not leave partial groups behind"
        );
    }

    #[test]
    fn merge_accepts_any_configured_channel_type_via_the_registry() {
        // The existence check reads the canonical channel registry, not a
        // hardcoded channel-type list: a configured channel of any type can
        // persist a paired identity without this module needing an edit.
        let mut config = Config::default();
        config.channels.telegram.insert(
            "admin".to_string(),
            zeroclaw_config::schema::TelegramConfig {
                enabled: true,
                ..Default::default()
            },
        );

        assert!(merge_external_peer(&mut config, "telegram", "admin", "someone").unwrap());
        let group = config
            .peer_groups
            .get("telegram_admin")
            .expect("group created under <type>_<alias>");
        assert_eq!(group.channel.as_str(), "telegram.admin");
    }

    #[tokio::test]
    async fn persist_without_handle_warns_and_returns_ok() {
        persist_external_peer(None, "whatsapp", "admin", "+15551234567")
            .await
            .expect("missing handle is a soft no-op");
    }
}
