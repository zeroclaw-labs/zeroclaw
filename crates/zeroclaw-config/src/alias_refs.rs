//! Alias reference discovery for typed delete-with-cascade (#7175).
//!
//! [`find_all_references`] enumerates every config site that references an
//! aliased entry of a given [`AliasKind`] (provider / agent / channel), tagging
//! each as a **HARD** reference (a mandatory field — deletion must refuse) or a
//! **SOFT** reference (removable — deletion scrubs it). [`plan_delete`] folds
//! the sites into an [`ImpactReport`] a surface (TUI / web / CLI / RPC) renders
//! before confirming a destructive action.
//!
//! This is the **read-only** foundation: it never mutates [`Config`]. It mirrors,
//! referrer-for-referrer, the dangling-reference walk in `Config::validate()`
//! (`schema.rs` ~16245-17483) — the same containers in deterministic order — so
//! the two cannot drift in which references they recognise. Anchors to the
//! mirrored validation are cited per arm below. `delete_with_cascade` (mutating)
//! applies the soft-ref [`ScrubAction`]s and removes the entry; owned non-config
//! state (memory rows, workspace dir, infra DB rows) is cascaded by the calling
//! surface, which owns those stores.

use crate::schema::Config;

/// Which aliased-entry kind is being deleted. The kind plus the leaf `alias`
/// determines the *target value* a referrer must equal to count as a reference:
/// `providers.<category>.<family>.<alias>` → `"<family>.<alias>"`,
/// `channels.<channel_type>.<alias>` → `"<channel_type>.<alias>"`,
/// `agents.<alias>` → bare `"<alias>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AliasKind {
    /// A provider profile under `providers.<category>.<family>.<alias>`.
    Provider {
        category: ProviderCategory,
        family: String,
    },
    /// A channel instance under `channels.<channel_type>.<alias>`.
    Channel { channel_type: String },
    /// An agent under `agents.<alias>`.
    Agent,
}

/// Which typed provider section the alias lives in. Selects which referrer
/// fields can point at it (model refs vs TTS vs transcription).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderCategory {
    Models,
    Tts,
    Transcription,
}

/// HARD = mandatory referrer; deleting the target invalidates config, so the
/// delete must refuse. SOFT = removable; the delete scrubs the referrer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefStrength {
    Hard,
    Soft,
}

/// How a soft reference would be repaired on delete (applied in PR2+). Hard
/// references carry [`ScrubAction::Refuse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrubAction {
    /// Mandatory reference — block the delete.
    Refuse,
    /// Clear a scalar / `Option` field to empty / `None`.
    ClearOptional,
    /// Remove the element at `index` from a `Vec`. PR2 must apply
    /// `DropFromVec` actions per container in **descending index order** so
    /// earlier removals don't shift later indices.
    DropFromVec { index: usize },
    /// Remove the entry keyed by `key` from a map.
    RemoveMapKey { key: String },
}

/// One concrete config site that references the target alias. `path` is the
/// resolved dotted path (e.g. `agents.researcher.channels[2]`), built with the
/// same `format!` templates `Config::validate()` emits so dashboard inline-error
/// binding keeps working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefSite {
    pub path: String,
    pub strength: RefStrength,
    pub action: ScrubAction,
    /// The stored reference text, e.g. `"anthropic.default"`.
    pub raw_value: String,
}

impl RefSite {
    fn hard(path: String, action: ScrubAction, raw_value: &str) -> Self {
        Self {
            path,
            strength: RefStrength::Hard,
            action,
            raw_value: raw_value.to_string(),
        }
    }
    fn soft(path: String, action: ScrubAction, raw_value: &str) -> Self {
        Self {
            path,
            strength: RefStrength::Soft,
            action,
            raw_value: raw_value.to_string(),
        }
    }
}

/// Non-config persisted state attributed to a deleted agent (ACP sessions,
/// session metadata, memory rows, workspace dirs). Enumerated from infra
/// stores, **not** from [`Config`], so the pure config walk leaves
/// [`ImpactReport::owned_state`] empty; the calling surface (which owns the infra
/// stores) populates and cascades it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedArtifact {
    pub store: String,
    pub strength: RefStrength,
    pub action: ScrubAction,
    pub locator: String,
}

/// Dry-run plan for deleting an aliased entry: which references block the
/// delete, which would be scrubbed, and whether the delete is allowed.
#[derive(Debug, Clone)]
pub struct ImpactReport {
    pub target_kind: AliasKind,
    pub target_alias: String,
    /// Hard references — non-empty means the delete is refused.
    pub blockers: Vec<RefSite>,
    /// Soft references that would be scrubbed.
    pub scrubs: Vec<RefSite>,
    /// Owned non-config state — empty from the pure config walk; populated by
    /// the surface cascade, which owns the infra stores.
    pub owned_state: Vec<OwnedArtifact>,
    /// `true` iff no hard reference (or hard owned artifact) blocks the delete.
    pub allowed: bool,
}

/// Enumerate every config site that references `alias` of `kind`. Pure /
/// read-only; mirrors `Config::validate()` referrer-for-referrer.
#[must_use]
pub fn find_all_references(cfg: &Config, kind: &AliasKind, alias: &str) -> Vec<RefSite> {
    let mut sites = Vec::new();
    match kind {
        AliasKind::Provider { category, family } => {
            collect_provider_refs(cfg, *category, family, alias, &mut sites);
        }
        AliasKind::Channel { channel_type } => {
            collect_channel_refs(cfg, channel_type, alias, &mut sites);
        }
        AliasKind::Agent => collect_agent_refs(cfg, alias, &mut sites),
    }
    sites
}

/// Build the dry-run [`ImpactReport`] for deleting `alias` of `kind`. Pure /
/// read-only; owned-state is gathered separately by the surface cascade.
#[must_use]
pub fn plan_delete(cfg: &Config, kind: &AliasKind, alias: &str) -> ImpactReport {
    let (blockers, scrubs): (Vec<_>, Vec<_>) = find_all_references(cfg, kind, alias)
        .into_iter()
        .partition(|s| s.strength == RefStrength::Hard);
    let allowed = blockers.is_empty();
    ImpactReport {
        target_kind: kind.clone(),
        target_alias: alias.to_string(),
        blockers,
        scrubs,
        owned_state: Vec::new(),
        allowed,
    }
}

// ── delete-with-cascade (mutating) ──────────────────────────────────────────

/// How a delete handles references and whether it mutates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CascadePolicy {
    /// Refuse if any HARD reference blocks; otherwise scrub the soft references
    /// and remove the entry. The #7175-accepted default.
    RefuseOnHard,
    /// Compute the plan and mutate nothing (the dry-run a surface renders).
    DryRun,
}

/// Outcome of a (non-refused) [`delete_with_cascade`].
#[derive(Debug, Clone)]
pub struct CascadeReport {
    /// The impact plan that was computed (same shape as [`plan_delete`]).
    pub plan: ImpactReport,
    /// Soft references actually scrubbed. Empty for [`CascadePolicy::DryRun`].
    pub applied: Vec<RefSite>,
    /// Dotted path of the removed entry, e.g. `providers.models.anthropic.default`.
    /// `None` for a dry run.
    pub deleted_entry: Option<String>,
}

