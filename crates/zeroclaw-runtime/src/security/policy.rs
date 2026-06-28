pub use zeroclaw_config::policy::*;

use std::path::{Path, PathBuf};
use zeroclaw_config::schema::{RiskProfileConfig, SandboxPolicyConfig};

/// Resolved OS-level sandbox policy derived from a `RiskProfileConfig`.
///
/// `from_risk_profile` is the single authoritative code path that produces a
/// `SandboxPolicy`. All paths are resolved to absolute form using the workspace
/// root; `~` is expanded to the user home directory with a `directories::UserDirs`
/// fallback for environments where `HOME` is unset.
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
        Self::resolve(&default_sp, &workspace, &default_sp)
    }
}

impl SandboxPolicy {
    /// Resolve a `RiskProfileConfig` + workspace into a `SandboxPolicy`.
    ///
    /// Resolution order (`sandbox_policy` is canonical):
    /// 1. `deny_read` — `sandbox_policy.deny_read`; falls back to `forbidden_paths`.
    /// 2. `allow_read` — `sandbox_policy.allow_read`; falls back to `allowed_roots`.
    /// 3. `allow_write` — `workspace_only = true` always wins (overrides custom
    ///    `allow_write`); otherwise `sandbox_policy.allow_write` if non-default,
    ///    then `allowed_roots` compat, then schema default.
    /// 4. `deny_write` — when `mandatory_deny_write_enabled`, the default guardrail
    ///    entries (shell configs, git hooks, `.env`, `.mcp.json`, etc.) are merged
    ///    in for any entries absent from the operator-supplied list.
    pub fn from_risk_profile(profile: &RiskProfileConfig, workspace: &Path) -> Self {
        let sp = &profile.sandbox_policy;
        let default_sp = SandboxPolicyConfig::default();

        let deny_read = resolve_deny_read(sp, profile);
        let allow_read = resolve_allow_read(sp, profile);
        let allow_write = resolve_allow_write(sp, profile, workspace, &default_sp);

        let resolved_sp = SandboxPolicyConfig {
            deny_read,
            allow_read,
            allow_write,
            deny_write: sp.deny_write.clone(),
            allowed_domains: sp.allowed_domains.clone(),
            denied_domains: sp.denied_domains.clone(),
            allow_unix_sockets: sp.allow_unix_sockets.clone(),
            bubblewrap_args: sp.bubblewrap_args.clone(),
            mandatory_deny_write_enabled: sp.mandatory_deny_write_enabled,
        };

        Self::resolve(&resolved_sp, workspace, &default_sp)
    }

