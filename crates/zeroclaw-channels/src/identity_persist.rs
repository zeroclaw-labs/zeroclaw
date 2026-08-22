//! Shared paired-identity persistence for QR-pairing channels.
//!
//! When a QR pairing completes, the linked account identity becomes an
//! authorized external peer. The canonical home for that authorization is
//! a `[peer_groups.<name>]` entry in `config.toml` whose `channel` field
//! is the dotted `<channel_type>.<alias>` instance ref — the same
//! channel-ref contract `Config::channel_external_peers` matches at
//! message-time (the runtime reader never looks at the map key). This
//! module is the single writer for that shape: WeChat and WhatsApp Web
//! both persist through it, so the two channels can never drift into
//! different on-disk layouts (and no channel grows a local allowlist
//! cache).
//!
//! Writes go through the shared `Arc<RwLock<Config>>` handle the
//! orchestrator wires into each channel — mutate canonical in-memory state
//! under the lock, then persist a snapshot with `Config::save()`. Channels
//! constructed without the handle (tests, one-shot CLI runs) skip
//! persistence with a warning: pairing still works for the process
//! lifetime, it just isn't durable.

use std::sync::Arc;
use zeroclaw_config::schema::Config;

/// Merge `identity` into the `external_peers` of the peer group whose
/// `channel` ref matches `<channel_type>.<alias>`, on the canonical
/// in-memory config. Returns `true` when the config changed (the caller
/// persists a snapshot), `false` when the identity was already authorized.
///
/// `match_fn` is the calling channel's own identity comparison, the one its
/// admission path applies. Both decisions this writer makes — whether a deny
/// shadows the identity, and whether a grant already admits it — are identity
/// questions, and only the channel can answer them: WhatsApp Web reads
/// `+15551234567`, `15551234567` and `15551234567@s.whatsapp.net` as one
/// account, WeChat distinguishes case. A generic comparison here is narrower
/// than one channel's rule and wider than another's, and in both directions it
/// breaks the invariant the writer exists to hold: **a write or a no-op must
/// leave the identity admissible under the same resolved policy, otherwise the
/// conflict is returned.** Report a pairing the admission matcher then
/// rejects, and the operator is left with no route back.
///
/// The merge target is chosen by the same channel-ref contract the runtime
/// reader (`Config::channel_external_peers`) authorizes by — the group's
/// `channel` field, never the `peer_groups` map key:
///
/// - If a matching group's `ignore` denies the identity, the merge is
///   rejected: the deny outranks any grant, so appending one would persist
///   an entry the admission matcher immediately shadows and report a
///   pairing that cannot work.
/// - If the identity is already authorized for `<channel_type>.<alias>`
///   through *any* matching group (instance-scoped or type-wide), nothing
///   is written.
/// - Otherwise the identity is appended to an existing group whose
///   `channel` is exactly `<channel_type>.<alias>` (preferring the
///   conventional `<channel_type>_<alias>` key when several match).
/// - When no group matches, a new group is created under the conventional
///   `<channel_type>_<alias>` key with `channel = "<channel_type>.<alias>"`
///   — the shape WeChat pairing established. If that key is already taken
///   by a group whose `channel` points elsewhere, the merge is rejected:
///   appending there would store the identity where the reader for this
///   channel never looks (and another channel's reader would pick it up).
///
/// Existing group entries (agents, other peers) are preserved.
pub(crate) fn merge_external_peer(
    cfg: &mut Config,
    channel_type: &str,
    alias: &str,
    identity: &str,
    match_fn: impl Fn(&str, &str) -> bool,
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

    let resolved = cfg.channel_external_peers(channel_type, alias);

    if let Some(conflict) = crate::allowlist::pairing_deny_conflict(
        &resolved,
        channel_type,
        alias,
        normalized,
        &match_fn,
    ) {
        anyhow::bail!(conflict);
    }

    // Already authorized through any group the reader matches (including
    // type-wide groups)? Then there is nothing to persist. This asks the same
    // question the runtime asks, through the same helper and the same matcher,
    // so writer and reader cannot disagree about what "already authorized"
    // means. Comparing the resolved entries by string would not: a grant this
    // identity matches may sit alongside an `ignore` that denies it, and
    // pairing must not report an ignored identity as authorized.
    if crate::allowlist::is_user_allowed_by(&resolved, normalized, &match_fn) {
        return Ok(false);
    }

    let dotted_ref = format!("{channel_type}.{alias}");
    let conventional_key = format!("{channel_type}_{alias}");

    // Append to the instance-scoped group the reader will match. Prefer
    // the conventional key when several groups carry the same dotted ref,
    // then fall back to the lexicographically first for determinism
    // (peer_groups is a HashMap; iteration order is unspecified).
    let target_key = if cfg
        .peer_groups
        .get(&conventional_key)
        .is_some_and(|group| group.channel.as_str() == dotted_ref)
    {
        Some(conventional_key.clone())
    } else {
        cfg.peer_groups
            .iter()
            .filter(|(_, group)| group.channel.as_str() == dotted_ref)
            .map(|(key, _)| key.clone())
            .min()
    };

    if let Some(key) = target_key {
        // Invariant: `target_key` was selected from existing map entries.
        if let Some(group) = cfg.peer_groups.get_mut(&key) {
            group
                .external_peers
                .push(PeerUsername::new(normalized.to_string()));
        }
        return Ok(true);
    }

    // No group carries this channel's dotted ref yet — create the
    // conventional shape. Refuse to squat on a key that belongs to a
    // different channel: writing there would put the identity where this
    // channel's reader never looks, while the *other* channel's reader
    // would silently start authorizing it.
    if let Some(existing) = cfg.peer_groups.get(&conventional_key) {
        anyhow::bail!(
            "peer group [{conventional_key}] already exists but its channel ref \
             is `{}` (expected `{dotted_ref}`) — fix the group key or channel ref \
             in config.toml before pairing",
            existing.channel.as_str()
        );
    }
    cfg.peer_groups.insert(
        conventional_key,
        PeerGroupConfig {
            channel: ChannelRef::new(dotted_ref),
            external_peers: vec![PeerUsername::new(normalized.to_string())],
            ..PeerGroupConfig::default()
        },
    );
    Ok(true)
}