/// Why a [`delete_with_cascade`] did not complete. `Refused` is an expected,
/// renderable outcome (a hard reference blocks the delete), not a bug.
#[derive(Debug)]
pub enum CascadeError {
    /// A hard reference blocks the delete; no mutation was performed. The report
    /// lists the blockers for the surface to render. Boxed so the common `Ok`
    /// path (and the other variants) don't carry `ImpactReport`'s several `Vec`s
    /// inline (`clippy::result_large_err`).
    Refused(Box<ImpactReport>),
    /// The target alias does not exist.
    NotFound(String),
    /// This alias kind is not yet wired into `delete_with_cascade`.
    NotImplemented(String),
    /// Bug guard: scrub drifted from `find_all_references` and left a dangling
    /// reference to the deleted alias. **The config WAS mutated** (scrub + entry
    /// removal ran) — the caller must NOT persist it. Unreachable while the two
    /// mirror exactly (same soft-ref sites, same `.trim()`); fires only on
    /// maintenance drift. The message names the offending paths.
    PostCondition(String),
}

impl std::fmt::Display for CascadeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(report) => write!(
                f,
                "delete refused: {} hard reference(s) block it",
                report.blockers.len()
            ),
            Self::NotFound(path) => write!(f, "alias not found: {path}"),
            Self::NotImplemented(msg) => write!(f, "{msg}"),
            Self::PostCondition(msg) => write!(f, "cascade post-condition failed: {msg}"),
        }
    }
}

impl std::error::Error for CascadeError {}

/// Delete an aliased entry and repair every reference to it, per `policy`.
///
/// `RefuseOnHard` refuses when any HARD reference would dangle (returns
/// [`CascadeError::Refused`] with the full report, no mutation), otherwise
/// scrubs the SOFT references, removes the entry, and verifies no dangling
/// reference to the alias remains. `DryRun` computes the plan and mutates
/// nothing. [`plan_delete`] is the read-only sibling.
///
/// Implements the **model-provider** kind (`providers.models.<family>.<alias>`)
/// and the **agent** kind (`agents.<alias>`). The agent arm cascades config
/// references only; its owned non-config state (memory rows, workspace dir,
/// cron/acp/session rows) is cascaded by the calling surface and is not yet
/// reflected in `ImpactReport.owned_state`. TTS/transcription providers and
/// channels return [`CascadeError::NotImplemented`] until their follow-up lands
/// (#7175).
pub fn delete_with_cascade(
    cfg: &mut Config,
    kind: &AliasKind,
    alias: &str,
    policy: CascadePolicy,
) -> Result<CascadeReport, CascadeError> {
    match kind {
        AliasKind::Provider {
            category: ProviderCategory::Models,
            family,
        } => delete_model_provider(cfg, family, alias, policy),
        AliasKind::Provider { .. } => Err(CascadeError::NotImplemented(
            "TTS/transcription provider delete-with-cascade is not yet implemented".to_string(),
        )),
        AliasKind::Agent => delete_agent(cfg, alias, policy),
        AliasKind::Channel { .. } => Err(CascadeError::NotImplemented(
            "channel delete-with-cascade lands in a follow-up (#7175)".to_string(),
        )),
    }
}

fn delete_model_provider(
    cfg: &mut Config,
    family: &str,
    alias: &str,
    policy: CascadePolicy,
) -> Result<CascadeReport, CascadeError> {
    let entry_path = format!("providers.models.{family}.{alias}");
    if cfg.providers.models.find(family, alias).is_none() {
        return Err(CascadeError::NotFound(entry_path));
    }

    let kind = AliasKind::Provider {
        category: ProviderCategory::Models,
        family: family.to_string(),
    };
    let report = plan_delete(cfg, &kind, alias);

    if policy == CascadePolicy::DryRun {
        return Ok(CascadeReport {
            plan: report,
            applied: Vec::new(),
            deleted_entry: None,
        });
    }
    if !report.allowed {
        return Err(CascadeError::Refused(Box::new(report)));
    }

    let applied = report.scrubs.clone();
    let target = format!("{family}.{alias}");
    scrub_model_provider_refs(cfg, &target);
    let removed = cfg.providers.models.remove_alias(family, alias);
    debug_assert!(removed, "existence was checked above");

    // Targeted post-condition: the cascade must leave no reference to the
    // deleted alias. (We intentionally do NOT re-run the global
    // `Config::validate()` here — that conflates pre-existing, unrelated
    // invalidity with this cascade's correctness; the calling surface
    // validates the whole config before persisting.)
    let remaining = find_all_references(cfg, &kind, alias);
    if !remaining.is_empty() {
        let paths: Vec<_> = remaining.iter().map(|s| s.path.as_str()).collect();
        return Err(CascadeError::PostCondition(format!(
            "{} dangling reference(s) to {target} remain: {}",
            remaining.len(),
            paths.join(", ")
        )));
    }

    Ok(CascadeReport {
        plan: report,
        applied,
        deleted_entry: Some(entry_path),
    })
}

/// Mutating mirror of the model-provider arm of [`find_all_references`]: clear
/// soft scalar refs and drop soft collection elements pointing at `target`
/// (`"<family>.<alias>"`). `model_provider` is a HARD ref and is never scrubbed
/// (a delete carrying one is refused before reaching here). `retain` handles
/// the index-shift concern for the vector drops. Comparisons `.trim()` the
/// stored value to mirror `find_all_references` (and `validate()`) exactly — a
/// whitespace-padded ref that find() flagged must be scrubbed here too, or the
/// post-condition would fail.
fn scrub_model_provider_refs(cfg: &mut Config, target: &str) {
    for agent in cfg.agents.values_mut() {
        if agent.classifier_provider.trim() == target {
            agent.classifier_provider = crate::providers::ModelProviderRef::default();
        }
    }
    for (_ty, _al, profile) in cfg.providers.models.iter_entries_mut() {
        profile.fallback.retain(|fb| fb.trim() != target);
    }
    cfg.model_routes
        .retain(|r| r.model_provider.trim() != target);
    cfg.embedding_routes
        .retain(|r| r.model_provider.trim() != target);
}

fn delete_agent(
    cfg: &mut Config,
    alias: &str,
    policy: CascadePolicy,
) -> Result<CascadeReport, CascadeError> {
    let entry_path = format!("agents.{alias}");
    if !cfg.agents.contains_key(alias) {
        return Err(CascadeError::NotFound(entry_path));
    }

    let kind = AliasKind::Agent;
    let report = plan_delete(cfg, &kind, alias);

    if policy == CascadePolicy::DryRun {
        return Ok(CascadeReport {
            plan: report,
            applied: Vec::new(),
            deleted_entry: None,
        });
    }
    // Config-scoped gate: refuse if `plan_delete` found any HARD ref. The hard
    // agent refs are whatever `collect_agent_refs` marks `RefStrength::Hard` —
    // currently an enabled `heartbeat.agent` and a channel the agent solely owns
    // (deleting its sole enabled owner would orphan the route). Owned-state HARD
    // refs (e.g. live ACP sessions) are enforced by the surface layer that owns
    // the infra stores; the pure config walk does not see them.
    if !report.allowed {
        return Err(CascadeError::Refused(Box::new(report)));
    }

    let applied = report.scrubs.clone();
    scrub_agent_refs(cfg, alias);
    cfg.agents.remove(alias);

    let remaining = find_all_references(cfg, &kind, alias);
    if !remaining.is_empty() {
        let paths: Vec<_> = remaining.iter().map(|s| s.path.as_str()).collect();
        return Err(CascadeError::PostCondition(format!(
            "{} dangling reference(s) to agent {alias} remain: {}",
            remaining.len(),
            paths.join(", ")
        )));
    }

    Ok(CascadeReport {
        plan: report,
        applied,
        deleted_entry: Some(entry_path),
    })
}

