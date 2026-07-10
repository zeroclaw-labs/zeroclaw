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
    match kind {
        AliasKind::Provider {
            category: ProviderCategory::Models,
            family,
        } => delete_model_provider(cfg, family, alias, policy),
        AliasKind::Provider { .. } => Err(CascadeError::NotImplemented(
            "TTS/transcription provider delete-with-cascade is not yet implemented".to_string(),
        )),
        AliasKind::Agent => delete_agent(cfg, alias, policy),
        AliasKind::Channel { channel_type } => delete_channel(cfg, channel_type, alias, policy),
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

fn scrub_model_provider_refs(cfg: &mut Config, target: &str) {
    for agent in cfg.agents.values_mut() {
        if agent.classifier_provider.trim() == target {
            agent.classifier_provider = crate::providers::ModelProviderRef::default();
        }
        if agent.summary_provider.trim() == target {
            agent.summary_provider = crate::providers::ModelProviderRef::default();
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
        agent.delegates.retain(|d| d.agent().trim() != alias); // trimmed (validate trims)
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
    // collision, then swaps the entry key. `Ok(false)` = the source key is absent.
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
        for d in agent.delegates.iter_mut() {
            if d.agent().trim() == old {
                d.agent = new.to_string(); // trimmed (validate trims delegates)
                touched = true;
            }
        }
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
        // delegates[].agent — validate() trims.
        for (i, d) in agent.delegates.iter().enumerate() {
            if d.agent().trim() == alias {
                sites.push(RefSite::soft(
                    format!("agents.{name}.delegates[{i}].agent"),
                    ScrubAction::DropFromVec { index: i },
                    d.agent(),
                ));
            }
        }
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
mod tests {
    use super::*;
    use crate::multi_agent::{AccessMode, AgentAlias, PeerGroupConfig};
    use crate::schema::{
        AliasedAgentConfig, Config, DelegateTargetConfig, EmbeddingRouteConfig, ModelRouteConfig,
    };

    /// Empty config with the alias-keyed containers cleared so Config::default
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
            delegates: vec![DelegateTargetConfig::bounded("bot")],
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
                delegates: vec![DelegateTargetConfig::bounded(" bot ")],
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
        assert!(paths.contains(&"agents.lead.delegates[0].agent"));
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

    // ── review two delete-impact gaps ────────────────────────────────

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
        // Only TTS/transcription providers remain unimplemented now (model
        // providers, agents, and channels are all wired).
        let mut cfg = empty_config();
        for category in [ProviderCategory::Tts, ProviderCategory::Transcription] {
            let kind = AliasKind::Provider {
                category,
                family: "x".to_string(),
            };
            assert!(matches!(
                delete_with_cascade(&mut cfg, &kind, "x", CascadePolicy::RefuseOnHard),
                Err(CascadeError::NotImplemented(_))
            ));
        }
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
            delegates: vec![DelegateTargetConfig::bounded("bot")],
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
            delegates: vec![DelegateTargetConfig::bounded(" bot ")],
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
            delegates: vec![DelegateTargetConfig::bounded("bot")],
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

    // ── delete_with_cascade (channels) ──────────────────────────────────────

    fn channel_kind() -> AliasKind {
        AliasKind::Channel {
            channel_type: "discord".to_string(),
        }
    }

    fn has_channel(cfg: &Config, alias: &str) -> bool {
        cfg.get_map_keys("channels.discord")
            .unwrap_or_default()
            .iter()
            .any(|k| k == alias)
    }

    #[test]
    fn cascade_channel_scrubs_soft_refs_and_removes_entry() {
        let mut cfg = empty_config();
        cfg.create_map_key("channels.discord", "main").unwrap();
        cfg.agents.insert(
            "ops".to_string(),
            AliasedAgentConfig {
                channels: vec!["discord.main".into()],
                ..Default::default()
            },
        );
        cfg.escalation
            .alert_channels
            .push("discord.main".to_string());

        let report = delete_with_cascade(
            &mut cfg,
            &channel_kind(),
            "main",
            CascadePolicy::RefuseOnHard,
        )
        .expect("soft-only channel delete succeeds");
        assert_eq!(report.applied.len(), 2, "agent channel + alert_channel");
        assert_eq!(
            report.deleted_entry.as_deref(),
            Some("channels.discord.main")
        );
        assert!(!has_channel(&cfg, "main"));
        assert!(cfg.agents["ops"].channels.is_empty());
        assert!(cfg.escalation.alert_channels.is_empty());
        assert!(find_all_references(&cfg, &channel_kind(), "main").is_empty());
    }

    #[test]
    fn cascade_channel_refuses_on_hard_peer_group_ref() {
        let mut cfg = empty_config();
        cfg.create_map_key("channels.discord", "main").unwrap();
        cfg.peer_groups.insert(
            "crew".to_string(),
            PeerGroupConfig {
                channel: "discord.main".into(),
                ..Default::default()
            },
        );
        let err = delete_with_cascade(
            &mut cfg,
            &channel_kind(),
            "main",
            CascadePolicy::RefuseOnHard,
        )
        .unwrap_err();
        match err {
            CascadeError::Refused(report) => {
                assert_eq!(report.blockers[0].path, "peer_groups.crew.channel");
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        assert!(has_channel(&cfg, "main"), "no mutation on refuse");
    }

    #[test]
    fn cascade_channel_dry_run_mutates_nothing() {
        let mut cfg = empty_config();
        cfg.create_map_key("channels.discord", "main").unwrap();
        cfg.agents.insert(
            "ops".to_string(),
            AliasedAgentConfig {
                channels: vec!["discord.main".into()],
                ..Default::default()
            },
        );
        let report =
            delete_with_cascade(&mut cfg, &channel_kind(), "main", CascadePolicy::DryRun).unwrap();
        assert!(report.deleted_entry.is_none());
        assert_eq!(report.plan.scrubs.len(), 1);
        assert!(has_channel(&cfg, "main"));
        assert_eq!(cfg.agents["ops"].channels.len(), 1);
    }

    #[test]
    fn cascade_channel_not_found() {
        let mut cfg = empty_config();
        let err = delete_with_cascade(
            &mut cfg,
            &channel_kind(),
            "ghost",
            CascadePolicy::RefuseOnHard,
        )
        .unwrap_err();
        assert!(matches!(err, CascadeError::NotFound(_)));
    }

    #[test]
    fn cascade_channel_refuses_orphaning_bare_group_member() {
        // BARE-type group ("discord", not "discord.main"). validate
        // (schema.rs:17461-17478) requires each member to keep some `discord.*`
        // channel. `ops`'s only discord channel is the one being deleted, so the
        // delete must REFUSE — scrubbing it would yield a config validate() rejects.
        let mut cfg = empty_config();
        cfg.create_map_key("channels.discord", "main").unwrap();
        cfg.agents.insert(
            "ops".to_string(),
            AliasedAgentConfig {
                channels: vec!["discord.main".into()],
                ..Default::default()
            },
        );
        let mut group = PeerGroupConfig {
            channel: "discord".into(), // bare type
            ..Default::default()
        };
        group.agents.push(AgentAlias::new("ops"));
        cfg.peer_groups.insert("crew".to_string(), group);

        let err = delete_with_cascade(
            &mut cfg,
            &channel_kind(),
            "main",
            CascadePolicy::RefuseOnHard,
        )
        .unwrap_err();
        match err {
            CascadeError::Refused(report) => {
                assert!(
                    report
                        .blockers
                        .iter()
                        .any(|b| b.path == "peer_groups.crew.agents[0]"),
                    "bare-group member orphan must be a hard blocker, got {:?}",
                    report.blockers
                );
            }
            other => panic!("expected Refused, got {other:?}"),
        }
        assert!(has_channel(&cfg, "main"), "no mutation on refuse");
        assert_eq!(
            cfg.agents["ops"].channels.len(),
            1,
            "member channel not scrubbed on refuse"
        );
    }

    #[test]
    fn cascade_channel_proceeds_when_bare_group_member_keeps_another() {
        // Same bare-type group, but `ops` also has `discord.backup`. Deleting
        // `discord.main` leaves it with a surviving `discord.*`, so membership
        // stays valid and the delete proceeds (scrubbing only the main ref).
        let mut cfg = empty_config();
        cfg.create_map_key("channels.discord", "main").unwrap();
        cfg.create_map_key("channels.discord", "backup").unwrap();
        cfg.agents.insert(
            "ops".to_string(),
            AliasedAgentConfig {
                channels: vec!["discord.main".into(), "discord.backup".into()],
                ..Default::default()
            },
        );
        let mut group = PeerGroupConfig {
            channel: "discord".into(), // bare type
            ..Default::default()
        };
        group.agents.push(AgentAlias::new("ops"));
        cfg.peer_groups.insert("crew".to_string(), group);

        let report = delete_with_cascade(
            &mut cfg,
            &channel_kind(),
            "main",
            CascadePolicy::RefuseOnHard,
        )
        .expect("delete proceeds when a sibling channel keeps membership valid");
        assert_eq!(
            report.deleted_entry.as_deref(),
            Some("channels.discord.main")
        );
        assert!(!has_channel(&cfg, "main"));
        let remaining: Vec<&str> = cfg.agents["ops"]
            .channels
            .iter()
            .map(|c| c.as_str())
            .collect();
        assert_eq!(
            remaining,
            vec!["discord.backup"],
            "only the deleted channel is scrubbed; backup survives"
        );
    }

    // ── rename_with_cascade─────────────────────────────────────────

    #[test]
    fn rename_agent_rewrites_every_ref_kind() {
        let mut cfg = empty_config();
        cfg.heartbeat.enabled = true;
        cfg.heartbeat.agent = "bot".to_string(); // HARD ref — rename rewrites it
        cfg.acp.default_agent = Some("bot".to_string());
        // The renamed agent itself self-delegates (must be rewritten too).
        let mut bot = AliasedAgentConfig {
            delegates: vec![DelegateTargetConfig::bounded("bot")],
            ..Default::default()
        };
        bot.workspace
            .access
            .insert(AgentAlias::new("bot"), AccessMode::Read);
        cfg.agents.insert("bot".to_string(), bot);
        // A referrer agent pointing at bot every which way.
        let mut lead = AliasedAgentConfig {
            delegates: vec![DelegateTargetConfig::bounded("bot")],
            ..Default::default()
        };
        lead.workspace
            .access
            .insert(AgentAlias::new("bot"), AccessMode::Read);
        lead.workspace.read_memory_from.push(AgentAlias::new("bot"));
        cfg.agents.insert("lead".to_string(), lead);
        let mut group = PeerGroupConfig::default();
        group.agents.push(AgentAlias::new("bot"));
        cfg.peer_groups.insert("crew".to_string(), group);

        let report = rename_with_cascade(&mut cfg, &AliasKind::Agent, "bot", "bot2")
            .expect("agent rename succeeds");
        assert_eq!(report.new_alias, "bot2");
        // entry moved
        assert!(!cfg.agents.contains_key("bot"));
        assert!(cfg.agents.contains_key("bot2"));
        // every ref now names bot2
        assert_eq!(cfg.heartbeat.agent, "bot2");
        assert_eq!(cfg.acp.default_agent.as_deref(), Some("bot2"));
        assert_eq!(
            cfg.agents["bot2"].delegates,
            vec![DelegateTargetConfig::bounded("bot2")]
        );
        assert!(
            cfg.agents["bot2"]
                .workspace
                .access
                .contains_key(&AgentAlias::new("bot2"))
        );
        assert_eq!(
            cfg.agents["lead"].delegates,
            vec![DelegateTargetConfig::bounded("bot2")]
        );
        assert!(
            cfg.agents["lead"]
                .workspace
                .access
                .contains_key(&AgentAlias::new("bot2"))
        );
        assert_eq!(
            cfg.agents["lead"].workspace.read_memory_from,
            vec![AgentAlias::new("bot2")]
        );
        assert_eq!(
            cfg.peer_groups["crew"].agents,
            vec![AgentAlias::new("bot2")]
        );
        // post-condition: nothing references the old alias
        assert!(find_all_references(&cfg, &AliasKind::Agent, "bot").is_empty());
        // dirty paths cover every touched entry/section + the entry-key swap, so
        // the surface persists exactly what changed (and nothing stays stale).
        for expected in [
            "heartbeat.agent",
            "acp.default_agent",
            "agents.bot", // old entry removed on disk
            "agents.bot2",
            "agents.lead",
            "peer_groups.crew",
        ] {
            assert!(
                report.dirty_paths.iter().any(|p| p == expected),
                "missing dirty path {expected:?} in {:?}",
                report.dirty_paths
            );
        }
        // sorted + deduplicated
        let mut sorted = report.dirty_paths.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted, report.dirty_paths);
    }

    #[test]
    fn rename_agent_not_found() {
        let mut cfg = empty_config();
        let err = rename_with_cascade(&mut cfg, &AliasKind::Agent, "ghost", "specter").unwrap_err();
        assert!(matches!(err, RenameError::NotFound(_)));
    }

    #[test]
    fn rename_agent_collision_is_invalid() {
        let mut cfg = empty_config();
        cfg.agents
            .insert("bot".to_string(), AliasedAgentConfig::default());
        cfg.agents
            .insert("other".to_string(), AliasedAgentConfig::default());
        let err = rename_with_cascade(&mut cfg, &AliasKind::Agent, "bot", "other").unwrap_err();
        assert!(matches!(err, RenameError::InvalidName(_)));
        // no mutation: both entries still present
        assert!(cfg.agents.contains_key("bot"));
        assert!(cfg.agents.contains_key("other"));
    }

    #[test]
    fn rename_agent_noop_is_invalid() {
        let mut cfg = empty_config();
        cfg.agents
            .insert("bot".to_string(), AliasedAgentConfig::default());
        let err = rename_with_cascade(&mut cfg, &AliasKind::Agent, "bot", "bot").unwrap_err();
        assert!(matches!(err, RenameError::InvalidName(_)));
    }

    #[test]
    fn rename_default_agent_is_reserved_both_directions() {
        let mut cfg = empty_config();
        cfg.agents
            .insert("default".to_string(), AliasedAgentConfig::default());
        cfg.agents
            .insert("bot".to_string(), AliasedAgentConfig::default());
        // can't rename the default agent away
        let from =
            rename_with_cascade(&mut cfg, &AliasKind::Agent, "default", "primary").unwrap_err();
        assert!(matches!(from, RenameError::Reserved(_)));
        // can't rename another agent onto `default`
        let onto = rename_with_cascade(&mut cfg, &AliasKind::Agent, "bot", "default").unwrap_err();
        assert!(matches!(onto, RenameError::Reserved(_)));
        // nothing mutated
        assert!(cfg.agents.contains_key("default"));
        assert!(cfg.agents.contains_key("bot"));
    }

    #[test]
    fn is_reserved_agent_alias_flags_only_default() {
        // The shared create guard uses this to refuse `default` symmetrically
        // with the rename guard (so no surface can author an undeletable agent).
        assert!(is_reserved_agent_alias("default"));
        assert!(is_reserved_agent_alias("  default  ")); // trims before comparing
        assert!(!is_reserved_agent_alias("default2"));
        assert!(!is_reserved_agent_alias("cronos"));
        assert!(!is_reserved_agent_alias(""));
    }

    #[test]
    fn create_map_key_checked_refuses_reserved_default_agent() {
        let mut cfg = empty_config();
        // The reserved `default` agent cannot be created, and nothing is
        // inserted -- the create analogue of rename_default_agent_is_reserved.
        let err = create_map_key_checked(&mut cfg, "agents", "default").unwrap_err();
        assert!(matches!(err, CreateError::Reserved(_)));
        assert!(!cfg.agents.contains_key("default"));
        // A whitespace-padded variant is refused the same way.
        assert!(matches!(
            create_map_key_checked(&mut cfg, "agents", "  default  ").unwrap_err(),
            CreateError::Reserved(_)
        ));
        // A non-reserved agent alias is created and persisted in memory.
        assert!(create_map_key_checked(&mut cfg, "agents", "scout").unwrap());
        assert!(cfg.agents.contains_key("scout"));
        // Agent-scoped only: `default` is a free key for non-agent kinds, so the
        // guard delegates rather than refusing it as reserved.
        assert!(create_map_key_checked(&mut cfg, "providers.models.anthropic", "default").unwrap());
        // An unknown section surfaces as Invalid, not Reserved.
        assert!(matches!(
            create_map_key_checked(&mut cfg, "not.a.real.section", "x").unwrap_err(),
            CreateError::Invalid(_)
        ));
    }

    #[test]
    fn ensure_map_key_for_path_refuses_reserved_default_agent() {
        let mut cfg = empty_config();
        // A set-prop on a nonexistent `agents.default` must NOT auto-vivify the
        // reserved runtime-fallback agent, and signals the refusal (true) so the
        // set-prop surface returns a reserved error (PUT /prop, PATCH, RPC set).
        assert!(cfg.ensure_map_key_for_path("agents.default.enabled"));
        assert!(!cfg.agents.contains_key("default"));
        // A non-reserved agent IS vivified (not refused), as normal set-prop-on-new.
        assert!(!cfg.ensure_map_key_for_path("agents.scout.enabled"));
        assert!(cfg.agents.contains_key("scout"));
        // An already-present `default` (e.g. migration-synthesized) is left intact
        // and still configurable: the existence check returns false (not refused).
        cfg.agents
            .insert("default".to_string(), AliasedAgentConfig::default());
        assert!(!cfg.ensure_map_key_for_path("agents.default.model"));
        assert!(cfg.agents.contains_key("default"));
    }

    #[test]
    fn rename_rejects_deleted_marker_target() {
        // `_deleted` is blocked as a new alias by validate_alias_key (leading
        // underscore) — surfaced as InvalidName via rename_map_key.
        let mut cfg = empty_config();
        cfg.agents
            .insert("bot".to_string(), AliasedAgentConfig::default());
        let err = rename_with_cascade(&mut cfg, &AliasKind::Agent, "bot", "_deleted").unwrap_err();
        assert!(matches!(err, RenameError::InvalidName(_)));
        assert!(cfg.agents.contains_key("bot"));
    }

    #[test]
    fn rename_model_provider_rewrites_dotted_refs() {
        let mut cfg = cfg_with_provider("anthropic", "default");
        cfg.agents.insert(
            "researcher".to_string(),
            AliasedAgentConfig {
                model_provider: "anthropic.default".into(), // HARD — rewritten, not refused
                classifier_provider: "anthropic.default".into(),
                ..Default::default()
            },
        );
        // another provider whose fallback names the target
        cfg.providers
            .models
            .ensure("openai", "fast")
            .expect("ensure");
        for (_t, al, p) in cfg.providers.models.iter_entries_mut() {
            if al == "fast" {
                p.fallback.push("anthropic.default".into());
            }
        }
        cfg.model_routes.push(ModelRouteConfig {
            hint: "deep".to_string(),
            model_provider: "anthropic.default".to_string(),
            model: "claude".to_string(),
            api_key: None,
        });

        let kind = provider_kind("anthropic");
        let report = rename_with_cascade(&mut cfg, &kind, "default", "prod")
            .expect("provider rename succeeds");
        assert_eq!(report.new_alias, "prod");
        assert!(cfg.providers.models.find("anthropic", "default").is_none());
        assert!(cfg.providers.models.find("anthropic", "prod").is_some());
        assert_eq!(
            cfg.agents["researcher"].model_provider.as_str(),
            "anthropic.prod"
        );
        assert_eq!(
            cfg.agents["researcher"].classifier_provider.as_str(),
            "anthropic.prod"
        );
        assert_eq!(cfg.model_routes[0].model_provider, "anthropic.prod");
        let fast_fallback: Vec<String> = cfg
            .providers
            .models
            .iter_entries()
            .filter(|(_, al, _)| *al == "fast")
            .flat_map(|(_, _, p)| p.fallback.iter().map(|f| f.as_str().to_string()))
            .collect();
        assert_eq!(
            fast_fallback,
            vec!["anthropic.prod".to_string()],
            "fallback rewritten"
        );
        assert!(find_all_references(&cfg, &kind, "default").is_empty());
    }

    #[test]
    fn rename_channel_rewrites_refs_and_preserves_bare_group_membership() {
        let mut cfg = empty_config();
        cfg.create_map_key("channels.discord", "main").unwrap();
        // member of a BARE-type group whose only discord channel is the target:
        // delete would REFUSE (orphan), but rename keeps membership valid.
        cfg.agents.insert(
            "ops".to_string(),
            AliasedAgentConfig {
                channels: vec!["discord.main".into()],
                ..Default::default()
            },
        );
        let mut group = PeerGroupConfig {
            channel: "discord".into(), // bare type
            ..Default::default()
        };
        group.agents.push(AgentAlias::new("ops"));
        cfg.peer_groups.insert("crew".to_string(), group);
        // also a dotted peer-group channel + an alert channel
        cfg.peer_groups.insert(
            "ops_team".to_string(),
            PeerGroupConfig {
                channel: "discord.main".into(), // dotted HARD ref — rewritten
                ..Default::default()
            },
        );
        cfg.escalation
            .alert_channels
            .push("discord.main".to_string());

        let kind = channel_kind();
        let report = rename_with_cascade(&mut cfg, &kind, "main", "primary")
            .expect("channel rename succeeds (no orphan, unlike delete)");
        assert_eq!(report.new_alias, "primary");
        assert!(!has_channel(&cfg, "main"));
        assert!(has_channel(&cfg, "primary"));
        assert_eq!(
            cfg.agents["ops"]
                .channels
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>(),
            vec!["discord.primary"],
            "member still has a discord.* channel — membership preserved"
        );
        assert_eq!(
            cfg.peer_groups["ops_team"].channel.as_str(),
            "discord.primary"
        );
        assert_eq!(
            cfg.escalation.alert_channels,
            vec!["discord.primary".to_string()]
        );
        assert!(find_all_references(&cfg, &kind, "main").is_empty());
    }

    #[test]
    fn rename_tts_provider_rewrites_scalar() {
        let mut cfg = empty_config();
        cfg.create_map_key("providers.tts.elevenlabs", "default")
            .expect("create tts entry");
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
        let report = rename_with_cascade(&mut cfg, &kind, "default", "studio")
            .expect("tts provider rename succeeds");
        assert!(report.dirty_paths.iter().any(|p| p == "agents.voice"));
        assert!(
            report
                .dirty_paths
                .iter()
                .any(|p| p == "providers.tts.elevenlabs.studio")
        );
        assert_eq!(
            cfg.agents["voice"].tts_provider.as_str(),
            "elevenlabs.studio"
        );
        assert!(find_all_references(&cfg, &kind, "default").is_empty());
    }

    #[test]
    fn dirty_entry_for_truncates_ref_paths_to_persistable_entries() {
        // agent / peer-group referrer sites → the entry root (whole subtree).
        assert_eq!(
            dirty_entry_for("agents.lead.delegates[0].agent"),
            "agents.lead"
        );
        assert_eq!(
            dirty_entry_for("agents.lead.workspace.access.bot"),
            "agents.lead"
        );
        assert_eq!(
            dirty_entry_for("peer_groups.crew.agents[1]"),
            "peer_groups.crew"
        );
        // scalars / whole-vector fields → the field/section, index stripped.
        assert_eq!(dirty_entry_for("heartbeat.agent"), "heartbeat.agent");
        assert_eq!(dirty_entry_for("acp.default_agent"), "acp.default_agent");
        assert_eq!(
            dirty_entry_for("escalation.alert_channels[3]"),
            "escalation.alert_channels"
        );
        assert_eq!(
            dirty_entry_for("model_routes[0].model_provider"),
            "model_routes"
        );
        // provider entry → the 4-segment entry path.
        assert_eq!(
            dirty_entry_for("providers.models.anthropic.default.fallback[0]"),
            "providers.models.anthropic.default"
        );
    }

    #[test]
    fn cascade_report_dirty_paths_covers_scrubs_and_deleted_entry() {
        // A delete that scrubbed two referrers in different entries + removed the
        // entry must report all three dirty paths (deduped, sorted).
        let mut cfg = empty_config();
        cfg.heartbeat.enabled = false;
        cfg.heartbeat.agent = "bot".to_string();
        cfg.agents
            .insert("bot".to_string(), AliasedAgentConfig::default());
        cfg.agents.insert(
            "lead".to_string(),
            AliasedAgentConfig {
                delegates: vec![DelegateTargetConfig::bounded("bot")],
                ..Default::default()
            },
        );
        let report = delete_with_cascade(
            &mut cfg,
            &AliasKind::Agent,
            "bot",
            CascadePolicy::RefuseOnHard,
        )
        .expect("delete succeeds");
        let dirty = report.dirty_paths();
        assert!(
            dirty.contains(&"agents.bot".to_string()),
            "removed entry: {dirty:?}"
        );
        assert!(
            dirty.contains(&"agents.lead".to_string()),
            "scrubbed delegate: {dirty:?}"
        );
        assert!(
            dirty.contains(&"heartbeat.agent".to_string()),
            "cleared heartbeat: {dirty:?}"
        );
    }

    #[test]
    fn bundle_refs_find_scrub_rewrite() {
        let mut cfg = empty_config();
        cfg.agents.insert(
            "a".to_string(),
            AliasedAgentConfig {
                skill_bundles: vec!["util".to_string(), "web".to_string()],
                ..Default::default()
            },
        );
        cfg.agents.insert(
            "b".to_string(),
            AliasedAgentConfig {
                skill_bundles: vec![" util ".to_string()], // padded — validate trims
                ..Default::default()
            },
        );

        // find (trim-matched across both agents)
        let sites = find_bundle_refs(&cfg, "util");
        assert_eq!(sites.len(), 2, "{sites:?}");
        assert!(sites.iter().all(|s| s.strength == RefStrength::Soft));

        // rewrite util -> tools
        let dirty = rewrite_bundle_refs(&mut cfg, "util", "tools");
        assert_eq!(dirty.len(), 2);
        assert_eq!(
            cfg.agents["a"].skill_bundles,
            vec!["tools".to_string(), "web".to_string()]
        );
        assert_eq!(cfg.agents["b"].skill_bundles, vec!["tools".to_string()]);
        assert!(find_bundle_refs(&cfg, "util").is_empty());

        // scrub tools from all agents
        let dirty = scrub_bundle_refs(&mut cfg, "tools");
        assert_eq!(dirty.len(), 2);
        assert_eq!(cfg.agents["a"].skill_bundles, vec!["web".to_string()]);
        assert!(cfg.agents["b"].skill_bundles.is_empty());
        assert!(find_bundle_refs(&cfg, "tools").is_empty());
    }
}
