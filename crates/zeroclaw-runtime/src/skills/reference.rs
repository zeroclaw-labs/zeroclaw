//! Skill identity + the disambiguation rule that every surface goes through.
//!
//! `SkillRef` is the canonical `(bundle, name)` pair. Fields are private; the
//! only public constructor is [`resolve`], which enforces the rule "bundle
//! optional when name is globally unique across configured bundles". CLI flag
//! parsing, gateway URL parsing, TUI selection — all must call `resolve` to
//! produce a `SkillRef`. If a future caller hand-builds one, they cannot:
//! the constructor is module-private.

use std::fmt;

use zeroclaw_config::schema::Config;

/// Canonical `(bundle-alias, skill-name)` identity for a skill on disk.
///
/// Construct via [`resolve`]; never by literal field assignment.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SkillRef {
    bundle: String,
    name: String,
}

impl SkillRef {
    pub(super) fn new_unchecked(bundle: String, name: String) -> Self {
        Self { bundle, name }
    }

    pub fn bundle(&self) -> &str {
        &self.bundle
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for SkillRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.bundle, self.name)
    }
}

/// Errors surfaced by [`resolve`] when a `(name, bundle?)` pair cannot be
/// turned into a unique `SkillRef`.
#[derive(Debug, thiserror::Error)]
pub enum SkillRefError {
    #[error("no skill bundles are configured; create one before adding skills")]
    NoBundles,

    #[error("skill bundle '{0}' is not configured")]
    UnknownBundle(String),

    #[error("skill '{name}' was not found in any configured bundle")]
    UnknownSkill { name: String },

    #[error(
        "skill name '{name}' is ambiguous across bundles {candidates:?}; pass --bundle to disambiguate"
    )]
    AmbiguousName {
        name: String,
        candidates: Vec<String>,
    },
}

/// Resolve a `(name, bundle?)` pair into a canonical [`SkillRef`].
///
/// Rule: `bundle` is optional iff `name` exists in exactly one configured
/// bundle's directory. Otherwise the caller must qualify.
///
/// Filesystem state (which directories actually contain a `SKILL.md`) is
/// checked by [`crate::skills::service::SkillsService::list_skills`]; this
/// function operates over `Config` alone and is filesystem-free, so it can
/// be unit-tested in isolation.
pub fn resolve(
    config: &Config,
    name: &str,
    bundle: Option<&str>,
) -> Result<SkillRef, SkillRefError> {
    if config.skill_bundles.is_empty() {
        return Err(SkillRefError::NoBundles);
    }

    if let Some(bundle_alias) = bundle {
        if !config.skill_bundles.contains_key(bundle_alias) {
            return Err(SkillRefError::UnknownBundle(bundle_alias.to_string()));
        }
        return Ok(SkillRef::new_unchecked(
            bundle_alias.to_string(),
            name.to_string(),
        ));
    }

    if config.skill_bundles.len() == 1 {
        let bundle_alias = config.skill_bundles.keys().next().unwrap().clone();
        return Ok(SkillRef::new_unchecked(bundle_alias, name.to_string()));
    }

    Err(SkillRefError::AmbiguousName {
        name: name.to_string(),
        candidates: config.skill_bundles.keys().cloned().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::SkillBundleConfig;

    fn cfg_with_bundles(aliases: &[&str]) -> Config {
        let mut cfg = Config::default();
        for alias in aliases {
            cfg.skill_bundles
                .insert((*alias).to_string(), SkillBundleConfig::default());
        }
        cfg
    }

    #[test]
    fn errors_when_no_bundles_configured() {
        let cfg = Config::default();
        assert!(matches!(
            resolve(&cfg, "anything", None),
            Err(SkillRefError::NoBundles),
        ));
    }

    #[test]
    fn errors_on_unknown_bundle_when_qualified() {
        let cfg = cfg_with_bundles(&["alpha"]);
        let err = resolve(&cfg, "name", Some("beta")).unwrap_err();
        assert!(matches!(err, SkillRefError::UnknownBundle(b) if b == "beta"));
    }

    #[test]
    fn auto_resolves_when_single_bundle() {
        let cfg = cfg_with_bundles(&["alpha"]);
        let r = resolve(&cfg, "code-review", None).unwrap();
        assert_eq!(r.bundle(), "alpha");
        assert_eq!(r.name(), "code-review");
    }

    #[test]
    fn errors_on_ambiguity_when_multiple_bundles_and_no_qualifier() {
        let cfg = cfg_with_bundles(&["alpha", "beta"]);
        let err = resolve(&cfg, "code-review", None).unwrap_err();
        let candidates = match err {
            SkillRefError::AmbiguousName { candidates, .. } => candidates,
            other => panic!("expected AmbiguousName, got {other:?}"),
        };
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().any(|c| c == "alpha"));
        assert!(candidates.iter().any(|c| c == "beta"));
    }

    #[test]
    fn qualified_resolves_in_multi_bundle_config() {
        let cfg = cfg_with_bundles(&["alpha", "beta"]);
        let r = resolve(&cfg, "code-review", Some("beta")).unwrap();
        assert_eq!(r.bundle(), "beta");
        assert_eq!(r.name(), "code-review");
    }
}