/// Mutating mirror of [`collect_agent_refs`]: clear soft scalar refs and drop
/// soft collection elements naming `alias`. Trims the same sites
/// `collect_agent_refs` trims (heartbeat, acp.default_agent, delegates) and
/// leaves the three `AgentAlias`-keyed sites raw (workspace.access,
/// read_memory_from, peer_groups.agents) — both mirror `validate()` exactly.
/// `heartbeat.agent` is cleared only when reached (an *enabled* heartbeat
/// pointing at `alias` is a HARD ref, refused before this runs). `retain` is
/// index-shift-safe. The loop over `cfg.agents.values_mut()` still includes the
/// to-be-deleted agent, so a self-reference (e.g. `bot.delegates = ["bot"]`) is
/// actively stripped by the `retain` here before the entry itself is removed.
fn scrub_agent_refs(cfg: &mut Config, alias: &str) {
    if cfg.heartbeat.agent.trim() == alias {
        cfg.heartbeat.agent.clear();
    }
    // Compute the match first so the immutable borrow ends before the assignment.
    let clear_acp = cfg
        .acp
        .default_agent
        .as_deref()
        .is_some_and(|da| da.trim() == alias);
    if clear_acp {
        cfg.acp.default_agent = None;
    }
    for agent in cfg.agents.values_mut() {
        agent.delegates.retain(|d| d.trim() != alias); // trimmed (validate trims)
        agent.workspace.access.retain(|k, _| k.as_str() != alias); // raw
        agent
            .workspace
            .read_memory_from
            .retain(|m| m.as_str() != alias); // raw
    }
    for group in cfg.peer_groups.values_mut() {
        group.agents.retain(|m| m.as_str() != alias); // raw
    }
}

// ── deterministic iteration over the alias-keyed maps ───────────────────────
// `Config::agents` / `peer_groups` are HashMaps; sort by key so RefSite order
// is stable across runs (tests + dashboard binding depend on it).

fn sorted_agents(cfg: &Config) -> Vec<(&String, &crate::schema::AliasedAgentConfig)> {
    let mut v: Vec<_> = cfg.agents.iter().collect();
    v.sort_by(|a, b| a.0.cmp(b.0));
    v
}

fn sorted_peer_groups(cfg: &Config) -> Vec<(&String, &crate::multi_agent::PeerGroupConfig)> {
    let mut v: Vec<_> = cfg.peer_groups.iter().collect();
    v.sort_by(|a, b| a.0.cmp(b.0));
    v
}

fn collect_provider_refs(
    cfg: &Config,
    category: ProviderCategory,
    family: &str,
    alias: &str,
    sites: &mut Vec<RefSite>,
) {
    let target = format!("{family}.{alias}");
    // `Config::validate()` TRIMS every provider ref before resolving it
    // (model_provider schema.rs:17143, classifier :17227, tts :17217,
    // transcription :17221, model/embedding routes :16549/:16595, the fallback
    // walk :16177). A whitespace-padded TOML value therefore passes validation,
    // so we must trim the stored value before matching here too or we silently
    // miss it. `raw_value` keeps the actual stored text (incl. any whitespace).
    match category {
        ProviderCategory::Models => {
            for (name, agent) in sorted_agents(cfg) {
                if agent.model_provider.trim() == target {
                    sites.push(RefSite::hard(
                        format!("agents.{name}.model_provider"),
                        ScrubAction::Refuse,
                        agent.model_provider.as_str(),
                    ));
                }
                if agent.classifier_provider.trim() == target {
                    sites.push(RefSite::soft(
                        format!("agents.{name}.classifier_provider"),
                        ScrubAction::ClearOptional,
                        agent.classifier_provider.as_str(),
                    ));
                }
            }
            for (ty, al, profile) in cfg.providers.models.iter_entries() {
                for (i, fb) in profile.fallback.iter().enumerate() {
                    if fb.trim() == target {
                        sites.push(RefSite::soft(
                            format!("providers.models.{ty}.{al}.fallback[{i}]"),
                            ScrubAction::DropFromVec { index: i },
                            fb.as_str(),
                        ));
                    }
                }
            }
            for (i, route) in cfg.model_routes.iter().enumerate() {
                if route.model_provider.trim() == target {
                    sites.push(RefSite::soft(
                        format!("model_routes[{i}].model_provider"),
                        ScrubAction::DropFromVec { index: i },
                        route.model_provider.as_str(),
                    ));
                }
            }
            for (i, route) in cfg.embedding_routes.iter().enumerate() {
                if route.model_provider.trim() == target {
                    sites.push(RefSite::soft(
                        format!("embedding_routes[{i}].model_provider"),
                        ScrubAction::DropFromVec { index: i },
                        route.model_provider.as_str(),
                    ));
                }
            }
        }
        // TTS / transcription preferences are optional scalars (empty = opt-out),
        // so deletion clears them. Mirrors the typed-provider-ref loop at
        // schema.rs:17216-17253.
        ProviderCategory::Tts => {
            for (name, agent) in sorted_agents(cfg) {
                if agent.tts_provider.trim() == target {
                    sites.push(RefSite::soft(
                        format!("agents.{name}.tts_provider"),
                        ScrubAction::ClearOptional,
                        agent.tts_provider.as_str(),
                    ));
                }
            }
        }
        ProviderCategory::Transcription => {
            for (name, agent) in sorted_agents(cfg) {
                if agent.transcription_provider.trim() == target {
                    sites.push(RefSite::soft(
                        format!("agents.{name}.transcription_provider"),
                        ScrubAction::ClearOptional,
                        agent.transcription_provider.as_str(),
                    ));
                }
            }
        }
    }
}