/// Persist a paired identity as an authorized external peer.
///
/// Mutates the shared canonical config under the write lock via
/// [`merge_external_peer`], then saves a snapshot to `config.toml`.
/// Idempotent: an already-authorized identity returns without writing, so
/// callers may invoke this on every connect/reconnect. `persist = None`
/// (no handle wired) warns and succeeds without persisting. `match_fn` is the
/// channel's own admission comparison; see [`merge_external_peer`].
pub(crate) async fn persist_external_peer(
    persist: Option<&Arc<parking_lot::RwLock<Config>>>,
    channel_type: &str,
    alias: &str,
    identity: &str,
    match_fn: impl Fn(&str, &str) -> bool,
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
        if !merge_external_peer(&mut cfg, channel_type, alias, identity, match_fn)? {
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

    /// Stands in for a channel whose identities are compared verbatim, for the
    /// group-selection tests where the matcher is not what is under test.
    fn exact(entry: &str, user: &str) -> bool {
        entry == user
    }

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

        let changed = merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact)
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

        assert!(
            merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact).unwrap()
        );
        assert!(
            !merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact).unwrap(),
            "an already-authorized identity is not re-added"
        );
        assert!(
            merge_external_peer(&mut config, "whatsapp", "admin", "+15559876543", exact).unwrap(),
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

        assert!(
            merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact).unwrap()
        );
        let group = config.peer_groups.get("whatsapp_admin").unwrap();
        assert_eq!(group.agents.len(), 1, "agent bindings survive the merge");
        assert_eq!(group.external_peers.len(), 2);
    }

    #[test]
    fn merge_rejects_empty_identity_and_unconfigured_channel() {
        let mut config = config_with_whatsapp("admin");
        assert!(merge_external_peer(&mut config, "whatsapp", "admin", "  ", exact).is_err());
        assert!(
            merge_external_peer(&mut config, "whatsapp", "ghost", "+15551234567", exact).is_err(),
            "an alias with no [channels.whatsapp.ghost] block is rejected"
        );
        assert!(
            merge_external_peer(&mut config, "telegram", "admin", "someone", exact).is_err(),
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

        assert!(merge_external_peer(&mut config, "telegram", "admin", "someone", exact).unwrap());
        let group = config
            .peer_groups
            .get("telegram_admin")
            .expect("group created under <type>_<alias>");
        assert_eq!(group.channel.as_str(), "telegram.admin");
    }

    #[test]
    fn merge_surfaces_a_conflict_instead_of_persisting_a_shadowed_grant() {
        use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};
        use zeroclaw_config::providers::ChannelRef;

        // The end of the recovery path: pairing is available again once a
        // fully shadowed grant no longer counts as authorization, so it must
        // not dead-end by appending a grant the deny immediately shadows.
        let mut config = config_with_whatsapp("admin");
        config.peer_groups.insert(
            "whatsapp_admin".to_string(),
            PeerGroupConfig {
                channel: ChannelRef::new("whatsapp.admin".to_string()),
                external_peers: vec![PeerUsername::new("+15551234567".to_string())],
                ignore: vec![PeerUsername::new("+15551234567".to_string())],
                ..Default::default()
            },
        );

        let err = merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact)
            .expect_err("an ignored identity must not be persisted as a grant");
        let message = err.to_string();
        assert!(
            message.contains("ignore"),
            "names the field to edit: {message}"
        );
        assert!(
            !message.contains("+15551234567"),
            "the identity is personal data and callers log this error: {message}"
        );

        let group = config.peer_groups.get("whatsapp_admin").unwrap();
        assert_eq!(
            group.external_peers.len(),
            1,
            "no second, equally shadowed grant was appended"
        );
        assert!(
            !crate::allowlist::is_user_allowed_by(
                &config.channel_external_peers("whatsapp", "admin"),
                "+15551234567",
                exact,
            ),
            "the operator's ignore stays authoritative"
        );

        // With the ignore removed, the same pairing attempt succeeds and the
        // identity is genuinely admissible: the recovery path terminates.
        config
            .peer_groups
            .get_mut("whatsapp_admin")
            .unwrap()
            .ignore
            .clear();
        assert!(
            !merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact)
                .expect("merge succeeds once the deny is gone"),
            "the existing grant is now effective, so nothing needs writing"
        );
        assert!(crate::allowlist::is_user_allowed_by(
            &config.channel_external_peers("whatsapp", "admin"),
            "+15551234567",
            exact,
        ));
    }

    #[test]
    fn merge_rejects_conventional_key_with_mismatched_channel_ref() {
        use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};
        use zeroclaw_config::providers::ChannelRef;

        // A stale/hand-edited [peer_groups.whatsapp_admin] that points at a
        // different channel must not silently receive the WhatsApp identity:
        // whatsapp.admin's reader would never see it, telegram.admin's would.
        let mut config = config_with_whatsapp("admin");
        config.peer_groups.insert(
            "whatsapp_admin".to_string(),
            PeerGroupConfig {
                channel: ChannelRef::new("telegram.admin".to_string()),
                external_peers: vec![PeerUsername::new("someone".to_string())],
                ..Default::default()
            },
        );

        let err = merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact)
            .expect_err("mismatched channel ref must be rejected");
        assert!(err.to_string().contains("telegram.admin"));

        let group = config.peer_groups.get("whatsapp_admin").unwrap();
        assert_eq!(group.channel.as_str(), "telegram.admin", "group untouched");
        assert_eq!(group.external_peers.len(), 1, "no identity appended");
        assert!(
            config
                .channel_external_peers("whatsapp", "admin")
                .is_empty(),
            "the identity must not be stored anywhere the reader matches"
        );
    }

    #[test]
    fn merge_targets_group_by_channel_ref_not_map_key() {
        use zeroclaw_config::multi_agent::PeerGroupConfig;
        use zeroclaw_config::providers::ChannelRef;

        // The reader authorizes by the group's `channel` field, so the
        // writer appends to the group carrying the dotted ref even when it
        // lives under a non-conventional key — and creates no second group.
        let mut config = config_with_whatsapp("admin");
        config.peer_groups.insert(
            "my_custom_group".to_string(),
            PeerGroupConfig {
                channel: ChannelRef::new("whatsapp.admin".to_string()),
                ..Default::default()
            },
        );

        assert!(
            merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact).unwrap()
        );
        assert!(
            !config.peer_groups.contains_key("whatsapp_admin"),
            "no duplicate conventional group is created"
        );
        assert_eq!(
            config
                .peer_groups
                .get("my_custom_group")
                .unwrap()
                .external_peers
                .len(),
            1
        );
        assert_eq!(
            config.channel_external_peers("whatsapp", "admin"),
            vec!["+15551234567".to_string()],
            "the reader sees the persisted identity"
        );
    }

    #[test]
    fn merge_appends_to_matching_group_even_when_conventional_key_is_taken() {
        use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};
        use zeroclaw_config::providers::ChannelRef;

        // The mismatched conventional key only blocks *creation*. When some
        // other group already carries this channel's dotted ref, the append
        // goes there and the foreign group is left untouched.
        let mut config = config_with_whatsapp("admin");
        config.peer_groups.insert(
            "whatsapp_admin".to_string(),
            PeerGroupConfig {
                channel: ChannelRef::new("telegram.admin".to_string()),
                external_peers: vec![PeerUsername::new("someone".to_string())],
                ..Default::default()
            },
        );
        config.peer_groups.insert(
            "renamed_whatsapp_group".to_string(),
            PeerGroupConfig {
                channel: ChannelRef::new("whatsapp.admin".to_string()),
                ..Default::default()
            },
        );

        assert!(
            merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact).unwrap()
        );
        assert_eq!(
            config
                .peer_groups
                .get("renamed_whatsapp_group")
                .unwrap()
                .external_peers
                .iter()
                .map(|p| p.as_str().to_string())
                .collect::<Vec<_>>(),
            vec!["+15551234567".to_string()],
            "identity lands in the group whose channel ref matches"
        );
        let foreign = config.peer_groups.get("whatsapp_admin").unwrap();
        assert_eq!(foreign.channel.as_str(), "telegram.admin");
        assert_eq!(
            foreign.external_peers.len(),
            1,
            "the foreign group under the conventional key is untouched"
        );
        assert_eq!(
            config.channel_external_peers("whatsapp", "admin"),
            vec!["+15551234567".to_string()],
            "the reader authorizes the identity for this channel"
        );
    }

    #[test]
    fn merge_prefers_the_conventional_key_when_several_groups_match() {
        use zeroclaw_config::multi_agent::PeerGroupConfig;
        use zeroclaw_config::providers::ChannelRef;

        // Two instance-scoped groups both carry `whatsapp.admin`; the append
        // deterministically targets the conventional `<type>_<alias>` key.
        let mut config = config_with_whatsapp("admin");
        for key in ["whatsapp_admin", "another_group"] {
            config.peer_groups.insert(
                key.to_string(),
                PeerGroupConfig {
                    channel: ChannelRef::new("whatsapp.admin".to_string()),
                    ..Default::default()
                },
            );
        }

        assert!(
            merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact).unwrap()
        );
        assert_eq!(
            config
                .peer_groups
                .get("whatsapp_admin")
                .unwrap()
                .external_peers
                .len(),
            1,
            "conventional key receives the append"
        );
        assert!(
            config
                .peer_groups
                .get("another_group")
                .unwrap()
                .external_peers
                .is_empty(),
            "the other matching group is untouched"
        );
    }

    #[test]
    fn merge_is_a_noop_when_a_type_wide_group_already_authorizes() {
        use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};
        use zeroclaw_config::providers::ChannelRef;

        // A bare-type group (`channel = "whatsapp"`) already authorizes the
        // identity for every alias of the type; the reader-driven
        // idempotency check must catch that and write nothing.
        let mut config = config_with_whatsapp("admin");
        config.peer_groups.insert(
            "whatsapp_everyone".to_string(),
            PeerGroupConfig {
                channel: ChannelRef::new("whatsapp".to_string()),
                external_peers: vec![PeerUsername::new("+15551234567".to_string())],
                ..Default::default()
            },
        );

        assert!(
            !merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact).unwrap(),
            "already authorized via the type-wide group"
        );
        assert_eq!(config.peer_groups.len(), 1, "no new group created");
    }

    #[test]
    fn merge_does_not_treat_an_ignored_identity_as_already_authorized() {
        use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};
        use zeroclaw_config::providers::ChannelRef;

        // The resolved list carries grants and denies together, so a grant this
        // identity matches can sit beside an `ignore` that denies it. Comparing
        // the entries by string would find the grant and report the identity as
        // already authorized, which is the opposite of the truth. The deny
        // reaches here through a type-wide group, not the instance-scoped one.
        let mut config = config_with_whatsapp("admin");
        config.peer_groups.insert(
            "whatsapp_everyone".to_string(),
            PeerGroupConfig {
                channel: ChannelRef::new("whatsapp".to_string()),
                external_peers: vec![PeerUsername::new("+15551234567".to_string())],
                ignore: vec![PeerUsername::new("+15551234567".to_string())],
                ..Default::default()
            },
        );

        // Neither of the two silent outcomes is right: `Ok(false)` would claim
        // the identity is already authorized, and `Ok(true)` would append a
        // grant the deny shadows just as thoroughly.
        merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact)
            .expect_err("an ignored identity is neither authorized nor grantable by appending");
        assert_eq!(
            config.peer_groups.len(),
            1,
            "no group was created to hold a shadowed grant"
        );
    }

    #[tokio::test]
    async fn persist_without_handle_warns_and_returns_ok() {
        persist_external_peer(None, "whatsapp", "admin", "+15551234567", exact)
            .await
            .expect("missing handle is a soft no-op");
    }
}
