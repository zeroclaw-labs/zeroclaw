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
    /// `sandbox_policy.deny_write.unwrap_or_default()` plus the
    /// [`MANDATORY_DENY_WRITE`] guardrail list when
    /// `mandatory_deny_write_enabled` is true.
    pub deny_write: Vec<String>,
    pub mandatory_deny_write_enabled: bool,
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
    pub allow_unix_sockets: Vec<String>,
    pub bubblewrap_args: Vec<String>,
}

impl EffectiveSandboxInputs {
    /// Resolve canonical-vs-legacy precedence for `profile` against `workspace`.
    ///
    /// Precedence (canonical `sandbox_policy` field wins whenever present):
    /// 1. `deny_read` — `sandbox_policy.deny_read` if `Some` (including
    ///    explicit `Some(vec![])`), else legacy `forbidden_paths`.
    /// 2. `allow_read` — `sandbox_policy.allow_read` if `Some`, else legacy
    ///    `allowed_roots`.
    /// 3. `allow_write` — `workspace_only = true` always wins (overrides any
    ///    `allow_write`, `Some` or `None`); otherwise `sandbox_policy.allow_write`
    ///    if `Some` (exactly, no legacy merge — even if it happens to equal the
    ///    old default shape); otherwise (`None`) [`DEFAULT_ALLOW_WRITE`] merged
    ///    with legacy `allowed_roots`.
    /// 4. `deny_write` — operator value (`sandbox_policy.deny_write.unwrap_or_default()`)
    ///    plus, when `mandatory_deny_write_enabled`, any [`MANDATORY_DENY_WRITE`]
    ///    entries missing from it.
    #[must_use]
    pub fn from_profile(profile: &RiskProfileConfig, workspace: &Path) -> Self {
        let sp = &profile.sandbox_policy;

        let deny_read = sp
            .deny_read
            .clone()
            .unwrap_or_else(|| profile.forbidden_paths.clone());
        let allow_read = sp
            .allow_read
            .clone()
            .unwrap_or_else(|| profile.allowed_roots.clone());
        let allow_write_is_explicit = sp.allow_write.is_some();
        let allow_write = resolve_allow_write(sp, profile, workspace);
        let deny_write = resolve_deny_write(sp);

        Self {
            deny_read,
            allow_read,
            allow_write,
            allow_write_is_explicit,
            deny_write,
            mandatory_deny_write_enabled: sp.mandatory_deny_write_enabled,
            allowed_domains: sp.allowed_domains.clone(),
            denied_domains: sp.denied_domains.clone(),
            allow_unix_sockets: sp.allow_unix_sockets.clone(),
            bubblewrap_args: sp.bubblewrap_args.clone(),
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
            deny_write: resolve_deny_write(sp),
            mandatory_deny_write_enabled: sp.mandatory_deny_write_enabled,
            allowed_domains: sp.allowed_domains.clone(),
            denied_domains: sp.denied_domains.clone(),
            allow_unix_sockets: sp.allow_unix_sockets.clone(),
            bubblewrap_args: sp.bubblewrap_args.clone(),
        }
    }
}

fn resolve_deny_write(sp: &SandboxPolicyConfig) -> Vec<String> {
    let mut deny_write = sp.deny_write.clone().unwrap_or_default();
    if sp.mandatory_deny_write_enabled {
        // Deduplication is string-based (pre-resolution). An operator entry like
        // "/home/user/.bashrc" will not prevent the default ".bashrc" entry from
        // also being added; both resolve independently. This is intentional — semantic
        // path equivalence checking is not performed here.
        let missing: Vec<String> = MANDATORY_DENY_WRITE
            .iter()
            .filter(|e| !deny_write.iter().any(|d| d == *e))
            .map(|e| (*e).to_string())
            .collect();
        deny_write.extend(missing);
    } else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            "sandbox_policy: mandatory_deny_write_enabled=false; \
             default write-deny guardrails (shell configs, git hooks, .env, etc.) \
             are not enforced for this profile"
        );
    }
    deny_write
}