fn collect_channel_refs(cfg: &Config, channel_type: &str, alias: &str, sites: &mut Vec<RefSite>) {
    let target = format!("{channel_type}.{alias}");
    // validate() trims channel refs before resolving (agent channels
    // schema.rs:17183, peer-group channel :17418); trim the stored value before
    // matching, mirror the dotted-vs-bare rule, and keep the raw text.
    // agents.<X>.channels[] — empty list is valid (delegate-only agents).
    for (name, agent) in sorted_agents(cfg) {
        for (i, ch) in agent.channels.iter().enumerate() {
            if ch.trim() == target {
                sites.push(RefSite::soft(
                    format!("agents.{name}.channels[{i}]"),
                    ScrubAction::DropFromVec { index: i },
                    ch.as_str(),
                ));
            }
        }
    }
    // peer_groups.<g>.channel — mandatory ChannelRef; deletion refused.
    // A bare-type group channel (`"discord"`) does not equal the dotted target,
    // so single-alias deletes don't match it.
    for (gname, group) in sorted_peer_groups(cfg) {
        if group.channel.trim() == target {
            sites.push(RefSite::hard(
                format!("peer_groups.{gname}.channel"),
                ScrubAction::Refuse,
                group.channel.as_str(),
            ));
        }
    }
    // Last-alias-of-type guard for BARE-type group channels. A bare channel
    // (`"discord"`) doesn't match the dotted target above, so it's skipped while
    // any `channels.<type>.*` alias survives — but deleting the *last* alias of
    // the type empties the block, and validate() then bails the bare-type group
    // (`peer_groups.<g>.channel = "<type>"` resolves to no configured
    // `[channels.<type>.*]`, schema.rs:17432-17439). Report those bare groups as
    // HARD so the plan refuses instead of letting the mutating delete remove the
    // type's final alias out from under them.
    // True only when `alias` is the sole existing alias of the type, so deleting
    // it empties the block. (If the type is unconfigured or `alias` isn't its
    // only key, this delete doesn't cause the dangle.)
    let removes_last_alias = cfg
        .get_map_keys(&format!("channels.{channel_type}"))
        .is_some_and(|keys| keys.iter().any(|k| k == alias) && keys.iter().all(|k| k == alias));
    if removes_last_alias {
        for (gname, group) in sorted_peer_groups(cfg) {
            if group.channel.trim() == channel_type {
                sites.push(RefSite::hard(
                    format!("peer_groups.{gname}.channel"),
                    ScrubAction::Refuse,
                    group.channel.as_str(),
                ));
            }
        }
    }
    // escalation.alert_channels[] — runtime WARN-skips unknown names (not
    // load-validated, schema.rs:6841); trim defensively (the runtime tolerates
    // padding) and drop the element.
    for (i, ch) in cfg.escalation.alert_channels.iter().enumerate() {
        if ch.trim() == target {
            sites.push(RefSite::soft(
                format!("escalation.alert_channels[{i}]"),
                ScrubAction::DropFromVec { index: i },
                ch.as_str(),
            ));
        }
    }
    // peer_groups.<g>.agents[i] — a member of a BARE-type group (`channel =
    // "discord"`) must keep at least one `<type>.*` channel (validate()
    // schema.rs:17461-17478, the `None`/bare arm). A bare group channel is not a
    // dotted ref, so it is not a HARD ref above — but scrubbing a member's *only*
    // `<type>.*` channel (the SOFT `agents.<m>.channels` ref collected above)
    // would leave that member without a required channel, producing a config
    // `validate()` rejects. Treat that as HARD: refuse rather than report success
    // on a delete that yields an invalid config. validate()'s member check uses
    // the *untrimmed* channel string, so the survivor test mirrors that exactly.
    // (This member-level guard is the companion to the type-level last-alias
    // guard above; it fires even while another `<type>.*` alias keeps the block
    // present, because the member's *own* only matching channel is the target.)
    let type_prefix = format!("{channel_type}.");
    for (gname, group) in sorted_peer_groups(cfg) {
        // Bare type only; type must match the channel being deleted. Dotted
        // groups are already covered by the direct peer-group channel ref above.
        if group.channel.trim() != channel_type {
            continue;
        }
        for (i, member) in group.agents.iter().enumerate() {
            let Some(m) = cfg.agents.get(member.as_str()) else {
                // a dangling member is validate()'s own DanglingReference; skip.
                continue;
            };
            // Does this member reference the channel being deleted (the ref that
            // would be scrubbed, which trims)?
            if !m.channels.iter().any(|ch| ch.trim() == target) {
                continue;
            }
            // Would any `<type>.*` channel survive the scrub? validate()'s bare
            // membership test does not trim, so neither does this survivor test.
            let survives = m
                .channels
                .iter()
                .any(|ch| ch.trim() != target && ch.as_str().starts_with(&type_prefix));
            if !survives {
                sites.push(RefSite::hard(
                    format!("peer_groups.{gname}.agents[{i}]"),
                    ScrubAction::Refuse,
                    member.as_str(),
                ));
            }
        }
    }
}

