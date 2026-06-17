//! Public service surface every consumer (CLI, gateway, future TUI) uses
//! to read and mutate skills + skill bundles. There is no second
//! implementation — drift is closed by construction.

use std::path::{Path, PathBuf};

use super::bundle::{self, BundleSummary};
use super::constants::{
    SKILL_ARCHIVE_DIR_NAME, SKILL_DEPRECATED_MANIFESTS, SKILL_MANIFEST_FILENAME,
};
use super::document::{DocumentParseError, SkillDocument};
use super::frontmatter::SkillFrontmatter;
use super::reference::{self, SkillRef, SkillRefError};
use super::scaffold::{self, ScaffoldError, ScaffoldOptions};
use zeroclaw_config::schema::Config;

/// Per-skill view returned by [`SkillsService::list_skills`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSummary {
    pub r#ref: SkillRef,
    pub directory: PathBuf,
    pub frontmatter: SkillFrontmatter,
}

/// Behaviour selector for [`SkillsService::remove_skill`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveMode {
    /// Move to `<install>/shared/skills/_deleted/<name>-<unix-ts>/`.
    Archive,
    /// `rm -rf`. Irreversible.
    Purge,
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Ref(#[from] SkillRefError),
    #[error(transparent)]
    Bundle(#[from] bundle::BundleError),
    #[error(transparent)]
    Scaffold(#[from] ScaffoldError),
    #[error(transparent)]
    DocumentParse(#[from] DocumentParseError),
    #[error("skill '{0}' is not present in any configured bundle")]
    NotFound(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Single source of truth for skill + skill-bundle operations.
///
/// Holds an immutable reference to `Config` and the install-root path. Reads
/// are filesystem operations against the resolved bundle directories;
/// writes go through the matching helpers in [`super::scaffold`],
/// [`super::bundle`], and [`super::document`] so a single rule lives in a
/// single place.
pub struct SkillsService<'a> {
    config: &'a Config,
    install_root: PathBuf,
}

impl<'a> SkillsService<'a> {
    pub fn new(config: &'a Config, install_root: impl Into<PathBuf>) -> Self {
        Self {
            config,
            install_root: install_root.into(),
        }
    }

    pub fn install_root(&self) -> &Path {
        &self.install_root
    }

    /// Resolve a `(name, bundle?)` pair into a unique [`SkillRef`] per the
    /// disambiguation rule defined in [`super::reference::resolve`].
    pub fn resolve_ref(&self, name: &str, bundle: Option<&str>) -> Result<SkillRef, ServiceError> {
        Ok(reference::resolve(self.config, name, bundle)?)
    }

    /// One [`BundleSummary`] per configured bundle, in HashMap order.
    pub fn list_bundles(&self) -> Result<Vec<BundleSummary>, ServiceError> {
        let mut out = Vec::with_capacity(self.config.skill_bundles.len());
        for (alias, cfg) in &self.config.skill_bundles {
            let directory = bundle::resolve_directory(self.config, &self.install_root, alias)?;
            out.push(BundleSummary {
                alias: alias.clone(),
                directory,
                include: cfg.include.clone(),
                exclude: cfg.exclude.clone(),
            });
        }
        Ok(out)
    }

    /// All skills in `bundle_filter` (or all bundles when `None`). Skips any
    /// child directory that's missing a canonical or deprecated manifest.
    pub fn list_skills(
        &self,
        bundle_filter: Option<&str>,
    ) -> Result<Vec<SkillSummary>, ServiceError> {
        let mut out = Vec::new();
        for summary in self.list_bundles()? {
            if let Some(filter) = bundle_filter
                && summary.alias != filter
            {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&summary.directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                if !has_manifest(&path) {
                    continue;
                }
                let canonical_path = path.join(SKILL_MANIFEST_FILENAME);
                let Ok(content) = std::fs::read_to_string(&canonical_path) else {
                    continue;
                };
                let Ok(doc) = SkillDocument::parse(&content) else {
                    continue;
                };
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                out.push(SkillSummary {
                    r#ref: SkillRef::new_unchecked(summary.alias.clone(), name),
                    directory: path,
                    frontmatter: doc.frontmatter,
                });
            }
        }
        Ok(out)
    }

    /// Read the `SKILL.md` for a resolved skill.
    pub fn read_skill(&self, target: &SkillRef) -> Result<SkillDocument, ServiceError> {
        let path = self.skill_directory(target)?.join(SKILL_MANIFEST_FILENAME);
        let content = std::fs::read_to_string(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ServiceError::NotFound(target.to_string())
            } else {
                ServiceError::Io(e)
            }
        })?;
        Ok(SkillDocument::parse(&content)?)
    }

    /// Overwrite the `SKILL.md` for a resolved skill.
    pub fn write_skill(&self, target: &SkillRef, doc: &SkillDocument) -> Result<(), ServiceError> {
        let dir = self.skill_directory(target)?;
        if !dir.exists() {
            return Err(ServiceError::NotFound(target.to_string()));
        }
        std::fs::write(dir.join(SKILL_MANIFEST_FILENAME), doc.serialize())?;
        super::cache::invalidate();
        Ok(())
    }

    /// Materialize a brand-new skill on disk per the canonical layout.
    pub fn scaffold_skill(
        &self,
        target: &SkillRef,
        frontmatter: SkillFrontmatter,
        opts: ScaffoldOptions,
    ) -> Result<PathBuf, ServiceError> {
        let path =
            scaffold::scaffold_skill(self.config, &self.install_root, target, frontmatter, opts)?;
        super::cache::invalidate();
        Ok(path)
    }

    /// Archive or purge a skill directory.
    pub fn remove_skill(&self, target: &SkillRef, mode: RemoveMode) -> Result<(), ServiceError> {
        let dir = self.skill_directory(target)?;
        if !dir.exists() {
            return Err(ServiceError::NotFound(target.to_string()));
        }
        match mode {
            RemoveMode::Purge => std::fs::remove_dir_all(&dir)?,
            RemoveMode::Archive => {
                let archive_root = self
                    .install_root
                    .join("shared")
                    .join("skills")
                    .join(SKILL_ARCHIVE_DIR_NAME);
                std::fs::create_dir_all(&archive_root)?;
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let archive_name = format!("{}-{}-{}", target.bundle(), target.name(), ts);
                std::fs::rename(&dir, archive_root.join(archive_name))?;
            }
        }
        super::cache::invalidate();
        Ok(())
    }

    fn skill_directory(&self, target: &SkillRef) -> Result<PathBuf, ServiceError> {
        let bundle_dir =
            bundle::resolve_directory(self.config, &self.install_root, target.bundle())?;
        Ok(bundle_dir.join(target.name()))
    }
}

fn has_manifest(path: &Path) -> bool {
    if path.join(SKILL_MANIFEST_FILENAME).is_file() {
        return true;
    }
    SKILL_DEPRECATED_MANIFESTS
        .iter()
        .any(|name| path.join(name).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use zeroclaw_config::schema::SkillBundleConfig;

    fn fixture(bundles: &[&str]) -> (TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        for alias in bundles {
            cfg.skill_bundles
                .insert((*alias).to_string(), SkillBundleConfig::default());
        }
        (dir, cfg)
    }

    fn make_skill(svc: &SkillsService, bundle: &str, name: &str) -> SkillRef {
        let target = SkillRef::new_unchecked(bundle.into(), name.into());
        svc.scaffold_skill(
            &target,
            SkillFrontmatter {
                name: name.into(),
                description: "stub".into(),
                ..Default::default()
            },
            ScaffoldOptions::default(),
        )
        .unwrap();
        target
    }

    #[test]
    fn list_bundles_includes_default_directory_for_unset_field() {
        let (dir, cfg) = fixture(&["alpha"]);
        let svc = SkillsService::new(&cfg, dir.path());
        let bundles = svc.list_bundles().unwrap();
        assert_eq!(bundles.len(), 1);
        assert_eq!(bundles[0].alias, "alpha");
        assert_eq!(bundles[0].directory, dir.path().join("shared/skills/alpha"),);
    }

    #[test]
    fn list_skills_returns_empty_when_bundle_dir_absent() {
        let (dir, cfg) = fixture(&["alpha"]);
        let svc = SkillsService::new(&cfg, dir.path());
        assert!(svc.list_skills(None).unwrap().is_empty());
    }

    #[test]
    fn scaffold_then_list_round_trip() {
        let (dir, cfg) = fixture(&["alpha"]);
        let svc = SkillsService::new(&cfg, dir.path());
        make_skill(&svc, "alpha", "code-review");
        let skills = svc.list_skills(None).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].r#ref.name(), "code-review");
        assert_eq!(skills[0].frontmatter.description, "stub");
    }

    #[test]
    fn list_skills_filters_by_bundle() {
        let (dir, cfg) = fixture(&["alpha", "beta"]);
        let svc = SkillsService::new(&cfg, dir.path());
        make_skill(&svc, "alpha", "a-skill");
        make_skill(&svc, "beta", "b-skill");
        let alpha_only = svc.list_skills(Some("alpha")).unwrap();
        assert_eq!(alpha_only.len(), 1);
        assert_eq!(alpha_only[0].r#ref.bundle(), "alpha");
    }

    #[test]
    fn read_and_write_round_trip_preserves_frontmatter() {
        let (dir, cfg) = fixture(&["alpha"]);
        let svc = SkillsService::new(&cfg, dir.path());
        let target = make_skill(&svc, "alpha", "rw");

        let mut doc = svc.read_skill(&target).unwrap();
        doc.frontmatter.description = "updated description text".into();
        doc.frontmatter.license = Some("MIT".into());
        svc.write_skill(&target, &doc).unwrap();

        let reread = svc.read_skill(&target).unwrap();
        assert_eq!(reread.frontmatter.description, "updated description text");
        assert_eq!(reread.frontmatter.license.as_deref(), Some("MIT"));
    }

    #[test]
    fn remove_archive_moves_to_deleted_root_and_leaves_no_trace() {
        let (dir, cfg) = fixture(&["alpha"]);
        let svc = SkillsService::new(&cfg, dir.path());
        let target = make_skill(&svc, "alpha", "to-archive");
        let original_dir = dir.path().join("shared/skills/alpha/to-archive");
        assert!(original_dir.exists());

        svc.remove_skill(&target, RemoveMode::Archive).unwrap();
        assert!(!original_dir.exists());
        let archive_root = dir.path().join("shared/skills/_deleted");
        assert!(archive_root.is_dir());
        let archived: Vec<_> = std::fs::read_dir(&archive_root)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(archived.len(), 1);
    }

    #[test]
    fn remove_purge_deletes_outright() {
        let (dir, cfg) = fixture(&["alpha"]);
        let svc = SkillsService::new(&cfg, dir.path());
        let target = make_skill(&svc, "alpha", "to-purge");
        let original_dir = dir.path().join("shared/skills/alpha/to-purge");
        svc.remove_skill(&target, RemoveMode::Purge).unwrap();
        assert!(!original_dir.exists());
        assert!(!dir.path().join("shared/skills/_deleted").exists());
    }

    #[test]
    fn read_skill_errors_with_not_found_for_missing_skill() {
        let (dir, cfg) = fixture(&["alpha"]);
        let svc = SkillsService::new(&cfg, dir.path());
        let target = SkillRef::new_unchecked("alpha".into(), "ghost".into());
        let err = svc.read_skill(&target).unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }
}
