//! Alias reference discovery for typed delete-with-cascade

use crate::schema::Config;

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

#[must_use]
pub fn alias_kind_for_map_path(path: &str) -> Option<AliasKind> {
    if path == "agents" {
        return Some(AliasKind::Agent);
    }

    if let Some(rest) = path.strip_prefix("providers.") {
        let (cat, family) = rest.split_once('.')?;
        if family.is_empty() || family.contains('.') {
            return None;
        }
        let category = match cat {
            "models" => ProviderCategory::Models,
            "tts" => ProviderCategory::Tts,
            "transcription" => ProviderCategory::Transcription,
            _ => return None,
        };
        return Some(AliasKind::Provider {
            category,
            family: family.to_string(),
        });
    }

    if let Some(ty) = path.strip_prefix("channels.") {
        if ty.is_empty() || ty.contains('.') {
            return None;
        }
        return Some(AliasKind::Channel {
            channel_type: ty.to_string(),
        });
    }

    None
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
    /// and remove the entry. The default.
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

impl CascadeReport {
    #[must_use]
    pub fn dirty_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .applied
            .iter()
            .map(|site| dirty_entry_for(&site.path))
            .collect();
        if let Some(entry) = &self.deleted_entry {
            paths.push(entry.clone());
        }
        paths.sort();
        paths.dedup();
        paths
    }
}

