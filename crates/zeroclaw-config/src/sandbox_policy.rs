use crate::autonomy::AutonomyLevel;
use crate::schema::{
    DEFAULT_ALLOW_WRITE, MANDATORY_DENY_WRITE, RiskProfileConfig, SandboxPolicyConfig,
};
use std::path::{Path, PathBuf};

/// Post-precedence, pre-path-resolution sandbox inputs: raw operator strings,
/// after canonical-vs-legacy precedence has been decided but before `~`
/// expansion / workspace-relative resolution.
///
/// This is the single place canonical-over-legacy precedence is decided.
/// Both `SandboxPolicy::from_risk_profile` (OS-sandbox resolution, this
/// module) and `SecurityPolicy::from_profiles` (`crate::policy`, app-layer
/// path guard) build on top of it, so the two enforcement surfaces can never
/// resolve a mixed legacy/canonical config differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveSandboxInputs {
    /// `sandbox_policy.deny_read` if `Some`, else legacy `forbidden_paths`.
    pub deny_read: Vec<String>,
    /// `sandbox_policy.allow_read` if `Some`, else legacy `allowed_roots`.
    pub allow_read: Vec<String>,
    /// See [`Self::from_profile`] for the full `allow_write` precedence rules.
    pub allow_write: Vec<String>,
    /// Whether `allow_write` came from an operator-supplied `sandbox_policy.allow_write
    /// = Some(_)` (`true`) rather than the omitted-field `None` fallback (`false`).
    /// `DEFAULT_ALLOW_WRITE` (`[".", "/tmp"]`) exists to satisfy OS-sandbox bind-mount
    /// needs for the fallback case, not as an app-layer grant — `sandbox_derived_tiers`
    /// (`crate::policy`) uses this flag to tell an explicit `allow_write = ["/tmp"]`
    /// apart from the same value arriving via fallback, so an explicit grant is not
    /// silently stripped from the app-layer write-only tier. `workspace_only = true`
    /// forcing `allow_write` to `[workspace]` does not set this — that path is a
    /// deliberate override of whatever `allow_write` held, not an operator grant this
    /// flag protects.
    pub allow_write_is_explicit: bool,
    /// RFC 6996: `true` when `allow_write` was compat-derived from a
    /// NON-EMPTY legacy `allowed_roots` under effective
    /// `workspace_only = false` (including `Full` autonomy). The merged set
    /// ([`DEFAULT_ALLOW_WRITE`] ∪ `allowed_roots`) is then enforced as a REAL
    /// write allowlist at the app layer — an intentional conversion of the
    /// historical additive-only behavior. An empty/omitted legacy list derives
    /// nothing (`false`): it must never narrow an unrestricted profile to the
    /// default write roots, because the legacy field cannot distinguish
    /// "omitted" from an explicit `[]`.
    pub allow_write_compat_allowlist: bool,
    /// `workspace_only` after autonomy resolution: always `false` at `Full`
    /// autonomy regardless of the configured value (existing behavior,
    /// preserved by RFC 6996). This is the single decision point both the
    /// write-grant resolution below and the app-layer policy
    /// (`SecurityPolicy::from_profiles`) consume, so the two surfaces cannot
    /// disagree on what `workspace_only` means.
    pub effective_workspace_only: bool,
    /// Operator-supplied `deny_write` entries ONLY (`sandbox_policy
    /// .deny_write.unwrap_or_default()`). The [`MANDATORY_DENY_WRITE`]
    /// guardrail list is always active (RFC 6996: no all-or-nothing switch)
    /// and is enforced separately from these absolute, exception-proof
    /// entries — see `SecurityPolicy::is_resolved_path_allowed` and
    /// `SandboxPolicy::from_effective` (which merges the list back for the
    /// OS-sandbox view).
    pub deny_write: Vec<String>,
    /// Per-entry exceptions to the [`MANDATORY_DENY_WRITE`] guardrail list
    /// (`sandbox_policy.guardrail_exceptions`, default `[]`). An exception
    /// uses the same matching rules as a guardrail entry and re-permits only
    /// the named file or subtree; operator `deny_write` entries are never
    /// relaxable. A non-empty list emits a visible WARN.
    pub guardrail_exceptions: Vec<String>,
}

