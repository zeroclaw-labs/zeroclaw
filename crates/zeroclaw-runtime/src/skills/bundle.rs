//! Runtime-side bundle facade. The directory rules (default path, inside-
//! `shared/` containment, uniqueness) live in [`zeroclaw_config::skill_bundles`]
//! so `Config::validate` and the SkillsService share one implementation.
//! This module is a thin re-exporter plus the `BundleSummary` shape

use std::path::{Path, PathBuf};

use zeroclaw_config::schema::Config;
pub use zeroclaw_config::skill_bundles::{
    BundleDirectoryError as BundleError, default_directory, resolve_directory, validate_directory,
    validate_uniqueness,
};

/// Lightweight bundle view returned by [`crate::skills::service::SkillsService::list_bundles`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleSummary {
    pub alias: String,
    pub directory: PathBuf,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

/// Build a [`BundleSummary`] for a configured bundle alias. Resolves the
/// directory via [`resolve_directory`] so default-path behaviour stays
/// single-sourced.
pub fn summary(
    config: &Config,
    install_root: &Path,
    alias: &str,
) -> Result<BundleSummary, BundleError> {
    let bundle = config
        .skill_bundles
        .get(alias)
        .ok_or_else(|| BundleError::UnknownBundle(alias.to_string()))?;
    Ok(BundleSummary {
        alias: alias.to_string(),
        directory: resolve_directory(config, install_root, alias)?,
        include: bundle.include.clone(),
        exclude: bundle.exclude.clone(),
    })
}