fn collect_agent_refs(cfg: &Config, alias: &str, sites: &mut Vec<RefSite>) {
    // TRIM-MATCHED agent refs: validate() trims these before resolving
    // (heartbeat schema.rs:16338, delegates :17331); acp.default_agent is not
    // load-validated but the ACP runtime resolves it by alias, so trim it too to
    // avoid leaving a whitespace-padded dangling pointer. raw_value keeps the
    // actual stored text.
    //
    // heartbeat.agent — hard only when heartbeat is enabled (validate() bails on
    // a dangling target only then); when disabled the pointer is tolerated, so
    // deletion clears it rather than refusing.
    if cfg.heartbeat.agent.trim() == alias {
        let raw = cfg.heartbeat.agent.as_str();
        if cfg.heartbeat.enabled {
            sites.push(RefSite::hard(
                "heartbeat.agent".to_string(),
                ScrubAction::Refuse,
                raw,
            ));
        } else {
            sites.push(RefSite::soft(
                "heartbeat.agent".to_string(),
                ScrubAction::ClearOptional,
                raw,
            ));
        }
    }
    // acp.default_agent — Option<String>, not load-validated (schema.rs:10889).
    if let Some(da) = cfg.acp.default_agent.as_deref()
        && da.trim() == alias
    {
        sites.push(RefSite::soft(
            "acp.default_agent".to_string(),
            ScrubAction::ClearOptional,
            da,
        ));
    }
    for (name, agent) in sorted_agents(cfg) {
        // delegates[] — validate() trims (schema.rs:17331).
        for (i, d) in agent.delegates.iter().enumerate() {
            if d.trim() == alias {
                sites.push(RefSite::soft(
                    format!("agents.{name}.delegates[{i}]"),
                    ScrubAction::DropFromVec { index: i },
                    d.as_str(),
                ));
            }
        }
        // RAW-MATCHED AgentAlias refs below: validate() compares these via
        // `as_str()` WITHOUT trimming (workspace.access schema.rs:17358,
        // read_memory_from :17382, peer_groups.agents :17453), so we must NOT
        // trim here either — trimming would itself drift from validate().
        //
        // workspace.access map key.
        if agent.workspace.access.keys().any(|k| k.as_str() == alias) {
            sites.push(RefSite::soft(
                format!("agents.{name}.workspace.access.{alias}"),
                ScrubAction::RemoveMapKey {
                    key: alias.to_string(),
                },
                alias,
            ));
        }
        // workspace.read_memory_from[].
        for (i, m) in agent.workspace.read_memory_from.iter().enumerate() {
            if m.as_str() == alias {
                sites.push(RefSite::soft(
                    format!("agents.{name}.workspace.read_memory_from[{i}]"),
                    ScrubAction::DropFromVec { index: i },
                    alias,
                ));
            }
        }
    }
    // peer_groups.<g>.agents[] — raw match (validate() :17453 does not trim).
    for (gname, group) in sorted_peer_groups(cfg) {
        for (i, m) in group.agents.iter().enumerate() {
            if m.as_str() == alias {
                sites.push(RefSite::soft(
                    format!("peer_groups.{gname}.agents[{i}]"),
                    ScrubAction::DropFromVec { index: i },
                    alias,
                ));
            }
        }
    }
    // Channel OWNERSHIP (the agent's own `channels`). `Config::agent_for_channel`
    // resolves a channel's owner to the (first) ENABLED agent whose `channels`
    // list contains it; deleting that agent leaves the channel with no owner —
    // the route is silently orphaned. #7175 treats channel ownership as a HARD
    // agent-delete concern, so report each channel the target *solely* owns as a
    // blocker (refuse), absent a repoint/prune policy. Ownership uses
    // `agent_for_channel`'s exact (untrimmed, enabled-only) match. A disabled
    // target owns nothing; a channel another enabled agent also lists is not
    // orphaned, so it isn't reported.
    if let Some(target) = cfg.agents.get(alias)
        && target.enabled
    {
        for (i, ch) in target.channels.iter().enumerate() {
            let owned_elsewhere = cfg.agents.iter().any(|(name, other)| {
                name.as_str() != alias
                    && other.enabled
                    && other.channels.iter().any(|c| c.as_str() == ch.as_str())
            });
            if !owned_elsewhere {
                sites.push(RefSite::hard(
                    format!("agents.{alias}.channels[{i}]"),
                    ScrubAction::Refuse,
                    ch.as_str(),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multi_agent::{AccessMode, AgentAlias, PeerGroupConfig};
    use crate::schema::{AliasedAgentConfig, Config, EmbeddingRouteConfig, ModelRouteConfig};

    /// Empty config with the alias-keyed containers cleared so Config::default()
    /// can't inject spurious references into assertions.
    fn empty_config() -> Config {
        let mut c = Config::default();
        c.agents.clear();
        c.peer_groups.clear();
        c.model_routes.clear();
        c.embedding_routes.clear();
        c.escalation.alert_channels.clear();
        c.heartbeat.enabled = false;
        c.heartbeat.agent.clear();
        c.acp.default_agent = None;
        c
    }

    fn provider_kind(family: &str) -> AliasKind {
        AliasKind::Provider {
            category: ProviderCategory::Models,
            family: family.to_string(),
        }
    }

    #[test]
    fn provider_models_hard_and_soft() {
        let mut cfg = empty_config();
        cfg.agents.insert(
            "researcher".to_string(),
            AliasedAgentConfig {
                model_provider: "anthropic.default".into(),
                ..Default::default()
            },
        );
        cfg.agents.insert(
            "triage".to_string(),
            AliasedAgentConfig {
                model_provider: "openai.fast".into(), // unrelated, must not match
                classifier_provider: "anthropic.default".into(),
                ..Default::default()
            },
        );
        cfg.model_routes.push(ModelRouteConfig {
            hint: "deep".to_string(),
            model_provider: "anthropic.default".to_string(),
            model: "claude".to_string(),
            api_key: None,
        });

        let kind = provider_kind("anthropic");
        let sites = find_all_references(&cfg, &kind, "default");
        assert_eq!(sites.len(), 3, "model_provider + classifier + route");

        let hard: Vec<_> = sites
            .iter()
            .filter(|s| s.strength == RefStrength::Hard)
            .collect();
        assert_eq!(hard.len(), 1);
        assert_eq!(hard[0].path, "agents.researcher.model_provider");
        assert_eq!(hard[0].action, ScrubAction::Refuse);

        let report = plan_delete(&cfg, &kind, "default");
        assert!(
            !report.allowed,
            "a hard model_provider ref must block the delete"
        );
        assert_eq!(report.blockers.len(), 1);
        assert_eq!(report.scrubs.len(), 2);
    }

    #[test]
    fn provider_tts_is_soft_clear() {
        let mut cfg = empty_config();
        cfg.agents.insert(
            "voice".to_string(),
            AliasedAgentConfig {
                tts_provider: "elevenlabs.default".into(),
                ..Default::default()
            },
        );
        let kind = AliasKind::Provider {
            category: ProviderCategory::Tts,
            family: "elevenlabs".to_string(),
        };
        let report = plan_delete(&cfg, &kind, "default");
        assert!(report.allowed);
        assert_eq!(report.scrubs.len(), 1);
        assert_eq!(report.scrubs[0].path, "agents.voice.tts_provider");
        assert_eq!(report.scrubs[0].action, ScrubAction::ClearOptional);
    }

    #[test]
    fn channel_hard_and_soft() {
        let mut cfg = empty_config();
        cfg.agents.insert(
            "ops".to_string(),
            AliasedAgentConfig {
                channels: vec!["discord.main".into()],
                ..Default::default()
            },
        );
        let group = PeerGroupConfig {
            channel: "discord.main".into(),
            ..Default::default()
        };
        cfg.peer_groups.insert("crew".to_string(), group);
        cfg.escalation
            .alert_channels
            .push("discord.main".to_string());

        let kind = AliasKind::Channel {
            channel_type: "discord".to_string(),
        };
        let report = plan_delete(&cfg, &kind, "main");
        assert_eq!(
            report.blockers.len(),
            1,
            "peer_groups channel is a hard ref"
        );
        assert_eq!(report.blockers[0].path, "peer_groups.crew.channel");
        assert_eq!(report.scrubs.len(), 2, "agent channel + alert_channel");
        assert!(!report.allowed);
    }

    #[test]
    fn channel_bare_type_group_is_not_matched_by_alias_delete() {
        let mut cfg = empty_config();
        // bare type, not a specific alias
        let group = PeerGroupConfig {
            channel: "discord".into(),
            ..Default::default()
        };
        cfg.peer_groups.insert("crew".to_string(), group);
        let kind = AliasKind::Channel {
            channel_type: "discord".to_string(),
        };
        assert!(find_all_references(&cfg, &kind, "main").is_empty());
    }

    #[test]
    fn agent_refs_heartbeat_hard_when_enabled() {
        let mut cfg = empty_config();
        cfg.heartbeat.enabled = true;
        cfg.heartbeat.agent = "bot".to_string();
        cfg.acp.default_agent = Some("bot".to_string());
        let mut referrer = AliasedAgentConfig {
            delegates: vec!["bot".to_string()],
            ..Default::default()
        };
        // workspace allowlists
        referrer
            .workspace
            .access
            .insert(AgentAlias::new("bot"), AccessMode::Read);
        referrer
            .workspace
            .read_memory_from
            .push(AgentAlias::new("bot"));
        cfg.agents.insert("lead".to_string(), referrer);
        let mut group = PeerGroupConfig::default();
        group.agents.push(AgentAlias::new("bot"));
        cfg.peer_groups.insert("crew".to_string(), group);

        let report = plan_delete(&cfg, &AliasKind::Agent, "bot");
        // heartbeat (hard) + delegates + access + read_memory_from + peer member + acp
        assert_eq!(report.blockers.len(), 1);
        assert_eq!(report.blockers[0].path, "heartbeat.agent");
        assert_eq!(report.scrubs.len(), 5);
        assert!(!report.allowed);
    }

    #[test]
    fn agent_heartbeat_soft_when_disabled() {
        let mut cfg = empty_config();
        cfg.heartbeat.enabled = false;
        cfg.heartbeat.agent = "bot".to_string();
        let report = plan_delete(&cfg, &AliasKind::Agent, "bot");
        assert!(report.allowed, "disabled heartbeat pointer is soft");
        assert_eq!(report.scrubs.len(), 1);
        assert_eq!(report.scrubs[0].action, ScrubAction::ClearOptional);
    }

    #[test]
    fn no_references_is_allowed_and_empty() {
        let cfg = empty_config();
        let report = plan_delete(&cfg, &provider_kind("anthropic"), "default");
        assert!(report.allowed);
        assert!(report.blockers.is_empty() && report.scrubs.is_empty());
    }

    #[test]
    fn ref_sites_are_sorted_by_owner() {
        let mut cfg = empty_config();
        for name in ["zeta", "alpha", "mid"] {
            cfg.agents.insert(
                name.to_string(),
                AliasedAgentConfig {
                    classifier_provider: "anthropic.default".into(),
                    ..Default::default()
                },
            );
        }
        let sites = find_all_references(&cfg, &provider_kind("anthropic"), "default");
        let paths: Vec<_> = sites.iter().map(|s| s.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "agents.alpha.classifier_provider",
                "agents.mid.classifier_provider",
                "agents.zeta.classifier_provider",
            ]
        );
    }

    #[test]
    fn whitespace_padded_provider_refs_are_found() {
        // validate() trims provider refs, so padded TOML values pass validation;
        // find_all_references must trim too or it silently misses them.
        let mut cfg = empty_config();
        cfg.agents.insert(
            "researcher".to_string(),
            AliasedAgentConfig {
                model_provider: "  anthropic.default  ".into(),
                classifier_provider: " anthropic.default ".into(),
                ..Default::default()
            },
        );
        cfg.model_routes.push(ModelRouteConfig {
            hint: "deep".to_string(),
            model_provider: " anthropic.default ".to_string(),
            model: "claude".to_string(),
            api_key: None,
        });
        let kind = provider_kind("anthropic");
        let sites = find_all_references(&cfg, &kind, "default");
        assert_eq!(
            sites.len(),
            3,
            "padded model_provider + classifier + route still found"
        );
        // raw_value preserves the actual stored (padded) text.
        let mp = sites
            .iter()
            .find(|s| s.path == "agents.researcher.model_provider")
            .unwrap();
        assert_eq!(mp.raw_value, "  anthropic.default  ");
        assert!(
            !plan_delete(&cfg, &kind, "default").allowed,
            "padded hard ref still blocks"
        );
    }

    #[test]
    fn agent_ref_trimming_mirrors_validate() {
        let mut cfg = empty_config();
        // TRIM-matched refs (validate trims): padded values must be FOUND.
        cfg.heartbeat.enabled = false;
        cfg.heartbeat.agent = "  bot  ".to_string();
        cfg.acp.default_agent = Some(" bot ".to_string());
        cfg.agents.insert(
            "lead".to_string(),
            AliasedAgentConfig {
                delegates: vec![" bot ".to_string()],
                ..Default::default()
            },
        );
        // RAW-matched ref (validate does NOT trim read_memory_from): a padded
        // value must NOT match, mirroring validate exactly.
        cfg.agents
            .get_mut("lead")
            .unwrap()
            .workspace
            .read_memory_from
            .push(AgentAlias::new(" bot "));

        let sites = find_all_references(&cfg, &AliasKind::Agent, "bot");
        let paths: Vec<_> = sites.iter().map(|s| s.path.as_str()).collect();
        assert!(paths.contains(&"heartbeat.agent"));
        assert!(paths.contains(&"acp.default_agent"));
        assert!(paths.contains(&"agents.lead.delegates[0]"));
        assert!(
            !paths.iter().any(|p| p.contains("read_memory_from")),
            "padded read_memory_from is raw-matched, must NOT match (mirror validate)"
        );
        let hb = sites.iter().find(|s| s.path == "heartbeat.agent").unwrap();
        assert_eq!(hb.raw_value, "  bot  ");
    }

    #[test]
    fn provider_fallback_and_embedding_route_refs_found() {
        let mut cfg = empty_config();
        // Another provider whose fallback names the target.
        cfg.providers
            .models
            .ensure("openai", "main")
            .unwrap()
            .fallback = vec!["anthropic.default".into()];
        cfg.embedding_routes.push(EmbeddingRouteConfig {
            hint: "sem".to_string(),
            model_provider: "anthropic.default".to_string(),
            model: "emb".to_string(),
            dimensions: None,
            api_key: None,
        });
        let sites = find_all_references(&cfg, &provider_kind("anthropic"), "default");
        let paths: Vec<_> = sites.iter().map(|s| s.path.as_str()).collect();
        assert!(paths.contains(&"providers.models.openai.main.fallback[0]"));
        assert!(paths.iter().any(|p| p.starts_with("embedding_routes[")));
        assert_eq!(sites.len(), 2);
    }

    #[test]
    fn provider_transcription_is_soft_clear() {
        let mut cfg = empty_config();
        cfg.agents.insert(
            "scribe".to_string(),
            AliasedAgentConfig {
                transcription_provider: "deepgram.default".into(),
                ..Default::default()
            },
        );
        let kind = AliasKind::Provider {
            category: ProviderCategory::Transcription,
            family: "deepgram".to_string(),
        };
        let report = plan_delete(&cfg, &kind, "default");
        assert!(report.allowed);
        assert_eq!(report.scrubs.len(), 1);
        assert_eq!(
            report.scrubs[0].path,
            "agents.scribe.transcription_provider"
        );
        assert_eq!(report.scrubs[0].action, ScrubAction::ClearOptional);
    }

    // ── review #7785: two delete-impact gaps ────────────────────────────────

    #[test]
    fn channel_delete_of_last_alias_blocks_bare_type_peer_group() {
        let mut cfg = empty_config();
        cfg.create_map_key("channels.discord", "main").unwrap(); // the ONLY discord alias
        cfg.peer_groups.insert(
            "crew".to_string(),
            PeerGroupConfig {
                channel: "discord".into(), // bare type — would dangle if discord empties
                ..Default::default()
            },
        );
        let kind = AliasKind::Channel {
            channel_type: "discord".to_string(),
        };
        // Deleting the last alias is HARD-blocked by the bare group's channel.
        let report = plan_delete(&cfg, &kind, "main");
        assert!(!report.allowed, "last-alias delete must be refused");
        assert!(
            report
                .blockers
                .iter()
                .any(|b| b.path == "peer_groups.crew.channel"),
            "{:?}",
            report.blockers
        );

        // With a SECOND alias present, deleting one is fine (the bare group still
        // has a `discord.*` to resolve against).
        cfg.create_map_key("channels.discord", "backup").unwrap();
        assert!(plan_delete(&cfg, &kind, "main").allowed);
    }

    #[test]
    fn channel_delete_blocks_when_bare_group_member_loses_only_channel() {
        // Audacity88's case: the TYPE survives (backup remains) but a bare-group
        // MEMBER's only `<type>.*` channel is the target. Soft-scrubbing it would
        // leave the member with no discord channel → validate() fails at
        // peer_groups.crew.agents[0]. The planner must HARD-block it instead.
        let mut cfg = empty_config();
        cfg.create_map_key("channels.discord", "main").unwrap();
        cfg.create_map_key("channels.discord", "backup").unwrap(); // type stays alive
        cfg.agents.insert(
            "bot".to_string(),
            AliasedAgentConfig {
                channels: vec!["discord.main".into()], // bot's ONLY discord channel
                ..Default::default()
            },
        );
        let mut crew = PeerGroupConfig {
            channel: "discord".into(), // bare type
            ..Default::default()
        };
        crew.agents.push(AgentAlias::new("bot"));
        cfg.peer_groups.insert("crew".to_string(), crew);

        let kind = AliasKind::Channel {
            channel_type: "discord".to_string(),
        };
        let report = plan_delete(&cfg, &kind, "main");
        assert!(
            !report.allowed,
            "member would be orphaned — must be refused: scrubs={:?}",
            report.scrubs
        );
        assert!(
            report
                .blockers
                .iter()
                .any(|b| b.path == "peer_groups.crew.agents[0]"),
            "{:?}",
            report.blockers
        );

        // If the member also has `discord.backup`, deleting `main` keeps it a
        // member of the bare group → allowed.
        cfg.agents.get_mut("bot").unwrap().channels =
            vec!["discord.main".into(), "discord.backup".into()];
        assert!(
            plan_delete(&cfg, &kind, "main").allowed,
            "member keeps a sibling discord channel → not orphaned"
        );
    }

    #[test]
    fn agent_delete_blocks_on_solely_owned_channel() {
        let mut cfg = empty_config();
        cfg.agents.insert(
            "bot".to_string(),
            AliasedAgentConfig {
                enabled: true,
                channels: vec!["discord.main".into()], // bot owns discord.main
                ..Default::default()
            },
        );
        // Deleting the sole enabled owner orphans the channel route → HARD block.
        let report = plan_delete(&cfg, &AliasKind::Agent, "bot");
        assert!(!report.allowed);
        assert!(
            report
                .blockers
                .iter()
                .any(|b| b.path == "agents.bot.channels[0]"),
            "{:?}",
            report.blockers
        );

        // A second enabled agent that also lists the channel keeps it owned, so
        // deleting `bot` no longer orphans it.
        cfg.agents.insert(
            "bot2".to_string(),
            AliasedAgentConfig {
                enabled: true,
                channels: vec!["discord.main".into()],
                ..Default::default()
            },
        );
        let report = plan_delete(&cfg, &AliasKind::Agent, "bot");
        assert!(
            report.allowed,
            "co-owned channel must not block: {:?}",
            report.blockers
        );

        // A DISABLED owner owns nothing, so its delete doesn't block either.
        let mut cfg = empty_config();
        cfg.agents.insert(
            "off".to_string(),
            AliasedAgentConfig {
                enabled: false,
                channels: vec!["discord.main".into()],
                ..Default::default()
            },
        );
        assert!(plan_delete(&cfg, &AliasKind::Agent, "off").allowed);
    }

    // ── delete_with_cascade (model providers) ───────────────────────────────

    fn cfg_with_provider(family: &str, alias: &str) -> Config {
        let mut c = empty_config();
        c.providers
            .models
            .ensure(family, alias)
            .expect("ensure creates the entry");
        c
    }

    #[test]
    fn cascade_refuses_when_model_provider_is_hard_ref() {
        let mut cfg = cfg_with_provider("anthropic", "default");
        cfg.agents.insert(
            "researcher".to_string(),
            AliasedAgentConfig {
                model_provider: "anthropic.default".into(),
                ..Default::default()
            },
        );
        let kind = provider_kind("anthropic");
        let err = delete_with_cascade(&mut cfg, &kind, "default", CascadePolicy::RefuseOnHard)
            .unwrap_err();
        match err {
            CascadeError::Refused(report) => assert_eq!(report.blockers.len(), 1),
            other => panic!("expected Refused, got {other:?}"),
        }
        // No mutation on refuse.
        assert!(cfg.providers.models.find("anthropic", "default").is_some());
        assert_eq!(
            cfg.agents["researcher"].model_provider.as_str(),
            "anthropic.default"
        );
    }

    #[test]
    fn cascade_scrubs_soft_refs_and_removes_entry() {
        let mut cfg = cfg_with_provider("anthropic", "default");
        // Another provider whose fallback points at the target.
        cfg.providers
            .models
            .ensure("openai", "main")
            .unwrap()
            .fallback = vec!["anthropic.default".into()];
        cfg.agents.insert(
            "triage".to_string(),
            AliasedAgentConfig {
                classifier_provider: "anthropic.default".into(),
                ..Default::default()
            },
        );
        cfg.model_routes.push(ModelRouteConfig {
            hint: "deep".to_string(),
            model_provider: "anthropic.default".to_string(),
            model: "claude".to_string(),
            api_key: None,
        });
        cfg.embedding_routes.push(EmbeddingRouteConfig {
            hint: "sem".to_string(),
            model_provider: "anthropic.default".to_string(),
            model: "emb".to_string(),
            dimensions: None,
            api_key: None,
        });

        let kind = provider_kind("anthropic");
        let report = delete_with_cascade(&mut cfg, &kind, "default", CascadePolicy::RefuseOnHard)
            .expect("soft-only delete succeeds");
        assert_eq!(
            report.applied.len(),
            4,
            "classifier + fallback + model_route + embedding_route"
        );
        assert_eq!(
            report.deleted_entry.as_deref(),
            Some("providers.models.anthropic.default")
        );
        assert!(cfg.providers.models.find("anthropic", "default").is_none());
        assert!(cfg.agents["triage"].classifier_provider.is_empty());
        assert!(
            cfg.providers
                .models
                .find("openai", "main")
                .unwrap()
                .fallback
                .is_empty()
        );
        assert!(cfg.model_routes.is_empty());
        assert!(cfg.embedding_routes.is_empty());
        assert!(find_all_references(&cfg, &kind, "default").is_empty());
    }

    #[test]
    fn cascade_scrubs_whitespace_padded_refs() {
        // scrub must trim like find/validate, else a padded ref find() flags is
        // left behind and the post-condition fails.
        let mut cfg = cfg_with_provider("anthropic", "default");
        cfg.agents.insert(
            "triage".to_string(),
            AliasedAgentConfig {
                classifier_provider: "  anthropic.default  ".into(),
                ..Default::default()
            },
        );
        cfg.model_routes.push(ModelRouteConfig {
            hint: "deep".to_string(),
            model_provider: " anthropic.default ".to_string(),
            model: "claude".to_string(),
            api_key: None,
        });
        let kind = provider_kind("anthropic");
        let report = delete_with_cascade(&mut cfg, &kind, "default", CascadePolicy::RefuseOnHard)
            .expect("padded soft refs scrubbed, post-condition passes");
        assert_eq!(report.applied.len(), 2);
        assert!(cfg.agents["triage"].classifier_provider.is_empty());
        assert!(cfg.model_routes.is_empty());
    }

    #[test]
    fn cascade_scrubs_all_matching_fallback_entries() {
        let mut cfg = cfg_with_provider("anthropic", "default");
        // openai.main lists the target twice in fallback (plus an unrelated one);
        // retain must drop BOTH matches and keep the unrelated entry.
        cfg.providers
            .models
            .ensure("openai", "main")
            .unwrap()
            .fallback = vec![
            "anthropic.default".into(),
            "anthropic.fast".into(),
            "anthropic.default".into(),
        ];
        let kind = provider_kind("anthropic");
        let report = delete_with_cascade(&mut cfg, &kind, "default", CascadePolicy::RefuseOnHard)
            .expect("soft-only delete succeeds");
        assert_eq!(
            report.applied.len(),
            2,
            "both matching fallback entries reported"
        );
        let fallback = &cfg
            .providers
            .models
            .find("openai", "main")
            .unwrap()
            .fallback;
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].as_str(), "anthropic.fast");
    }

    #[test]
    fn cascade_dry_run_mutates_nothing() {
        let mut cfg = cfg_with_provider("anthropic", "default");
        cfg.agents.insert(
            "triage".to_string(),
            AliasedAgentConfig {
                classifier_provider: "anthropic.default".into(),
                ..Default::default()
            },
        );
        let kind = provider_kind("anthropic");
        let report =
            delete_with_cascade(&mut cfg, &kind, "default", CascadePolicy::DryRun).unwrap();
        assert!(report.deleted_entry.is_none());
        assert!(report.applied.is_empty());
        assert_eq!(report.plan.scrubs.len(), 1);
        assert!(cfg.providers.models.find("anthropic", "default").is_some());
        assert_eq!(
            cfg.agents["triage"].classifier_provider.as_str(),
            "anthropic.default"
        );
    }

    #[test]
    fn cascade_not_found_for_missing_provider() {
        let mut cfg = empty_config();
        let err = delete_with_cascade(
            &mut cfg,
            &provider_kind("anthropic"),
            "ghost",
            CascadePolicy::RefuseOnHard,
        )
        .unwrap_err();
        assert!(matches!(err, CascadeError::NotFound(_)));
    }

    #[test]
    fn cascade_removes_unreferenced_provider() {
        let mut cfg = cfg_with_provider("anthropic", "spare");
        let report = delete_with_cascade(
            &mut cfg,
            &provider_kind("anthropic"),
            "spare",
            CascadePolicy::RefuseOnHard,
        )
        .unwrap();
        assert!(report.applied.is_empty());
        assert_eq!(
            report.deleted_entry.as_deref(),
            Some("providers.models.anthropic.spare")
        );
        assert!(cfg.providers.models.find("anthropic", "spare").is_none());
    }

    #[test]
    fn cascade_not_implemented_for_other_kinds() {
        let mut cfg = empty_config();
        assert!(matches!(
            delete_with_cascade(
                &mut cfg,
                &AliasKind::Channel {
                    channel_type: "discord".to_string()
                },
                "x",
                CascadePolicy::RefuseOnHard,
            ),
            Err(CascadeError::NotImplemented(_))
        ));
        let tts = AliasKind::Provider {
            category: ProviderCategory::Tts,
            family: "elevenlabs".to_string(),
        };
        assert!(matches!(
            delete_with_cascade(&mut cfg, &tts, "x", CascadePolicy::RefuseOnHard),
            Err(CascadeError::NotImplemented(_))
        ));
    }

    // ── delete_with_cascade (agents) ────────────────────────────────────────

    #[test]
    fn cascade_agent_refuses_when_heartbeat_enabled() {
        let mut cfg = empty_config();
        cfg.agents
            .insert("bot".to_string(), AliasedAgentConfig::default());
        cfg.heartbeat.enabled = true;
        cfg.heartbeat.agent = "bot".to_string();
        let err = delete_with_cascade(
            &mut cfg,
            &AliasKind::Agent,
            "bot",
            CascadePolicy::RefuseOnHard,
        )
        .unwrap_err();
        match err {
            CascadeError::Refused(report) => {
                assert_eq!(report.blockers.len(), 1);
                assert_eq!(report.blockers[0].path, "heartbeat.agent");
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        assert!(cfg.agents.contains_key("bot"));
        assert_eq!(cfg.heartbeat.agent.as_str(), "bot");
    }

    #[test]
    fn cascade_agent_refuses_when_solely_owned_channel() {
        // The agent arm of `delete_with_cascade` must also refuse on a sole-owned
        // channel — the second HARD agent ref besides an enabled `heartbeat.agent`
        // — before any mutation, locking the mutating path against future
        // scrub/collect drift (the plan-only case is `agent_delete_blocks_on_solely_owned_channel`).
        let mut cfg = empty_config();
        cfg.agents.insert(
            "bot".to_string(),
            AliasedAgentConfig {
                enabled: true,
                channels: vec!["discord.main".into()], // bot is the sole enabled owner
                ..Default::default()
            },
        );
        let err = delete_with_cascade(
            &mut cfg,
            &AliasKind::Agent,
            "bot",
            CascadePolicy::RefuseOnHard,
        )
        .unwrap_err();
        match err {
            CascadeError::Refused(report) => {
                assert!(
                    report
                        .blockers
                        .iter()
                        .any(|b| b.path == "agents.bot.channels[0]"),
                    "{:?}",
                    report.blockers
                );
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        // Refuse-before-mutate: the agent and its channel ownership survive intact.
        assert!(cfg.agents.contains_key("bot"));
        assert_eq!(cfg.agents["bot"].channels, vec!["discord.main".to_string()]);
    }

    #[test]
    fn cascade_agent_scrubs_all_soft_refs_and_removes() {
        let mut cfg = empty_config();
        cfg.agents
            .insert("bot".to_string(), AliasedAgentConfig::default());
        cfg.heartbeat.enabled = false; // disabled → heartbeat.agent is a SOFT ref
        cfg.heartbeat.agent = "bot".to_string();
        cfg.acp.default_agent = Some("bot".to_string());
        let mut lead = AliasedAgentConfig {
            delegates: vec!["bot".to_string()],
            ..Default::default()
        };
        lead.workspace
            .access
            .insert(AgentAlias::new("bot"), AccessMode::Read);
        lead.workspace.read_memory_from.push(AgentAlias::new("bot"));
        cfg.agents.insert("lead".to_string(), lead);
        cfg.peer_groups.insert(
            "crew".to_string(),
            PeerGroupConfig {
                agents: vec![AgentAlias::new("bot")],
                ..Default::default()
            },
        );

        let report = delete_with_cascade(
            &mut cfg,
            &AliasKind::Agent,
            "bot",
            CascadePolicy::RefuseOnHard,
        )
        .expect("soft-only agent delete succeeds");
        assert_eq!(report.applied.len(), 6);
        assert_eq!(report.deleted_entry.as_deref(), Some("agents.bot"));
        assert!(!cfg.agents.contains_key("bot"));
        assert!(cfg.heartbeat.agent.is_empty());
        assert!(cfg.acp.default_agent.is_none());
        assert!(cfg.agents["lead"].delegates.is_empty());
        assert!(cfg.agents["lead"].workspace.access.is_empty());
        assert!(cfg.agents["lead"].workspace.read_memory_from.is_empty());
        assert!(cfg.peer_groups["crew"].agents.is_empty());
        assert!(find_all_references(&cfg, &AliasKind::Agent, "bot").is_empty());
    }

    #[test]
    fn cascade_agent_scrub_trim_split_mirrors_find() {
        // Trimmed sites (heartbeat/acp/delegates) scrub a padded ref; raw sites
        // (read_memory_from) do not — exactly as find/validate.
        let mut cfg = empty_config();
        cfg.agents
            .insert("bot".to_string(), AliasedAgentConfig::default());
        cfg.heartbeat.enabled = false;
        cfg.heartbeat.agent = "  bot  ".to_string();
        cfg.acp.default_agent = Some(" bot ".to_string());
        let mut lead = AliasedAgentConfig {
            delegates: vec![" bot ".to_string()],
            ..Default::default()
        };
        lead.workspace
            .read_memory_from
            .push(AgentAlias::new(" bot ")); // raw, must remain
        cfg.agents.insert("lead".to_string(), lead);

        let report = delete_with_cascade(
            &mut cfg,
            &AliasKind::Agent,
            "bot",
            CascadePolicy::RefuseOnHard,
        )
        .expect("padded trimmed refs scrubbed, post-condition passes");
        assert_eq!(
            report.applied.len(),
            3,
            "heartbeat + acp + delegates (trimmed)"
        );
        assert!(cfg.heartbeat.agent.is_empty());
        assert!(cfg.acp.default_agent.is_none());
        assert!(cfg.agents["lead"].delegates.is_empty());
        // raw read_memory_from did not match " bot " != "bot" → untouched.
        assert_eq!(cfg.agents["lead"].workspace.read_memory_from.len(), 1);
    }

    #[test]
    fn cascade_agent_dry_run_mutates_nothing() {
        let mut cfg = empty_config();
        cfg.agents
            .insert("bot".to_string(), AliasedAgentConfig::default());
        cfg.acp.default_agent = Some("bot".to_string());
        let report =
            delete_with_cascade(&mut cfg, &AliasKind::Agent, "bot", CascadePolicy::DryRun).unwrap();
        assert!(report.deleted_entry.is_none());
        assert_eq!(report.plan.scrubs.len(), 1);
        assert!(cfg.agents.contains_key("bot"));
        assert_eq!(cfg.acp.default_agent.as_deref(), Some("bot"));
    }

    #[test]
    fn cascade_agent_not_found() {
        let mut cfg = empty_config();
        let err = delete_with_cascade(
            &mut cfg,
            &AliasKind::Agent,
            "ghost",
            CascadePolicy::RefuseOnHard,
        )
        .unwrap_err();
        assert!(matches!(err, CascadeError::NotFound(_)));
    }

    #[test]
    fn cascade_agent_self_reference_is_scrubbed() {
        // An agent that names ITSELF in delegates / read_memory_from: deleting it
        // must succeed (the scrub loop processes the to-be-deleted agent and
        // strips the self-refs before the entry is removed; the post-condition
        // then confirms nothing dangles).
        let mut cfg = empty_config();
        let mut bot = AliasedAgentConfig {
            delegates: vec!["bot".to_string()],
            ..Default::default()
        };
        bot.workspace.read_memory_from.push(AgentAlias::new("bot"));
        cfg.agents.insert("bot".to_string(), bot);

        let report = delete_with_cascade(
            &mut cfg,
            &AliasKind::Agent,
            "bot",
            CascadePolicy::RefuseOnHard,
        )
        .expect("self-referencing agent deletes cleanly");
        assert_eq!(report.deleted_entry.as_deref(), Some("agents.bot"));
        assert!(!cfg.agents.contains_key("bot"));
        assert!(find_all_references(&cfg, &AliasKind::Agent, "bot").is_empty());
    }
}