impl EffectiveSandboxInputs {
    /// Resolve canonical-vs-legacy precedence for `profile` against `workspace`.
    ///
    /// Precedence (canonical `sandbox_policy` field wins whenever present):
    /// 1. `deny_read` — `sandbox_policy.deny_read` if `Some` (including
    ///    explicit `Some(vec![])`), else legacy `forbidden_paths`.
    /// 2. `allow_read` — `sandbox_policy.allow_read` if `Some`, else legacy
    ///    `allowed_roots`.
    /// 3. `allow_write` — `sandbox_policy.allow_write` if `Some` (exactly, no
    ///    legacy merge — even if it happens to equal the old default shape —
    ///    and authoritative regardless of `workspace_only`, which scopes only
    ///    the implicit grant); effective `workspace_only = true` (configured
    ///    `workspace_only` at any autonomy below `Full`) with `None` → the
    ///    workspace root only; otherwise (`None`) [`DEFAULT_ALLOW_WRITE`]
    ///    merged with legacy `allowed_roots`. When that legacy list is
    ///    NON-EMPTY the merged set is a real write allowlist
    ///    (`allow_write_compat_allowlist`); an empty legacy list derives
    ///    nothing and writes stay unrestricted mod `deny_write`.
    /// 4. `deny_write` — operator value (`sandbox_policy.deny_write.unwrap_or_default()`)
    ///    only. The [`MANDATORY_DENY_WRITE`] guardrail list is always active
    ///    and enforced separately; `guardrail_exceptions` relaxes individual
    ///    guardrail entries per-entry (RFC 6996).
    #[must_use]
    pub fn from_profile(profile: &RiskProfileConfig, workspace: &Path) -> Self {
        let sp = &profile.sandbox_policy;
        // Single decision point for the Full-autonomy rule (RFC 6996): at
        // `Full`, `workspace_only` is always treated as `false`, matching the
        // pre-canonical app-layer behavior. Resolved here — before any
        // downstream consumer reads the raw field — so the OS-sandbox resolver
        // and the app-layer path guard cannot disagree.
        let effective_workspace_only =
            profile.workspace_only && profile.level != AutonomyLevel::Full;

        let deny_read = sp
            .deny_read
            .clone()
            .unwrap_or_else(|| profile.forbidden_paths.clone());
        let allow_read = sp
            .allow_read
            .clone()
            .unwrap_or_else(|| profile.allowed_roots.clone());
        let allow_write_is_explicit = sp.allow_write.is_some();
        // RFC 6996 compat conversion: only a NON-EMPTY legacy list derives a
        // write allowlist. Empty reads as absent (the legacy field cannot
        // distinguish omitted from an explicit `[]`).
        let allow_write_compat_allowlist = !allow_write_is_explicit
            && !effective_workspace_only
            && !profile.allowed_roots.is_empty();
        let allow_write = resolve_allow_write(sp, effective_workspace_only, profile, workspace);
        let deny_write = resolve_deny_write(sp);

        if !sp.guardrail_exceptions.is_empty() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "sandbox_policy: guardrail exception(s) active: default write-deny \
                 guardrails are relaxed for the named paths only; operator deny_write \
                 entries remain enforced"
            );
        }

        Self {
            deny_read,
            allow_read,
            allow_write,
            allow_write_is_explicit,
            allow_write_compat_allowlist,
            effective_workspace_only,
            deny_write,
            guardrail_exceptions: sp.guardrail_exceptions.clone(),
        }
    }

    /// Build effective inputs directly from a bare `SandboxPolicyConfig` with
    /// no profile and no legacy-compat fallback (`None` fields resolve to
    /// empty, not to some other struct's legacy fields). Used by
    /// `SandboxPolicy::default()`, which has no `RiskProfileConfig` to fall
    /// back to.
    fn from_bare_config(sp: &SandboxPolicyConfig, workspace: &Path) -> Self {
        let allow_write_is_explicit = sp.allow_write.is_some();
        let allow_write = if allow_write_is_explicit {
            sp.allow_write.clone().unwrap_or_default()
        } else {
            DEFAULT_ALLOW_WRITE
                .iter()
                .map(|s| (*s).to_string())
                .collect()
        };
        let _ = workspace;
        Self {
            deny_read: sp.deny_read.clone().unwrap_or_default(),
            allow_read: sp.allow_read.clone().unwrap_or_default(),
            allow_write,
            allow_write_is_explicit,
            // No profile → no legacy compat derivation possible.
            allow_write_compat_allowlist: false,
            effective_workspace_only: false,
            deny_write: resolve_deny_write(sp),
            guardrail_exceptions: sp.guardrail_exceptions.clone(),
        }
    }
}

