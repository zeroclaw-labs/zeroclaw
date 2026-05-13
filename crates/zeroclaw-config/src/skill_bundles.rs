//! Skill-bundle directory rules and helpers.
//!
//! Single source of truth for:
//! - the `shared/skills/<alias>/` default
//! - the inside-`shared/` containment rule
//! - the per-config uniqueness rule
//!
//! Lives in `zeroclaw-config` (not `zeroclaw-runtime/skills/bundle.rs`) so
//! [`crate::schema::Config::validate`] can call into it at load time.
//! Runtime's `bundle.rs` re-exports these functions; there is no second
//! implementation.

use std::path::{Path, PathBuf};

use crate::schema::Config;

/// Canonical default directory for a bundle: `<install>/shared/skills/<alias>/`.
#[must_use]
pub fn default_directory(install_root: &Path, alias: &str) -> PathBuf {
    install_root.join("shared").join("skills").join(alias)
}

/// Resolve the on-disk directory for a configured bundle, applying the
/// default when `[skill-bundles.<alias>].directory` is unset or empty.
/// Absolute paths configured by the user pass through verbatim; relative
/// paths are resolved against the install root.
pub fn resolve_directory(
    config: &Config,
    install_root: &Path,
    alias: &str,
) -> Result<PathBuf, BundleDirectoryError> {
    let bundle = config
        .skill_bundles
        .get(alias)
        .ok_or_else(|| BundleDirectoryError::UnknownBundle(alias.to_string()))?;

    let configured = bundle
        .directory
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let path = match configured {
        Some(raw) => {
            let candidate = PathBuf::from(raw);
            if candidate.is_absolute() {
                candidate
            } else {
                install_root.join(candidate)
            }
        }
        None => default_directory(install_root, alias),
    };
    Ok(path)
}

/// Reject directories that escape `<install>/shared/`. Run at scaffold time
/// and inside [`crate::schema::Config::validate`].
pub fn validate_directory(path: &Path, install_root: &Path) -> Result<(), BundleDirectoryError> {
    let shared = install_root.join("shared");
    let normalized = normalize_path(path);
    let shared_normalized = normalize_path(&shared);
    if !normalized.starts_with(&shared_normalized) {
        return Err(BundleDirectoryError::EscapesShared {
            path: normalized.display().to_string(),
            shared: shared_normalized.display().to_string(),
        });
    }
    Ok(())
}

/// Reject configs where two bundles resolve to the same directory.
pub fn validate_uniqueness(
    config: &Config,
    install_root: &Path,
) -> Result<(), BundleDirectoryError> {
    let mut seen: Vec<(String, PathBuf)> = Vec::with_capacity(config.skill_bundles.len());
    for alias in config.skill_bundles.keys() {
        let dir = resolve_directory(config, install_root, alias)?;
        let normalized = normalize_path(&dir);
        if let Some((other, _)) = seen.iter().find(|(_, p)| p == &normalized) {
            return Err(BundleDirectoryError::DirectoryCollision {
                path: normalized.display().to_string(),
                first: other.clone(),
                second: alias.clone(),
            });
        }
        seen.push((alias.clone(), normalized));
    }
    Ok(())
}

/// Lexical normalization that resolves `.` and `..` without touching the
/// filesystem. Sufficient for "stays inside `shared/`" reasoning where the
/// path may not yet exist on disk.
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        use std::path::Component;
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[derive(Debug, thiserror::Error)]
pub enum BundleDirectoryError {
    #[error("skill bundle '{0}' is not configured")]
    UnknownBundle(String),

    #[error(
        "skill-bundle directory '{path}' escapes the shared workspace at '{shared}'; bundles must stay inside `<install>/shared/`"
    )]
    EscapesShared { path: String, shared: String },

    #[error(
        "skill-bundles '{first}' and '{second}' both resolve to directory '{path}'; each bundle must own a unique directory"
    )]
    DirectoryCollision {
        path: String,
        first: String,
        second: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::SkillBundleConfig;

    fn cfg_with_bundle(alias: &str, directory: Option<&str>) -> Config {
        let mut cfg = Config::default();
        cfg.skill_bundles.insert(
            alias.to_string(),
            SkillBundleConfig {
                directory: directory.map(String::from),
                ..Default::default()
            },
        );
        cfg
    }

    #[test]
    fn defaults_to_shared_skills_alias_when_unset() {
        let cfg = cfg_with_bundle("alpha", None);
        let root = Path::new("/tmp/install");
        let resolved = resolve_directory(&cfg, root, "alpha").unwrap();
        assert_eq!(resolved, root.join("shared/skills/alpha"));
    }

    #[test]
    fn empty_directory_string_is_treated_as_unset() {
        let cfg = cfg_with_bundle("alpha", Some("   "));
        let root = Path::new("/tmp/install");
        assert_eq!(
            resolve_directory(&cfg, root, "alpha").unwrap(),
            root.join("shared/skills/alpha"),
        );
    }

    #[test]
    fn validate_directory_rejects_dotdot_escape() {
        let root = Path::new("/tmp/install");
        let path = root.join("shared/../etc");
        let err = validate_directory(&path, root).unwrap_err();
        assert!(matches!(err, BundleDirectoryError::EscapesShared { .. }));
    }

    #[test]
    fn uniqueness_rejects_two_bundles_pointing_at_same_dir() {
        let mut cfg = Config::default();
        cfg.skill_bundles.insert(
            "alpha".into(),
            SkillBundleConfig {
                directory: Some("shared/skills/shared-pool".into()),
                ..Default::default()
            },
        );
        cfg.skill_bundles.insert(
            "beta".into(),
            SkillBundleConfig {
                directory: Some("shared/skills/shared-pool".into()),
                ..Default::default()
            },
        );
        let err = validate_uniqueness(&cfg, Path::new("/tmp/install")).unwrap_err();
        assert!(matches!(
            err,
            BundleDirectoryError::DirectoryCollision { .. }
        ));
    }

    #[test]
    fn uniqueness_passes_for_distinct_default_directories() {
        let mut cfg = Config::default();
        cfg.skill_bundles
            .insert("alpha".into(), SkillBundleConfig::default());
        cfg.skill_bundles
            .insert("beta".into(), SkillBundleConfig::default());
        validate_uniqueness(&cfg, Path::new("/tmp/install")).unwrap();
    }
}