    /// Core resolver: path-expand all fields of a `SandboxPolicyConfig` against
    /// `workspace` and merge the mandatory deny-write guardrail list when enabled.
    ///
    /// `default_sp` is passed in so callers can reuse an already-constructed default
    /// rather than allocating a second one inside this function.
    fn resolve(
        sp: &SandboxPolicyConfig,
        workspace: &Path,
        default_sp: &SandboxPolicyConfig,
    ) -> Self {
        let mut deny_write = sp.deny_write.clone();
        if sp.mandatory_deny_write_enabled {
            // Deduplication is string-based (pre-resolution). An operator entry like
            // "/home/user/.bashrc" will not prevent the default ".bashrc" entry from
            // also being added; both resolve independently. This is intentional — semantic
            // path equivalence checking is not performed here.
            let missing: Vec<String> = default_sp
                .deny_write
                .iter()
                .filter(|e| !deny_write.contains(e))
                .cloned()
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

        Self {
            deny_read: resolve_paths(&sp.deny_read, workspace),
            allow_read: resolve_paths(&sp.allow_read, workspace),
            allow_write: resolve_paths(&sp.allow_write, workspace),
            deny_write: resolve_paths(&deny_write, workspace),
            allowed_domains: sp.allowed_domains.clone(),
            denied_domains: sp.denied_domains.clone(),
            allow_unix_sockets: resolve_paths(&sp.allow_unix_sockets, workspace),
            bubblewrap_args: sp.bubblewrap_args.clone(),
            mandatory_deny_write_enabled: sp.mandatory_deny_write_enabled,
        }
    }
}

// ── per-field compat resolution helpers ─────────────────────────────────────

fn resolve_deny_read(sp: &SandboxPolicyConfig, profile: &RiskProfileConfig) -> Vec<String> {
    if sp.deny_read.is_empty() {
        profile.forbidden_paths.clone()
    } else {
        sp.deny_read.clone()
    }
}

fn resolve_allow_read(sp: &SandboxPolicyConfig, profile: &RiskProfileConfig) -> Vec<String> {
    if sp.allow_read.is_empty() {
        profile.allowed_roots.clone()
    } else {
        sp.allow_read.clone()
    }
}

/// Resolve `allow_write` with `workspace_only` priority and `allowed_roots` compat fallback.
///
/// - `workspace_only = true` always wins and overrides any concurrently set `allow_write`.
/// - If `allow_write` is at its schema default (order-independent), `allowed_roots` is used
///   as a compat fallback (the top-level `allowed_roots` field historically granted both
///   read and write access to those paths). If `allow_write` was explicitly customised,
///   `allowed_roots` is NOT merged in — the explicit value takes precedence.
fn resolve_allow_write(
    sp: &SandboxPolicyConfig,
    profile: &RiskProfileConfig,
    workspace: &Path,
    default_sp: &SandboxPolicyConfig,
) -> Vec<String> {
    // Order-independent comparison: [".", "/tmp"] and ["/tmp", "."] are both "at default".
    let at_default_write = {
        let mut actual = sp.allow_write.clone();
        let mut expected = default_sp.allow_write.clone();
        actual.sort();
        expected.sort();
        actual == expected
    };

    if profile.workspace_only {
        if !at_default_write {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
                "sandbox_policy: workspace_only=true overrides custom allow_write; \
                 allow_write will be restricted to the workspace root"
            );
        }
        vec![workspace.to_string_lossy().into_owned()]
    } else if !at_default_write {
        // Explicit sandbox_policy.allow_write: allowed_roots is NOT merged in.
        sp.allow_write.clone()
    } else if !profile.allowed_roots.is_empty() {
        // allowed_roots compat fallback: only used when allow_write is at its default value.
        profile.allowed_roots.clone()
    } else {
        sp.allow_write.clone()
    }
}

// ── path utilities ───────────────────────────────────────────────────────────

/// Expand `~` and resolve relative paths against `workspace`.
fn resolve_paths(paths: &[String], workspace: &Path) -> Vec<PathBuf> {
    paths.iter().map(|p| resolve_path(p, workspace)).collect()
}