/// Operator-supplied `deny_write` entries only. The [`MANDATORY_DENY_WRITE`]
/// guardrail list is NOT merged here: guardrails are always active (RFC 6996
/// removed the all-or-nothing switch) and are enforced at check time with
/// their own suffix-matching rules and per-entry exceptions, so the two
/// cannot be collapsed into one prefix-matched list.
fn resolve_deny_write(sp: &SandboxPolicyConfig) -> Vec<String> {
    sp.deny_write.clone().unwrap_or_default()
}

/// Resolve `allow_write` with presence-preserving canonical precedence,
/// `workspace_only` implicit-grant scoping, and `allowed_roots` compat fallback.
///
/// - `allow_write: Some(v)` wins outright — `v` exactly, no legacy merge, even
///   when `v` happens to be shaped like the old default — and regardless of
///   `workspace_only`. Per RFC 6996, `workspace_only` scopes only the
///   IMPLICIT workspace grant; an explicit canonical `allow_write` (including
///   an explicit `[]`) is authoritative for every path, workspace included.
/// - Effective `workspace_only = true` with `allow_write: None` → the
///   workspace root only (legacy behavior preserved).
/// - `allow_write: None` — [`DEFAULT_ALLOW_WRITE`] merged with legacy
///   `allowed_roots` (dedup, defaults first). The top-level `allowed_roots`
///   field historically granted extra write access on top of the default
///   workspace/temp roots, not a replacement of them. When that legacy list
///   is non-empty the merged set is a real write allowlist (see
///   `EffectiveSandboxInputs::allow_write_compat_allowlist`).
fn resolve_allow_write(
    sp: &SandboxPolicyConfig,
    effective_workspace_only: bool,
    profile: &RiskProfileConfig,
    workspace: &Path,
) -> Vec<String> {
    if let Some(v) = &sp.allow_write {
        return v.clone();
    }

    if effective_workspace_only {
        return vec![workspace.to_string_lossy().into_owned()];
    }

    let mut merged: Vec<String> = DEFAULT_ALLOW_WRITE
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    for root in &profile.allowed_roots {
        if !merged.contains(root) {
            merged.push(root.clone());
        }
    }
    merged
}

/// Resolved OS-level sandbox policy derived from a `RiskProfileConfig`.
///
/// `from_risk_profile` is the single authoritative code path that produces a
/// `SandboxPolicy`. All paths are resolved to absolute form using the workspace
/// root; `~` is expanded to the user home directory with a `directories::UserDirs`
/// fallback for environments where `HOME` is unset.
///
/// This lives in `zeroclaw-config` (not `zeroclaw-runtime`) so both the call site that
/// passes a resolved policy to `zeroclaw-runtime::security::detect::create_sandbox`
/// (which does not yet forward it to individual OS sandbox backends) and the app-layer
/// path guard (`SecurityPolicy::from_profiles`, same crate) derive from the identical
/// resolution — two enforcement layers reading two different resolutions of the same
/// config would otherwise be a dual-policy-surface gap.
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxPolicy {
    pub deny_read: Vec<PathBuf>,
    pub allow_read: Vec<PathBuf>,
    pub allow_write: Vec<PathBuf>,
    pub deny_write: Vec<PathBuf>,
    /// Per-entry guardrail exceptions (`guardrail_exceptions`), carried raw
    /// (suffix-matched at check time; workspace-agnostic, so no rebase
    /// re-resolution is needed). Empty means every
    /// [`MANDATORY_DENY_WRITE`] entry is enforced as-is.
    pub guardrail_exceptions: Vec<String>,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        let workspace = match std::env::current_dir() {
            Ok(dir) => dir,
            Err(_) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                    "SandboxPolicy::default(): current_dir() failed; \
                     falling back to filesystem root — guardrail paths may not match real user files"
                );
                PathBuf::from("/")
            }
        };
        let default_sp = SandboxPolicyConfig::default();
        let effective = EffectiveSandboxInputs::from_bare_config(&default_sp, &workspace);
        SandboxPolicy::from_effective(&effective, &workspace)
    }
}