/// Truncate a [`RefSite`] dotted path to the entry/section path that an
/// incremental save (`apply_dirty_path`) re-serialises wholesale, so a nested
/// change (a dropped vec element or a removed/renamed map key) persists with the
/// whole entry rather than needing a leaf-precise dirty path.
#[must_use]
pub fn dirty_entry_for(refsite_path: &str) -> String {
    let segs: Vec<&str> = refsite_path.split('.').collect();
    match segs.first().copied() {
        // agents.<name>.* and peer_groups.<g>.* → the entry root.
        Some("agents" | "peer_groups") if segs.len() >= 2 => format!("{}.{}", segs[0], segs[1]),
        // providers.<cat>.<fam>.<alias>.* → the provider entry.
        Some("providers") if segs.len() >= 4 => segs[..4].join("."),
        // Scalars / whole-vector fields (heartbeat.agent, acp.default_agent,
        // escalation.alert_channels[i], model_routes[i]…) → strip any index.
        _ => refsite_path
            .split('[')
            .next()
            .unwrap_or(refsite_path)
            .to_string(),
    }
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

pub fn delete_with_cascade(
    cfg: &mut Config,
    kind: &AliasKind,
    alias: &str,
    policy: CascadePolicy,
) -> Result<CascadeReport, CascadeError> {
    let report = match kind {
        AliasKind::Provider {
            category: ProviderCategory::Models,
            family,
        } => delete_model_provider(cfg, family, alias, policy)?,
        AliasKind::Provider { .. } => {
            return Err(CascadeError::NotImplemented(
                "TTS/transcription provider delete-with-cascade is not yet implemented".to_string(),
            ));
        }
        AliasKind::Agent => delete_agent(cfg, alias, policy)?,
        AliasKind::Channel { channel_type } => delete_channel(cfg, channel_type, alias, policy)?,
    };
    if report.deleted_entry.is_some() {
        cfg.drop_secret_snapshots_in_section(&section_path(kind), alias);
    }
    Ok(report)
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

/// Whether `agent.advisor` is a model target naming `target` (`type.alias`).
fn advisor_model_ref_is(agent: &crate::schema::AliasedAgentConfig, target: &str) -> bool {
    agent
        .advisor
        .as_ref()
        .and_then(crate::advisor::AdvisorTarget::model_ref)
        .is_some_and(|model| model.trim() == target)
}

fn scrub_model_provider_refs(cfg: &mut Config, target: &str) {
    for agent in cfg.agents.values_mut() {
        if agent.classifier_provider.trim() == target {
            agent.classifier_provider = crate::providers::ModelProviderRef::default();
        }
        if agent.summary_provider.trim() == target {
            agent.summary_provider = crate::providers::ModelProviderRef::default();
        }
        if advisor_model_ref_is(agent, target) {
            agent.advisor = None;
        }
    }
    // Profile-level context-compression summarizer ref
    for profile in cfg.runtime_profiles.values_mut() {
        if profile.context_compression.summary_provider.trim() == target {
            profile.context_compression.summary_provider =
                crate::providers::ModelProviderRef::default();
        }
    }
    for (_ty, _al, profile) in cfg.providers.models.iter_entries_mut() {
        profile.fallback.retain(|fb| fb.trim() != target);
    }
    let removed_model_hints: Vec<String> = cfg
        .model_routes
        .iter()
        .filter(|r| r.model_provider.trim() == target)
        .map(|r| r.hint.clone())
        .collect();
    cfg.model_routes
        .retain(|r| r.model_provider.trim() != target);
    for hint in &removed_model_hints {
        cfg.drop_secret_snapshots_in_section("model_routes", hint);
    }

    let removed_embedding_hints: Vec<String> = cfg
        .embedding_routes
        .iter()
        .filter(|r| r.model_provider.trim() == target)
        .map(|r| r.hint.clone())
        .collect();
    cfg.embedding_routes
        .retain(|r| r.model_provider.trim() != target);
    for hint in &removed_embedding_hints {
        cfg.drop_secret_snapshots_in_section("embedding_routes", hint);
    }
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

fn delete_channel(
    cfg: &mut Config,
    channel_type: &str,
    alias: &str,
    policy: CascadePolicy,
) -> Result<CascadeReport, CascadeError> {
    let entry_path = format!("channels.{channel_type}.{alias}");
    let section = format!("channels.{channel_type}");
    let exists = cfg
        .get_map_keys(&section)
        .is_some_and(|keys| keys.iter().any(|k| k == alias));
    if !exists {
        return Err(CascadeError::NotFound(entry_path));
    }

    let kind = AliasKind::Channel {
        channel_type: channel_type.to_string(),
    };
    let report = plan_delete(cfg, &kind, alias);

    if policy == CascadePolicy::DryRun {
        return Ok(CascadeReport {
            plan: report,
            applied: Vec::new(),
            deleted_entry: None,
        });
    }
    // HARD channel refs (see `collect_channel_refs`): a mandatory dotted
    // `peer_groups.<g>.channel`, or a bare-type group member whose only
    // `<type>.*` channel is the target (scrubbing it would orphan the member).
    if !report.allowed {
        return Err(CascadeError::Refused(Box::new(report)));
    }

    let applied = report.scrubs.clone();
    let target = format!("{channel_type}.{alias}");
    scrub_channel_refs(cfg, &target);
    // Remove the `channels.<type>.<alias>` entry via the same generic map-key
    // path the gateway/CLI use.
    if let Err(e) = cfg.delete_map_key(&section, alias) {
        return Err(CascadeError::PostCondition(format!(
            "failed to remove {entry_path}: {e}"
        )));
    }

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

/// Mutating mirror of [`collect_channel_refs`]: drop the soft channel references
/// to `target` (`"<type>.<alias>"`). `peer_groups.<g>.channel` is a HARD ref and
/// is never scrubbed (a delete carrying one is refused before reaching here).
/// Comparisons `.trim()` to mirror `find_all_references` and `validate()`.
fn scrub_channel_refs(cfg: &mut Config, target: &str) {
    for agent in cfg.agents.values_mut() {
        agent.channels.retain(|ch| ch.trim() != target);
    }
    cfg.escalation
        .alert_channels
        .retain(|ch| ch.trim() != target);
}

// ── rename-with-cascade─────────────────────────────────────────────

const RESERVED_DEFAULT_AGENT: &str = "default";

#[must_use]
pub fn is_reserved_agent_alias(alias: &str) -> bool {
    alias.trim() == RESERVED_DEFAULT_AGENT
}

/// Why a [`create_map_key_checked`] did not create the key.
#[derive(Debug)]
pub enum CreateError {
    /// The key is the reserved alias for its section (the `default` agent).
    Reserved(String),
    /// The generated [`Config::create_map_key`] rejected the request: there is
    /// no map-keyed section at `path`, or the key is invalid. Carries the reason.
    Invalid(String),
}

impl std::fmt::Display for CreateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reserved(a) => write!(f, "alias `{a}` is reserved and cannot be created"),
            Self::Invalid(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for CreateError {}

pub fn create_map_key_checked(
    cfg: &mut Config,
    path: &str,
    key: &str,
) -> Result<bool, CreateError> {
    if path == "agents" && is_reserved_agent_alias(key) {
        return Err(CreateError::Reserved(RESERVED_DEFAULT_AGENT.to_string()));
    }
    cfg.create_map_key(path, key).map_err(CreateError::Invalid)
}

/// Outcome of a successful [`rename_with_cascade`].
#[derive(Debug, Clone)]
pub struct RenameReport {
    pub target_kind: AliasKind,
    /// The previous alias (now gone from config).
    pub old_alias: String,
    /// The alias the entry now lives under.
    pub new_alias: String,
    pub dirty_paths: Vec<String>,
}

/// Why a [`rename_with_cascade`] did not complete. Unlike [`CascadeError`] there
/// is no `Refused` variant: rename **rewrites** HARD references to follow the new
/// name rather than refusing, so the only failures are bad inputs and the
/// post-condition bug-guard.
#[derive(Debug)]
pub enum RenameError {
    /// The source alias does not exist.
    NotFound(String),
    /// The new alias is unusable: fails `validate_alias_key`, collides with an
    /// existing entry, or equals the current name. Carries the reason.
    InvalidName(String),
    /// The source or target alias is reserved (the `default` agent).
    Reserved(String),
    /// Bug guard: rewrite drifted from `find_all_references` and left a dangling
    /// reference to the OLD alias. **The config WAS mutated** (key swapped + refs
    /// rewritten) — the caller must NOT persist it. Unreachable while rewrite and
    /// the collect_* walks mirror each other (same sites, same trim split).
    PostCondition(String),
}

impl std::fmt::Display for RenameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(p) => write!(f, "alias not found: {p}"),
            Self::InvalidName(m) => write!(f, "invalid new alias: {m}"),
            Self::Reserved(a) => write!(f, "alias `{a}` is reserved and cannot be renamed"),
            Self::PostCondition(m) => write!(f, "rename post-condition failed: {m}"),
        }
    }
}

impl std::error::Error for RenameError {}

pub fn rename_with_cascade(
    cfg: &mut Config,
    kind: &AliasKind,
    old_alias: &str,
    new_alias: &str,
) -> Result<RenameReport, RenameError> {
    if old_alias == new_alias {
        return Err(RenameError::InvalidName(
            "new alias must differ from the current name".to_string(),
        ));
    }
    // Reserved-name guard (agent-scoped): the `default` agent is the runtime
    // fallback; renaming it away or onto it would silently change dispatch.
    if matches!(kind, AliasKind::Agent)
        && (old_alias == RESERVED_DEFAULT_AGENT || new_alias == RESERVED_DEFAULT_AGENT)
    {
        return Err(RenameError::Reserved(RESERVED_DEFAULT_AGENT.to_string()));
    }

    let section = section_path(kind);
    // `rename_map_key` validates `new_alias` via `validate_alias_key` (whose
    // leading-underscore rule also blocks the `_deleted` marker) and refuses a
    // collision, then swaps the entry key and rewrites secret snapshots.
    // `Ok(false)` = the source key is absent.
    match cfg.rename_map_key(&section, old_alias, new_alias) {
        Ok(true) => {}
        Ok(false) => return Err(RenameError::NotFound(entry_path(kind, old_alias))),
        Err(e) => return Err(RenameError::InvalidName(e)),
    }

    // Rewrite every referrer old → new. Mirrors the `collect_*_refs` walks
    // (same containers, same TRIM/RAW split) but replaces in place instead of
    // scrubbing. HARD refs are rewritten too — rename never refuses. Each rewrite
    // fn returns the entry/section paths it touched (for the surface to persist).
    let mut dirty_paths = match kind {
        AliasKind::Agent => rewrite_agent_refs(cfg, old_alias, new_alias),
        AliasKind::Provider { category, family } => {
            rewrite_provider_refs(cfg, *category, family, old_alias, new_alias)
        }
        AliasKind::Channel { channel_type } => {
            rewrite_channel_refs(cfg, channel_type, old_alias, new_alias)
        }
    };
    // The entry-key swap itself: the old key must be removed from disk and the
    // new key written. (`rename_map_key` already moved it in memory.)
    dirty_paths.push(entry_path(kind, old_alias));
    dirty_paths.push(entry_path(kind, new_alias));
    dirty_paths.sort();
    dirty_paths.dedup();

    // Post-condition: nothing may still reference the OLD alias. (Targeted, not a
    // global `validate()` — same rationale as `delete_with_cascade`.)
    let remaining = find_all_references(cfg, kind, old_alias);
    if !remaining.is_empty() {
        let paths: Vec<_> = remaining.iter().map(|s| s.path.as_str()).collect();
        return Err(RenameError::PostCondition(format!(
            "{} dangling reference(s) to {old_alias} remain after rewrite: {}",
            remaining.len(),
            paths.join(", ")
        )));
    }

    Ok(RenameReport {
        target_kind: kind.clone(),
        old_alias: old_alias.to_string(),
        new_alias: new_alias.to_string(),
        dirty_paths,
    })
}

/// The map-key section path for a kind — the `section_path` argument to
/// `Config::rename_map_key` / `Config::delete_map_key`.
fn section_path(kind: &AliasKind) -> String {
    match kind {
        AliasKind::Agent => "agents".to_string(),
        AliasKind::Provider { category, family } => {
            format!("providers.{}.{family}", provider_section(*category))
        }
        AliasKind::Channel { channel_type } => format!("channels.{channel_type}"),
    }
}

fn provider_section(category: ProviderCategory) -> &'static str {
    match category {
        ProviderCategory::Models => "models",
        ProviderCategory::Tts => "tts",
        ProviderCategory::Transcription => "transcription",
    }
}

/// The dotted entry path for a kind + alias (e.g. `agents.bot`,
/// `providers.models.anthropic.default`, `channels.discord.main`).
fn entry_path(kind: &AliasKind, alias: &str) -> String {
    format!("{}.{alias}", section_path(kind))
}

fn rewrite_agent_refs(cfg: &mut Config, old: &str, new: &str) -> Vec<String> {
    use crate::multi_agent::AgentAlias;
    let mut dirty = Vec::new();
    if cfg.heartbeat.agent.trim() == old {
        cfg.heartbeat.agent = new.to_string();
        dirty.push("heartbeat.agent".to_string());
    }
    let hit_acp = cfg
        .acp
        .default_agent
        .as_deref()
        .is_some_and(|da| da.trim() == old);
    if hit_acp {
        cfg.acp.default_agent = Some(new.to_string());
        dirty.push("acp.default_agent".to_string());
    }
    for (name, agent) in cfg.agents.iter_mut() {
        let mut touched = false;
        // workspace.access map key (raw match) — re-key, preserving the AccessMode.
        if let Some(mode) = agent.workspace.access.remove(&AgentAlias::new(old)) {
            agent.workspace.access.insert(AgentAlias::new(new), mode);
            touched = true;
        }
        // workspace.read_memory_from[] (raw match).
        for m in agent.workspace.read_memory_from.iter_mut() {
            if m.as_str() == old {
                *m = AgentAlias::new(new);
                touched = true;
            }
        }
        if touched {
            dirty.push(format!("agents.{name}"));
        }
    }
    for (gname, group) in cfg.peer_groups.iter_mut() {
        let mut touched = false;
        for m in group.agents.iter_mut() {
            if m.as_str() == old {
                *m = AgentAlias::new(new);
                touched = true;
            }
        }
        if touched {
            dirty.push(format!("peer_groups.{gname}"));
        }
    }
    dirty
}

/// Dispatch the provider rewrite by category (mirrors the `collect_provider_refs`
/// per-category arms).
fn rewrite_provider_refs(
    cfg: &mut Config,
    category: ProviderCategory,
    family: &str,
    old: &str,
    new: &str,
) -> Vec<String> {
    match category {
        ProviderCategory::Models => rewrite_model_provider_refs(cfg, family, old, new),
        ProviderCategory::Tts => rewrite_tts_provider_refs(cfg, family, old, new),
        ProviderCategory::Transcription => {
            rewrite_transcription_provider_refs(cfg, family, old, new)
        }
    }
}

fn rewrite_model_provider_refs(
    cfg: &mut Config,
    family: &str,
    old: &str,
    new: &str,
) -> Vec<String> {
    let old_target = format!("{family}.{old}");
    let new_target = format!("{family}.{new}");
    let mut dirty = Vec::new();
    for (name, agent) in cfg.agents.iter_mut() {
        let mut touched = false;
        if agent.model_provider.trim() == old_target {
            agent.model_provider = new_target.as_str().into();
            touched = true;
        }
        if agent.classifier_provider.trim() == old_target {
            agent.classifier_provider = new_target.as_str().into();
            touched = true;
        }
        if agent.summary_provider.trim() == old_target {
            agent.summary_provider = new_target.as_str().into();
            touched = true;
        }
        if advisor_model_ref_is(agent, &old_target) {
            agent.advisor = Some(crate::advisor::AdvisorTarget::Model(
                new_target.as_str().into(),
            ));
            touched = true;
        }
        if touched {
            dirty.push(format!("agents.{name}"));
        }
    }
    // Profile-level context-compression summarizer ref
    for (pname, profile) in cfg.runtime_profiles.iter_mut() {
        if profile.context_compression.summary_provider.trim() == old_target {
            profile.context_compression.summary_provider = new_target.as_str().into();
            dirty.push(format!(
                "runtime_profiles.{pname}.context_compression.summary_provider"
            ));
        }
    }
    for (ty, al, profile) in cfg.providers.models.iter_entries_mut() {
        let mut touched = false;
        for fb in profile.fallback.iter_mut() {
            if fb.trim() == old_target {
                *fb = new_target.as_str().into();
                touched = true;
            }
        }
        if touched {
            dirty.push(format!("providers.models.{ty}.{al}"));
        }
    }
    let mut routes_touched = false;
    for r in cfg.model_routes.iter_mut() {
        if r.model_provider.trim() == old_target {
            r.model_provider = new_target.clone(); // String field
            routes_touched = true;
        }
    }
    if routes_touched {
        dirty.push("model_routes".to_string());
    }
    let mut embed_touched = false;
    for r in cfg.embedding_routes.iter_mut() {
        if r.model_provider.trim() == old_target {
            r.model_provider = new_target.clone();
            embed_touched = true;
        }
    }
    if embed_touched {
        dirty.push("embedding_routes".to_string());
    }
    dirty
}

/// Rewrite the single optional `tts_provider` scalar (SOFT, TRIM-matched) from
/// `"<family>.<old>"` to `"<family>.<new>"` across all agents. Returns touched
/// `agents.<name>` paths.
fn rewrite_tts_provider_refs(cfg: &mut Config, family: &str, old: &str, new: &str) -> Vec<String> {
    let old_target = format!("{family}.{old}");
    let new_target = format!("{family}.{new}");
    let mut dirty = Vec::new();
    for (name, agent) in cfg.agents.iter_mut() {
        if agent.tts_provider.trim() == old_target {
            agent.tts_provider = new_target.as_str().into();
            dirty.push(format!("agents.{name}"));
        }
    }
    dirty
}

/// Rewrite the single optional `transcription_provider` scalar (SOFT,
/// TRIM-matched) from `"<family>.<old>"` to `"<family>.<new>"` across all agents.
/// Returns touched `agents.<name>` paths.
fn rewrite_transcription_provider_refs(
    cfg: &mut Config,
    family: &str,
    old: &str,
    new: &str,
) -> Vec<String> {
    let old_target = format!("{family}.{old}");
    let new_target = format!("{family}.{new}");
    let mut dirty = Vec::new();
    for (name, agent) in cfg.agents.iter_mut() {
        if agent.transcription_provider.trim() == old_target {
            agent.transcription_provider = new_target.as_str().into();
            dirty.push(format!("agents.{name}"));
        }
    }
    dirty
}

fn rewrite_channel_refs(cfg: &mut Config, channel_type: &str, old: &str, new: &str) -> Vec<String> {
    let old_target = format!("{channel_type}.{old}");
    let new_target = format!("{channel_type}.{new}");
    let mut dirty = Vec::new();
    for (name, agent) in cfg.agents.iter_mut() {
        let mut touched = false;
        for ch in agent.channels.iter_mut() {
            if ch.trim() == old_target {
                *ch = new_target.as_str().into();
                touched = true;
            }
        }
        if touched {
            dirty.push(format!("agents.{name}"));
        }
    }
    for (gname, group) in cfg.peer_groups.iter_mut() {
        if group.channel.trim() == old_target {
            group.channel = new_target.as_str().into();
            dirty.push(format!("peer_groups.{gname}"));
        }
    }
    let mut alert_touched = false;
    for ch in cfg.escalation.alert_channels.iter_mut() {
        if ch.trim() == old_target {
            *ch = new_target.clone(); // String field
            alert_touched = true;
        }
    }
    if alert_touched {
        dirty.push("escalation.alert_channels".to_string());
    }
    dirty
}

/// Enumerate every agent that references skill bundle `alias` (TRIM-matched, as
/// `Config::validate()` does). All refs are SOFT (droppable from the list).
#[must_use]
pub fn find_bundle_refs(cfg: &Config, alias: &str) -> Vec<RefSite> {
    let mut sites = Vec::new();
    for (name, agent) in sorted_agents(cfg) {
        for (i, b) in agent.skill_bundles.iter().enumerate() {
            if b.trim() == alias {
                sites.push(RefSite::soft(
                    format!("agents.{name}.skill_bundles[{i}]"),
                    ScrubAction::DropFromVec { index: i },
                    b.as_str(),
                ));
            }
        }
    }
    sites
}

/// Mutating mirror of [`find_bundle_refs`] for delete: drop `alias` from every
/// agent's `skill_bundles` list. Returns the touched `agents.<name>` dirty paths.
pub fn scrub_bundle_refs(cfg: &mut Config, alias: &str) -> Vec<String> {
    let mut dirty = Vec::new();
    for (name, agent) in cfg.agents.iter_mut() {
        let before = agent.skill_bundles.len();
        agent.skill_bundles.retain(|b| b.trim() != alias);
        if agent.skill_bundles.len() != before {
            dirty.push(format!("agents.{name}"));
        }
    }
    dirty
}

/// Mutating mirror for rename: rewrite every agent's `skill_bundles` entry
/// naming `old` to name `new`. Returns the touched `agents.<name>` dirty paths.
pub fn rewrite_bundle_refs(cfg: &mut Config, old: &str, new: &str) -> Vec<String> {
    let mut dirty = Vec::new();
    for (name, agent) in cfg.agents.iter_mut() {
        let mut touched = false;
        for b in agent.skill_bundles.iter_mut() {
            if b.trim() == old {
                *b = new.to_string();
                touched = true;
            }
        }
        if touched {
            dirty.push(format!("agents.{name}"));
        }
    }
    dirty
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
                if agent.summary_provider.trim() == target {
                    sites.push(RefSite::soft(
                        format!("agents.{name}.summary_provider"),
                        ScrubAction::ClearOptional,
                        agent.summary_provider.as_str(),
                    ));
                }
                if advisor_model_ref_is(agent, &target)
                    && let Some(advisor) = &agent.advisor
                {
                    sites.push(RefSite::soft(
                        format!("agents.{name}.advisor"),
                        ScrubAction::ClearOptional,
                        &advisor.to_string(),
                    ));
                }
            }
            // Profile-level context-compression summarizer ref
            {
                let mut pnames: Vec<&String> = cfg.runtime_profiles.keys().collect();
                pnames.sort();
                for pname in pnames {
                    let sp = &cfg.runtime_profiles[pname]
                        .context_compression
                        .summary_provider;
                    if sp.trim() == target {
                        sites.push(RefSite::soft(
                            format!(
                                "runtime_profiles.{pname}.context_compression.summary_provider"
                            ),
                            ScrubAction::ClearOptional,
                            sp.as_str(),
                        ));
                    }
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
mod tests;
