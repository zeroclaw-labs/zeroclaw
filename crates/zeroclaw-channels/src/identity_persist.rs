//! Shared paired-identity persistence for QR-pairing channels.
//!
//! When a QR pairing completes, the linked account identity becomes an
//! authorized external peer. The canonical home for that authorization is
//! a `[peer_groups.<name>]` entry in `config.toml` whose `channel` field
//! is the dotted `<channel_type>.<alias>` instance ref — the same
//! channel-ref contract `Config::channel_external_peers` matches at
//! message-time (the runtime reader never looks at the map key). This
//! module is the single writer for that shape: WeChat, WhatsApp Web,
//! Telegram and LINE all persist through it, so no two channels can drift
//! into different on-disk layouts (and no channel grows a local allowlist
//! cache).
//!
//! Writes go through the shared `Arc<RwLock<Config>>` handle the
//! orchestrator wires into each channel — stage the merge on a clone, save
//! it, and publish to the canonical in-memory state only once the durable
//! write has succeeded, so a failed save cannot leave a grant the file never
//! received. Channels
//! constructed without the handle (tests, one-shot CLI runs) skip
//! persistence with a warning: pairing still works for the process
//! lifetime, it just isn't durable.

use std::sync::Arc;
use zeroclaw_config::schema::Config;

/// The conflict message when a matching `ignore` already denies `identity`,
/// or `None` when a pairing write would take effect.
///
/// This is the deny half of `merge_external_peer`'s contract, split out so a
/// handler can ask it *before* the pairing transition. `PairingGuard::try_pair`
/// consumes the one-time code and mints a bearer token; discovering the deny
/// only after that leaves the operator's single code spent on a pairing that
/// can never admit the sender, with `pairing_code_active()` false and no route
/// to retry. The writer delegates here too, so the pre-check and the write
/// cannot disagree about what "denied" means.
pub(crate) fn external_peer_deny_conflict(
    cfg: &Config,
    channel_type: &str,
    alias: &str,
    identities: &[&str],
    match_fn: impl Fn(&str, &str) -> bool,
) -> Option<String> {
    let usable: Vec<&str> = identities
        .iter()
        .map(|identity| identity.trim())
        .filter(|identity| !identity.is_empty())
        .collect();
    if usable.is_empty() {
        return None;
    }
    let resolved = cfg.channel_external_peers(channel_type, alias);
    crate::allowlist::pairing_deny_conflict(&resolved, channel_type, alias, &usable, match_fn)
}

/// The `peer_groups` key whose `channel` is exactly `<channel_type>.<alias>`,
/// which is where a write for that instance lands.
///
/// Prefers the conventional `<channel_type>_<alias>` key when several groups
/// carry the same dotted ref, then falls back to the lexicographically first
/// for determinism (`peer_groups` is a `HashMap`, so iteration order is
/// unspecified). Callers reporting where an identity is bound use this rather
/// than assuming the conventional name: a custom key such as
/// `[peer_groups.ops]` with `channel = "telegram.alerts"` is a legitimate
/// destination, and the conventional name may name nothing at all.
#[must_use]
pub fn instance_group_key(cfg: &Config, channel_type: &str, alias: &str) -> Option<String> {
    let dotted_ref = format!("{channel_type}.{alias}");
    let conventional_key = format!("{channel_type}_{alias}");
    if cfg
        .peer_groups
        .get(&conventional_key)
        .is_some_and(|group| group.channel.as_str() == dotted_ref)
    {
        return Some(conventional_key);
    }
    cfg.peer_groups
        .iter()
        .filter(|(_, group)| group.channel.as_str() == dotted_ref)
        .map(|(key, _)| key.clone())
        .min()
}