impl SandboxPolicy {
    /// Resolve a `RiskProfileConfig` + workspace into a `SandboxPolicy`.
    ///
    /// Delegates precedence resolution to [`EffectiveSandboxInputs::from_profile`]
    /// (the single canonical-vs-legacy precedence function shared with
    /// `SecurityPolicy::from_profiles`), then path-resolves the result.
    #[must_use]
    pub fn from_risk_profile(profile: &RiskProfileConfig, workspace: &Path) -> Self {
        let effective = EffectiveSandboxInputs::from_profile(profile, workspace);
        Self::from_effective(&effective, workspace)
    }

    /// Path-resolve already-precedence-decided `EffectiveSandboxInputs`
    /// against `workspace`. Public so callers that need to re-resolve the
    /// same raw inputs against a different workspace (e.g. subagent
    /// workspace rebase) do not have to re-derive precedence.
    #[must_use]
    pub fn from_effective(effective: &EffectiveSandboxInputs, workspace: &Path) -> Self {
        // OS-sandbox view: the always-on [`MANDATORY_DENY_WRITE`] guardrails
        // are merged into the resolved `deny_write` list (an OS backend
        // denies by enumeration, so it needs the full set). The app-layer
        // path guard does NOT use this merged list — it enforces operator
        // entries and guardrails separately with RFC 6996 suffix matching and
        // per-entry exceptions, which a flat prefix-matched list cannot
        // express.
        let mut deny_write = resolve_paths(&effective.deny_write, workspace);
        for entry in MANDATORY_DENY_WRITE {
            let resolved = resolve_path(entry, workspace);
            if !deny_write.contains(&resolved) {
                deny_write.push(resolved);
            }
        }
        Self {
            deny_read: resolve_paths(&effective.deny_read, workspace),
            allow_read: resolve_paths(&effective.allow_read, workspace),
            allow_write: resolve_paths(&effective.allow_write, workspace),
            deny_write,
            guardrail_exceptions: effective.guardrail_exceptions.clone(),
        }
    }
}

// ── path utilities ───────────────────────────────────────────────────────────

/// Expand `~` and resolve relative paths against `workspace`.
pub(crate) fn resolve_paths(paths: &[String], workspace: &Path) -> Vec<PathBuf> {
    paths.iter().map(|p| resolve_path(p, workspace)).collect()
}