fn resolve_path(p: &str, workspace: &Path) -> PathBuf {
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
    use std::path::Path;
    use zeroclaw_config::schema::RiskProfileConfig;

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
        profile.sandbox_policy.deny_read = vec![];
        profile.forbidden_paths = vec!["/secret".to_string()];
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert!(policy.deny_read.contains(&PathBuf::from("/secret")));
    }

    #[test]
    fn sandbox_policy_deny_read_takes_precedence_over_forbidden_paths() {
        let mut profile = RiskProfileConfig::default();
        profile.sandbox_policy.deny_read = vec!["/explicit".to_string()];
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
    fn allowed_roots_compat_maps_to_allow_read_and_allow_write_when_at_default() {
        let mut profile = RiskProfileConfig::default();
        profile.workspace_only = false;
        profile.sandbox_policy.allow_read = vec![];
        profile.allowed_roots = vec!["/extra".to_string()];
        // allow_write at default — allowed_roots compat applies to both fields
        profile.sandbox_policy.allow_write = SandboxPolicyConfig::default().allow_write;
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert!(policy.allow_read.contains(&PathBuf::from("/extra")));
        assert!(policy.allow_write.contains(&PathBuf::from("/extra")));
    }

    #[test]
    fn allowed_roots_does_not_override_explicit_allow_write() {
        let mut profile = RiskProfileConfig::default();
        profile.workspace_only = false;
        profile.allowed_roots = vec!["/extra".to_string()];
        profile.sandbox_policy.allow_write = vec!["/custom".to_string()];
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        // explicit allow_write wins; allowed_roots is not merged in
        assert_eq!(policy.allow_write, vec![PathBuf::from("/custom")]);
    }

    #[test]
    fn workspace_only_always_overrides_allow_write() {
        let mut profile = RiskProfileConfig::default();
        profile.workspace_only = true;
        // Set a custom allow_write — workspace_only must still win
        profile.sandbox_policy.allow_write = vec!["/should_be_overridden".to_string()];
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert_eq!(policy.allow_write, vec![ws().to_path_buf()]);
    }

    #[test]
    fn workspace_only_false_uses_custom_allow_write() {
        let mut profile = RiskProfileConfig::default();
        profile.workspace_only = false;
        profile.sandbox_policy.allow_write = vec!["/custom".to_string()];
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert_eq!(policy.allow_write, vec![PathBuf::from("/custom")]);
    }

    #[test]
    fn at_default_write_comparison_is_order_independent() {
        let mut profile = RiskProfileConfig::default();
        profile.workspace_only = false;
        profile.allowed_roots = vec!["/via_compat".to_string()];
        // Same elements as default [".", "/tmp"] but reversed order
        profile.sandbox_policy.allow_write = vec!["/tmp".to_string(), ".".to_string()];
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        // Still "at default" so allowed_roots compat should apply
        assert!(policy.allow_write.contains(&PathBuf::from("/via_compat")));
    }

    #[test]
    fn mandatory_deny_write_merges_only_missing_entries() {
        let mut profile = RiskProfileConfig::default();
        let default_deny = SandboxPolicyConfig::default().deny_write;
        let mut extended = default_deny.clone();
        extended.push("/extra_blocked".to_string());
        profile.sandbox_policy.deny_write = extended;
        profile.sandbox_policy.mandatory_deny_write_enabled = true;
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        for entry in &default_deny {
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
        profile.sandbox_policy.deny_write = vec!["/only_this".to_string()];
        profile.sandbox_policy.mandatory_deny_write_enabled = false;
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert_eq!(policy.deny_write, vec![PathBuf::from("/only_this")]);
    }

    #[test]
    fn relative_paths_resolved_against_workspace() {
        let mut profile = RiskProfileConfig::default();
        profile.sandbox_policy.deny_read = vec!["relative/dir".to_string()];
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert!(policy.deny_read.contains(&ws().join("relative/dir")));
    }

    #[test]
    fn tilde_expanded_in_deny_read() {
        let mut profile = RiskProfileConfig::default();
        profile.sandbox_policy.deny_read = vec!["~/.ssh".to_string()];
        let policy = SandboxPolicy::from_risk_profile(&profile, ws());
        assert!(policy.deny_read.iter().all(|p| p.is_absolute()));
    }

    #[test]
    fn old_style_and_new_style_produce_equivalent_policy() {
        // Old-style: forbidden_paths / allowed_roots, sandbox_policy at default.
        let mut old_style = RiskProfileConfig::default();
        old_style.forbidden_paths = vec!["/secret".to_string()];
        old_style.allowed_roots = vec!["/extra".to_string()];
        old_style.workspace_only = false;
        old_style.sandbox_policy = SandboxPolicyConfig::default();

        // New-style: same semantics via sandbox_policy directly.
        let mut new_style = RiskProfileConfig::default();
        new_style.forbidden_paths = vec![];
        new_style.allowed_roots = vec![];
        new_style.workspace_only = false;
        new_style.sandbox_policy.deny_read = vec!["/secret".to_string()];
        new_style.sandbox_policy.allow_read = vec!["/extra".to_string()];
        new_style.sandbox_policy.allow_write = vec!["/extra".to_string()];

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