/// The `peer_groups` key whose grant actually authorizes `identity` for
/// `<channel_type>.<alias>`.
///
/// [`instance_group_key`] answers a different question: which group a *write*
/// for this instance belongs in. It only matches exact dotted refs, so when an
/// identity is already authorized through a bare type-wide group
/// (`channel = "telegram"`) it returns `None` and a caller that falls back to
/// the conventional `<type>_<alias>` name reports a group that need not exist.
/// That sends an operator to edit the wrong block.
///
/// Type-wide groups are included here because the runtime reader honors them,
/// and the lowest key wins so the answer is stable rather than map-order
/// dependent.
#[must_use]
pub fn authorizing_group_key(
    cfg: &Config,
    channel_type: &str,
    alias: &str,
    identity: &str,
    match_fn: impl Fn(&str, &str) -> bool,
) -> Option<String> {
    let dotted_ref = format!("{channel_type}.{alias}");
    cfg.peer_groups
        .iter()
        .filter(|(_, group)| {
            let channel = group.channel.as_str();
            channel == dotted_ref || channel == channel_type
        })
        .filter(|(_, group)| {
            // The group's own grants only: the caller has already established
            // that the identity is authorized overall, and this asks which
            // block carries the entry that says so.
            let grants: Vec<String> = group
                .external_peers
                .iter()
                .map(|peer| peer.as_str().to_string())
                .collect();
            crate::allowlist::is_user_allowed_by(&grants, identity, &match_fn)
        })
        .map(|(key, _)| key.clone())
        .min()
}

/// Merge `identity` into the `external_peers` of the peer group whose
/// `channel` ref matches `<channel_type>.<alias>`, on the canonical
/// in-memory config. Returns the `peer_groups` key that was written when the
/// config changed (the caller persists a snapshot, and callers that report the
/// destination must report *this* key rather than assuming the conventional
/// one), or `None` when the identity was already authorized.
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
) -> anyhow::Result<Option<String>> {
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

    if let Some(conflict) =
        external_peer_deny_conflict(cfg, channel_type, alias, &[normalized], &match_fn)
    {
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
        return Ok(None);
    }

    let dotted_ref = format!("{channel_type}.{alias}");
    let conventional_key = format!("{channel_type}_{alias}");

    let target_key = instance_group_key(cfg, channel_type, alias);

    if let Some(key) = target_key {
        // Invariant: `target_key` was selected from existing map entries.
        if let Some(group) = cfg.peer_groups.get_mut(&key) {
            group
                .external_peers
                .push(PeerUsername::new(normalized.to_string()));
        }
        return Ok(Some(key));
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
        conventional_key.clone(),
        PeerGroupConfig {
            channel: ChannelRef::new(dotted_ref),
            external_peers: vec![PeerUsername::new(normalized.to_string())],
            ..PeerGroupConfig::default()
        },
    );
    Ok(Some(conventional_key))
}