/// RFC 6996 guardrail/exception entry matching against a resolved target path.
///
/// Two entry shapes, mirroring the RFC's matching rules:
///
/// - **Anchored entries** (`~/...` or absolute): expanded through
///   [`resolve_path`] and prefix-matched against `resolved` — the entry names
///   one specific location, so the match is rooted there.
/// - **Relative entries** (bare names like `.bashrc`, or `path/segment`
///   forms like `.git/hooks/`): matched as a contiguous component run against
///   `resolved`'s components, so the entry matches at any depth under a
///   covered root (the caller gates on covered roots). An entry with a
///   trailing `/` is a DIRECTORY entry: it matches the directory itself and
///   every descendant. Without the trailing slash it is a FILE entry: it
///   matches only when the run is the FINAL components of `resolved`.
///
/// No glob syntax in this slice. Nested repositories need no special case:
/// a nested repo's own `.git/hooks/` is covered by the same run match as the
/// top-level one.
pub(crate) fn guardrail_entry_matches(entry: &str, resolved: &Path, workspace: &Path) -> bool {
    let is_directory_entry = entry.ends_with('/');
    let expanded = shellexpand::tilde(entry);
    let anchored = expanded.starts_with('/') || expanded.starts_with('~');

    if anchored {
        let resolved_entry = resolve_path(entry, workspace);
        return resolved.starts_with(&resolved_entry);
    }

    let entry_components: Vec<String> = entry
        .split('/')
        .filter(|c| !c.is_empty() && *c != ".")
        .map(|c| c.trim_end_matches('/').to_string())
        .collect();
    if entry_components.is_empty() {
        return false;
    }

    let target: Vec<String> = resolved
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if target.len() < entry_components.len() {
        return false;
    }

    if !is_directory_entry {
        // FILE entry: the run must be the final components of the target.
        return target[target.len() - entry_components.len()..] == entry_components[..];
    }

    // DIRECTORY entry: the run may appear at any component position — the
    // directory itself (run final) or any descendant (run followed by more).
    (0..=target.len() - entry_components.len())
        .any(|start| target[start..start + entry_components.len()] == entry_components[..])
}