/// Resolve `allow_write` with `workspace_only` priority, presence-preserving
/// canonical precedence, and `allowed_roots` compat fallback.
///
/// - `workspace_only = true` always wins and overrides any concurrently set
///   `allow_write` (`Some` or `None`).
/// - `allow_write: Some(v)` wins outright — `v` exactly, no legacy merge, even
///   when `v` happens to be shaped like the old default.
/// - `allow_write: None` — [`DEFAULT_ALLOW_WRITE`] merged with legacy
///   `allowed_roots` (dedup, defaults first). The top-level `allowed_roots`
///   field historically granted extra write access on top of the default
///   workspace/temp roots, not a replacement of them.
fn resolve_allow_write(
    sp: &SandboxPolicyConfig,
    profile: &RiskProfileConfig,
    workspace: &Path,
) -> Vec<String> {
    if profile.workspace_only {
        if sp.allow_write.is_some() {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "sandbox_policy: workspace_only=true overrides custom allow_write; \
                 allow_write will be restricted to the workspace root"
            );
        }
        return vec![workspace.to_string_lossy().into_owned()];
    }

    match &sp.allow_write {
        Some(v) => v.clone(),
        None => {
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
    }
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
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
    pub allow_unix_sockets: Vec<PathBuf>,
    pub bubblewrap_args: Vec<String>,
    pub mandatory_deny_write_enabled: bool,
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
        Self {
            deny_read: resolve_paths(&effective.deny_read, workspace),
            allow_read: resolve_paths(&effective.allow_read, workspace),
            allow_write: resolve_paths(&effective.allow_write, workspace),
            deny_write: resolve_paths(&effective.deny_write, workspace),
            allowed_domains: effective.allowed_domains.clone(),
            denied_domains: effective.denied_domains.clone(),
            allow_unix_sockets: resolve_paths(&effective.allow_unix_sockets, workspace),
            bubblewrap_args: effective.bubblewrap_args.clone(),
            mandatory_deny_write_enabled: effective.mandatory_deny_write_enabled,
        }
    }
}

// ── path utilities ───────────────────────────────────────────────────────────

/// Expand `~` and resolve relative paths against `workspace`.
fn resolve_paths(paths: &[String], workspace: &Path) -> Vec<PathBuf> {
    paths.iter().map(|p| resolve_path(p, workspace)).collect()
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
        assert!(policy.mandatory_deny_write_enabled);
        assert!(
            !policy.deny_write.is_empty(),
            "guardrail list must be present"
        );
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
    fn workspace_only_always_overrides_allow_write() {
        let mut profile = RiskProfileConfig {
            workspace_only: true,
            ..RiskProfileConfig::default()
        };
        // Set a custom allow_write — workspace_only must still win
        profile.sandbox_policy.allow_write = Some(vec!["/should_be_overridden".to_string()]);
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
    fn mandatory_deny_write_merges_only_missing_entries() {
        let mut profile = RiskProfileConfig::default();
        let mut extended: Vec<String> = MANDATORY_DENY_WRITE
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        extended.push("/extra_blocked".to_string());
        profile.sandbox_policy.deny_write = Some(extended);
        profile.sandbox_policy.mandatory_deny_write_enabled = true;
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        for entry in MANDATORY_DENY_WRITE {
            assert!(
                policy.deny_write.iter().any(|p| p.ends_with(entry)),
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
    fn mandatory_deny_write_disabled_skips_guardrail_merge() {
        let mut profile = RiskProfileConfig::default();
        profile.sandbox_policy.deny_write = Some(vec!["/only_this".to_string()]);
        profile.sandbox_policy.mandatory_deny_write_enabled = false;
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert_eq!(policy.deny_write, vec![PathBuf::from("/only_this")]);
    }

    #[test]
    fn relative_paths_resolved_against_workspace() {
        let mut profile = RiskProfileConfig::default();
        profile.sandbox_policy.deny_read = Some(vec!["relative/dir".to_string()]);
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert!(policy.deny_read.contains(&ws().join("relative/dir")));
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
            old_policy.mandatory_deny_write_enabled,
            new_policy.mandatory_deny_write_enabled
        );
    }
}