/// Persist a paired identity as an authorized external peer.
///
/// The durable write comes first: the merge is staged on a clone, saved to
/// `config.toml`, and only published to the shared canonical config once the
/// save succeeds. Mutating the live config before the save would leave a grant
/// the file never received, which a retry then reads as "already authorized"
/// and reports as a successful binding that disappears on restart.
///
/// The publish re-runs the same merge against the live config rather than
/// installing the staged clone, so an unrelated edit made while the save was in
/// flight is preserved. The saved *file* is still last-writer-wins, which is
/// `Config::save`'s pre-existing contract and not something this writer can
/// narrow on its own.
///
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
    let staged = {
        let cfg = config.read();
        let mut staged = cfg.clone();
        if merge_external_peer(&mut staged, channel_type, alias, identity, &match_fn)?.is_none() {
            return Ok(());
        }
        staged
    };
    staged
        .save()
        .await
        .with_context(|| format!("Failed to persist {channel_type} peer to config.toml"))?;
    {
        let mut cfg = config.write();
        merge_external_peer(&mut cfg, channel_type, alias, identity, &match_fn)?;
    }
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
        assert!(changed.is_some());

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
            merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact)
                .unwrap()
                .is_some()
        );
        assert!(
            merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact)
                .unwrap()
                .is_none(),
            "an already-authorized identity is not re-added"
        );
        assert!(
            merge_external_peer(&mut config, "whatsapp", "admin", "+15559876543", exact)
                .unwrap()
                .is_some(),
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
            merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact)
                .unwrap()
                .is_some()
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

        assert!(
            merge_external_peer(&mut config, "telegram", "admin", "someone", exact)
                .unwrap()
                .is_some()
        );
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
            merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact)
                .expect("merge succeeds once the deny is gone")
                .is_none(),
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
            merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact)
                .unwrap()
                .is_some()
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
            merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact)
                .unwrap()
                .is_some()
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
            merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact)
                .unwrap()
                .is_some()
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
            merge_external_peer(&mut config, "whatsapp", "admin", "+15551234567", exact)
                .unwrap()
                .is_none(),
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

    #[cfg(any(feature = "channel-wechat", feature = "whatsapp-web"))]
    #[tokio::test]
    async fn persist_without_handle_warns_and_returns_ok() {
        persist_external_peer(None, "whatsapp", "admin", "+15551234567", exact)
            .await
            .expect("missing handle is a soft no-op");
    }

    /// A failed `Config::save()` must leave nothing behind: not on disk, and
    /// not in the live config either.
    ///
    /// Mutating the shared config before the save made the grant survive a
    /// failure the file never recorded. The retry then read its own leftover
    /// through the already-authorized check, returned `Ok` without attempting
    /// another write, and the channel answered "bound" for an authorization
    /// that vanished on restart.
    #[tokio::test]
    async fn failed_save_leaves_no_grant_and_the_retry_still_persists() {
        let mut config = config_with_whatsapp("admin");
        // A parent directory that does not exist, so the write cannot land.
        config.config_path = std::path::PathBuf::from("/nonexistent-zeroclaw-9428/config.toml");
        let shared = Arc::new(parking_lot::RwLock::new(config));

        let first =
            persist_external_peer(Some(&shared), "whatsapp", "admin", "+15551234567", exact).await;
        assert!(first.is_err(), "an unwritable config path must surface");

        assert!(
            shared
                .read()
                .channel_external_peers("whatsapp", "admin")
                .is_empty(),
            "a save that failed must not leave the grant live in memory"
        );

        // The retry must reach the writer again rather than short-circuit on a
        // leftover grant. Under the old ordering this returned Ok(()), which is
        // the channel reporting a successful binding that was never persisted.
        let second =
            persist_external_peer(Some(&shared), "whatsapp", "admin", "+15551234567", exact).await;
        assert!(
            second.is_err(),
            "the retry must attempt persistence again, not report success"
        );
        assert!(
            shared
                .read()
                .channel_external_peers("whatsapp", "admin")
                .is_empty(),
            "still nothing authorized after the second failure"
        );
    }

    /// An identity already authorized by a bare type-wide group must be
    /// reported against that group, not the conventional instance key.
    ///
    /// `instance_group_key` only matches exact dotted refs, so the bind
    /// endpoint fell back to `<type>_<alias>` and named a block that need not
    /// exist. An operator following that answer edits the wrong group.
    #[test]
    fn authorizing_group_key_names_a_type_wide_source() {
        use zeroclaw_config::multi_agent::{PeerGroupConfig, PeerUsername};
        use zeroclaw_config::providers::ChannelRef;

        let mut config = config_with_whatsapp("admin");
        config.peer_groups.insert(
            "whatsapp_all".to_string(),
            PeerGroupConfig {
                channel: ChannelRef::new("whatsapp".to_string()),
                external_peers: vec![PeerUsername::new("+15551234567".to_string())],
                ..Default::default()
            },
        );

        assert_eq!(
            authorizing_group_key(&config, "whatsapp", "admin", "+15551234567", exact).as_deref(),
            Some("whatsapp_all"),
            "the group carrying the grant is the one to name"
        );
        assert_eq!(
            instance_group_key(&config, "whatsapp", "admin"),
            None,
            "and the write-target resolver genuinely cannot answer this"
        );

        // An identity no group grants has no source to report.
        assert_eq!(
            authorizing_group_key(&config, "whatsapp", "admin", "+15559999999", exact),
            None
        );
    }

    /// The success path still publishes, so the fix cannot be "never write".
    #[tokio::test]
    async fn successful_save_publishes_the_grant_to_the_live_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = config_with_whatsapp("admin");
        config.config_path = dir.path().join("config.toml");
        let shared = Arc::new(parking_lot::RwLock::new(config));

        persist_external_peer(Some(&shared), "whatsapp", "admin", "+15551234567", exact)
            .await
            .expect("a writable path persists");

        assert_eq!(
            shared.read().channel_external_peers("whatsapp", "admin"),
            vec!["+15551234567".to_string()],
            "the live config carries the grant once the save succeeded"
        );
    }
}