pub(crate) fn resolve_path(p: &str, workspace: &Path) -> PathBuf {
    let expanded = shellexpand::tilde(p);
    let path_str = expanded.as_ref();

    // shellexpand::tilde leaves '~' intact when $HOME is unset.
    // Fall back to directories::UserDirs so daemons / cron jobs get correct expansion.
    let path: PathBuf = if path_str.starts_with('~') {
        if let Some(user_dirs) = directories::UserDirs::new() {
            let home = user_dirs.home_dir();
            if let Some(rest) = path_str.strip_prefix("~/") {
                home.join(rest)
            } else {
                home.to_path_buf()
            }
        } else {
            PathBuf::from(path_str)
        }
    } else {
        PathBuf::from(path_str)
    };

    if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::RiskProfileConfig;
    use std::path::Path;

    fn ws() -> &'static Path {
        Path::new("/workspace")
    }

    #[test]
    fn sandbox_policy_default_produces_absolute_paths() {
        let policy = SandboxPolicy::default();
        for p in policy.deny_write.iter().chain(policy.allow_write.iter()) {
            assert!(
                p.is_absolute(),
                "default path not absolute: {}",
                p.display()
            );
        }
    }

    #[test]
    fn default_profile_resolves_without_panic() {
        let policy = SandboxPolicy::from_risk_profile(&RiskProfileConfig::default(), ws());
        assert!(
            !policy.deny_write.is_empty(),
            "guardrail list must always be present (RFC 6996: no disable switch)"
        );
        assert!(policy.guardrail_exceptions.is_empty());
    }

    #[test]
    fn forbidden_paths_compat_maps_to_deny_read() {
        let mut profile = RiskProfileConfig::default();
        profile.sandbox_policy.deny_read = None;
        profile.forbidden_paths = vec!["/secret".to_string()];
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert!(policy.deny_read.contains(&PathBuf::from("/secret")));
    }

    #[test]
    fn sandbox_policy_deny_read_takes_precedence_over_forbidden_paths() {
        let mut profile = RiskProfileConfig::default();
        profile.sandbox_policy.deny_read = Some(vec!["/explicit".to_string()]);
        profile.forbidden_paths = vec!["/should_be_ignored".to_string()];
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert!(policy.deny_read.contains(&PathBuf::from("/explicit")));
        assert!(
            !policy
                .deny_read
                .contains(&PathBuf::from("/should_be_ignored"))
        );
    }

    #[test]
    fn explicit_empty_deny_read_clears_legacy_forbidden_paths() {
        let mut profile = RiskProfileConfig::default();
        profile.sandbox_policy.deny_read = Some(vec![]);
        profile.forbidden_paths = vec!["~/.ssh".to_string()];
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert!(
            policy.deny_read.is_empty(),
            "explicit empty deny_read must clear legacy forbidden_paths fallback, got: {:?}",
            policy.deny_read
        );
    }

    #[test]
    fn explicit_empty_allow_read_clears_legacy_allowed_roots() {
        let mut profile = RiskProfileConfig::default();
        profile.sandbox_policy.allow_read = Some(vec![]);
        profile.allowed_roots = vec!["/legacy_read".to_string()];
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert!(
            policy.allow_read.is_empty(),
            "explicit empty allow_read must clear legacy allowed_roots fallback, got: {:?}",
            policy.allow_read
        );
    }

    #[test]
    fn allowed_roots_compat_maps_to_allow_read_and_allow_write_when_omitted() {
        let mut profile = RiskProfileConfig {
            workspace_only: false,
            allowed_roots: vec!["/extra".to_string()],
            ..RiskProfileConfig::default()
        };
        profile.sandbox_policy.allow_read = None;
        // allow_write omitted — allowed_roots compat applies to both fields
        profile.sandbox_policy.allow_write = None;
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert!(policy.allow_read.contains(&PathBuf::from("/extra")));
        assert!(policy.allow_write.contains(&PathBuf::from("/extra")));
    }

    #[test]
    fn allowed_roots_compat_merges_onto_default_write_roots_not_replaces() {
        // allowed_roots must be merged onto the default write roots
        // (workspace + /tmp), not replace them outright.
        let profile = RiskProfileConfig {
            workspace_only: false,
            allowed_roots: vec!["/extra".to_string()],
            ..RiskProfileConfig::default()
        };
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        for default_entry in DEFAULT_ALLOW_WRITE {
            let resolved_default = resolve_path(default_entry, ws());
            assert!(
                policy.allow_write.contains(&resolved_default),
                "default write root {default_entry} must survive the allowed_roots compat merge"
            );
        }
        assert!(policy.allow_write.contains(&PathBuf::from("/extra")));
    }

    #[test]
    fn allowed_roots_does_not_override_explicit_allow_write() {
        let mut profile = RiskProfileConfig {
            workspace_only: false,
            allowed_roots: vec!["/extra".to_string()],
            ..RiskProfileConfig::default()
        };
        profile.sandbox_policy.allow_write = Some(vec!["/custom".to_string()]);
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        // explicit allow_write wins; allowed_roots is not merged in
        assert_eq!(policy.allow_write, vec![PathBuf::from("/custom")]);
    }

    #[test]
    fn explicit_default_shaped_allow_write_blocks_legacy_merge() {
        // allow_write explicitly set to the old default shape must NOT trigger
        // the allowed_roots compat merge — presence, not shape, decides.
        let mut profile = RiskProfileConfig {
            workspace_only: false,
            allowed_roots: vec!["/legacy".to_string()],
            ..RiskProfileConfig::default()
        };
        profile.sandbox_policy.allow_write = Some(vec![".".to_string(), "/tmp".to_string()]);
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert!(
            !policy.allow_write.contains(&PathBuf::from("/legacy")),
            "explicit allow_write must block the allowed_roots legacy merge, got: {:?}",
            policy.allow_write
        );
    }

    #[test]
    fn explicit_allow_write_is_authoritative_over_workspace_only() {
        // RFC 6996: workspace_only scopes only the IMPLICIT workspace grant.
        // An explicit canonical allow_write replaces it outright — the
        // workspace root is writable only by being named in the list.
        let mut profile = RiskProfileConfig {
            workspace_only: true,
            ..RiskProfileConfig::default()
        };
        profile.sandbox_policy.allow_write = Some(vec!["/custom".to_string()]);
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert_eq!(policy.allow_write, vec![PathBuf::from("/custom")]);
    }

    #[test]
    fn explicit_empty_allow_write_wins_over_workspace_only() {
        // Explicit [] is a real (empty) allowlist, not "absent".
        let mut profile = RiskProfileConfig {
            workspace_only: true,
            ..RiskProfileConfig::default()
        };
        profile.sandbox_policy.allow_write = Some(vec![]);
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert!(
            policy.allow_write.is_empty(),
            "explicit empty allow_write must win over workspace_only, got {:?}",
            policy.allow_write
        );
    }

    #[test]
    fn workspace_only_scopes_implicit_grant_when_allow_write_omitted() {
        let profile = RiskProfileConfig {
            workspace_only: true,
            ..RiskProfileConfig::default()
        };
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert_eq!(policy.allow_write, vec![ws().to_path_buf()]);
    }

    #[test]
    fn workspace_only_false_uses_custom_allow_write() {
        let mut profile = RiskProfileConfig {
            workspace_only: false,
            ..RiskProfileConfig::default()
        };
        profile.sandbox_policy.allow_write = Some(vec!["/custom".to_string()]);
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert_eq!(policy.allow_write, vec![PathBuf::from("/custom")]);
    }

    #[test]
    fn guardrails_merge_onto_operator_deny_write_deduped() {
        // RFC 6996: the guardrail list is always active — there is no switch.
        // The OS-sandbox view (`SandboxPolicy`) merges operator entries and
        // the defaults into one deny list, deduplicated.
        let mut profile = RiskProfileConfig::default();
        let mut extended: Vec<String> = MANDATORY_DENY_WRITE
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        extended.push("/extra_blocked".to_string());
        profile.sandbox_policy.deny_write = Some(extended);
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        for entry in MANDATORY_DENY_WRITE {
            assert!(
                policy
                    .deny_write
                    .iter()
                    .any(|p| p.ends_with(entry.trim_end_matches('/'))),
                "missing guardrail: {entry}"
            );
        }
        assert!(
            policy
                .deny_write
                .iter()
                .any(|p| p.ends_with("extra_blocked"))
        );
    }

    #[test]
    fn operator_deny_write_resolves_without_guardrails_in_effective_inputs() {
        // The app-layer view (EffectiveSandboxInputs) keeps operator entries
        // ONLY — guardrails are enforced separately with suffix matching and
        // per-entry exceptions, so they must not be collapsed into the
        // prefix-matched operator list.
        let mut profile = RiskProfileConfig::default();
        profile.sandbox_policy.deny_write = Some(vec!["/only_this".to_string()]);
        let effective = EffectiveSandboxInputs::from_profile(&profile, ws());
        assert_eq!(effective.deny_write, vec!["/only_this".to_string()]);
    }

    #[test]
    fn relative_paths_resolved_against_workspace() {
        let mut profile = RiskProfileConfig::default();
        profile.sandbox_policy.deny_read = Some(vec!["relative/dir".to_string()]);
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert!(policy.deny_read.contains(&ws().join("relative/dir")));
    }

    #[test]
    fn guardrail_directory_entry_covers_directory_and_descendants() {
        // RFC 6996 + closing record: a trailing-`/` entry covers the
        // directory itself and every descendant, at any depth under covered
        // roots (nested repos included).
        assert!(guardrail_entry_matches(
            ".vscode/",
            Path::new("/w/.vscode"),
            ws()
        ));
        assert!(guardrail_entry_matches(
            ".vscode/",
            Path::new("/w/.vscode/settings.json"),
            ws()
        ));
        assert!(
            guardrail_entry_matches(
                ".git/hooks/",
                Path::new("/w/sub/repo/.git/hooks/pre-commit"),
                ws()
            ),
            "a nested repo's own .git/hooks must be covered by the same suffix rule"
        );
        // Component boundaries are exact — a longer name is not a match.
        assert!(!guardrail_entry_matches(
            ".vscode/",
            Path::new("/w/.vscode-extended/settings.json"),
            ws()
        ));
    }

    #[test]
    fn guardrail_file_entry_matches_final_component_only() {
        assert!(
            guardrail_entry_matches(".bashrc", Path::new("/w/sub/.bashrc"), ws()),
            "bare entry matches at any depth"
        );
        assert!(
            !guardrail_entry_matches(".bashrc", Path::new("/w/sub/.bashrc/inner.sh"), ws()),
            "a file entry does not cover a directory that happens to share the name"
        );
        assert!(!guardrail_entry_matches(
            ".bashrc",
            Path::new("/w/.bashrc-backup"),
            ws()
        ));
    }

    #[test]
    fn guardrail_anchored_entry_prefix_matches() {
        let home = resolve_path("~/.bashrc", ws());
        assert!(guardrail_entry_matches("~/.bashrc", &home, ws()));
        assert!(
            !guardrail_entry_matches("~/.bashrc", &ws().join(".bashrc"), ws()),
            "the anchored home entry does not cover the workspace copy"
        );
    }

    #[test]
    fn guardrail_path_segment_entry_matches_relative_suffix() {
        assert!(guardrail_entry_matches(
            "shared/skills/",
            Path::new("/install/shared/skills/bundle/skill.md"),
            ws()
        ));
        assert!(guardrail_entry_matches(
            "shared/skills/",
            Path::new("/install/shared/skills"),
            ws()
        ));
        assert!(!guardrail_entry_matches(
            "shared/skills/",
            Path::new("/install/shared/other/skills"),
            ws()
        ));
    }

    #[test]
    fn tilde_expanded_in_deny_read() {
        let mut profile = RiskProfileConfig::default();
        profile.sandbox_policy.deny_read = Some(vec!["~/.ssh".to_string()]);
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert!(policy.deny_read.iter().all(|p| p.is_absolute()));
    }

    #[test]
    fn old_style_and_new_style_produce_equivalent_policy() {
        // Old-style: forbidden_paths / allowed_roots, sandbox_policy omitted.
        let old_style = RiskProfileConfig {
            forbidden_paths: vec!["/secret".to_string()],
            allowed_roots: vec!["/extra".to_string()],
            workspace_only: false,
            sandbox_policy: SandboxPolicyConfig::default(),
            ..RiskProfileConfig::default()
        };

        // New-style: same semantics via sandbox_policy directly.
        let mut new_style = RiskProfileConfig {
            forbidden_paths: vec![],
            allowed_roots: vec![],
            workspace_only: false,
            ..RiskProfileConfig::default()
        };
        new_style.sandbox_policy.deny_read = Some(vec!["/secret".to_string()]);
        new_style.sandbox_policy.allow_read = Some(vec!["/extra".to_string()]);
        // allow_write compat MERGES allowed_roots onto the default write roots (see
        // resolve_allow_write); an explicit allow_write must include the same defaults to
        // reach the same resolved policy as the old-style / compat path.
        let mut merged_write: Vec<String> = DEFAULT_ALLOW_WRITE
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        merged_write.push("/extra".to_string());
        new_style.sandbox_policy.allow_write = Some(merged_write);

        let old_policy = SandboxPolicy::from_risk_profile(&old_style, ws());
        let new_policy = SandboxPolicy::from_risk_profile(&new_style, ws());

        assert_eq!(old_policy.deny_read, new_policy.deny_read);
        assert_eq!(old_policy.allow_read, new_policy.allow_read);
        assert_eq!(old_policy.allow_write, new_policy.allow_write);
        assert_eq!(old_policy.deny_write, new_policy.deny_write);
        assert_eq!(
            old_policy.guardrail_exceptions,
            new_policy.guardrail_exceptions
        );

        // Composition case: workspace_only=true (implicit grant) is expressed
        // old-style by the flag alone and new-style by naming the workspace in
        // an explicit allow_write — the resolved policies must match, because
        // an explicit canonical list is authoritative over the implicit grant.
        let old_style_ws_only = RiskProfileConfig {
            workspace_only: true,
            ..RiskProfileConfig::default()
        };
        let mut new_style_ws_only = RiskProfileConfig {
            workspace_only: true,
            ..RiskProfileConfig::default()
        };
        new_style_ws_only.sandbox_policy.allow_write =
            Some(vec![ws().to_string_lossy().into_owned()]);

        let old_ws = SandboxPolicy::from_risk_profile(&old_style_ws_only, ws());
        let new_ws = SandboxPolicy::from_risk_profile(&new_style_ws_only, ws());
        assert_eq!(old_ws.allow_write, new_ws.allow_write);
    }
}
