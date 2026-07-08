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
use super::{DroppedSkill, ShadowedSkill};
use std::collections::HashMap;
use zeroclaw_config::schema::Config;

/// Per-skill view returned by [`SkillsService::list_skills`].
// `Eq` dropped: `frontmatter.slash_options` carry `f64` bounds (not `Eq`).
#[derive(Debug, Clone, PartialEq)]
pub struct SkillSummary {
    pub r#ref: SkillRef,
    pub directory: PathBuf,
    pub frontmatter: SkillFrontmatter,
}

/// Where an agent-effective skill came from. The dashboard mirrors the
/// runtime's four-source union ([`super::load_skills_for_agent_from_config`])
/// so an operator sees the same skills the agent actually loads — not just
/// the `[skill_bundles.*]` table. Only [`SkillOrigin::Bundle`] skills are
/// editable through the bundle write APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillOrigin {
    /// `<install>/agents/<alias>/workspace/skills/`.
    Workspace,
    /// The open-skills repo (tagged `open-skills`).
    OpenSkills,
    /// A `plugins-wasm` plugin (`plugin:<name>/...`); holds the plugin name.
    Plugin(String),
    /// A configured `[skill_bundles.<alias>]`; holds the bundle alias.
    Bundle(String),
}

/// One skill in an agent's *effective* set, with provenance — returned by
/// [`SkillsService::resolve_effective_skills`].
#[derive(Debug, Clone)]
pub struct EffectiveSkill {
    pub name: String,
    pub description: String,
    pub origin: SkillOrigin,
    pub directory: Option<PathBuf>,
    /// `true` only for [`SkillOrigin::Bundle`] — the only writable source.
    pub editable: bool,
    /// `Some(alias)` iff `editable` (routes the bundle editor).
    pub bundle: Option<String>,
    /// Lower-precedence same-name skills this one shadowed (didn't load).
    /// Empty for the common case. (#7963)
    pub shadowed: Vec<ShadowedSkill>,
}

/// Result of resolving an agent's effective skills: the loaded set plus the
/// audit-dropped candidates the resolver skipped. Lets the dashboard tell
/// "no skills configured" (both empty) apart from "all failed audit"
/// (skills empty, dropped non-empty). (#7963)
#[derive(Debug, Clone)]
pub struct EffectiveSkillSet {
    pub skills: Vec<EffectiveSkill>,
    pub dropped: Vec<DroppedSkill>,
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
    #[error(
        "skill '{name}' is not editable: it is a {origin} skill, only bundle skills are writable"
    )]
    NotEditable { name: String, origin: String },
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

    /// An agent's *effective* skill set — the four-source union the runtime
    /// actually loads (workspace / open-skills / plugin / bundle), each tagged
    /// with its [`SkillOrigin`]. This fixes the dashboard's "shows zero skills
    /// when skills exist" gap (#7757): it is sourced from the **audited**
    /// resolver [`super::load_skills_for_agent_from_config`], NOT from
    /// [`Self::list_skills`] (which is bundle-only and does a raw, unaudited
    /// `read_dir`) — so the page reflects exactly what the agent injects, and
    /// never surfaces un-audited external (open-skills/plugin) frontmatter.
    pub fn resolve_effective_skills(
        &self,
        agent_alias: &str,
    ) -> Result<EffectiveSkillSet, ServiceError> {
        // Resolve each configured bundle's directory once, to attribute
        // bundle-origin skills by `location` prefix.
        let bundles = self.list_bundles()?;
        let (skills, dropped, shadows) =
            super::load_skills_for_agent_from_config_audited(self.config, agent_alias);
        // Group shadow records by the winning skill's name so each
        // EffectiveSkill can carry the losers it shadowed. (#7963)
        let mut shadow_index: HashMap<String, Vec<ShadowedSkill>> = HashMap::new();
        for sh in shadows {
            shadow_index.entry(sh.name.clone()).or_default().push(sh);
        }
        let skills = skills
            .into_iter()
            .map(|s| {
                let origin = Self::derive_origin(&s, &bundles);
                let (editable, bundle) = match &origin {
                    SkillOrigin::Bundle(alias) => (true, Some(alias.clone())),
                    _ => (false, None),
                };
                let shadowed = shadow_index.remove(&s.name).unwrap_or_default();
                EffectiveSkill {
                    name: s.name,
                    description: s.description,
                    origin,
                    directory: s.location,
                    editable,
                    bundle,
                    shadowed,
                }
            })
            .collect();
        Ok(EffectiveSkillSet { skills, dropped })
    }

    /// Attribute a resolved skill to its [`SkillOrigin`], mirroring the
    /// resolver's own discriminators so dashboard provenance can't drift: the
    /// `open-skills` tag, the `plugin:` name/tag prefix, then a `location`
    /// match against a configured bundle directory; otherwise the workspace.
    fn derive_origin(skill: &super::Skill, bundles: &[BundleSummary]) -> SkillOrigin {
        if skill.tags.iter().any(|t| t == "open-skills") {
            return SkillOrigin::OpenSkills;
        }
        if let Some(rest) = skill.name.strip_prefix("plugin:") {
            let plugin = rest.split('/').next().unwrap_or(rest);
            return SkillOrigin::Plugin(plugin.to_string());
        }
        if let Some(plugin) = skill.tags.iter().find_map(|t| t.strip_prefix("plugin:")) {
            return SkillOrigin::Plugin(plugin.to_string());
        }
        if let Some(loc) = &skill.location {
            for b in bundles {
                if loc.starts_with(&b.directory) {
                    return SkillOrigin::Bundle(b.alias.clone());
                }
            }
        }
        SkillOrigin::Workspace
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

    /// A bundle write/delete is permitted only when the target resolves to an
    /// existing skill *inside its bundle directory* — i.e. a real `Bundle`-origin
    /// skill. Workspace/open-skills/plugin skills never have a manifest under the
    /// bundle dir, so this rejects them with [`ServiceError::NotEditable`] rather
    /// than a misleading [`ServiceError::NotFound`]. Operates on the bundle `dir`
    /// the caller already resolved and existence-checked, so a truly-absent target
    /// still surfaces as `NotFound` and this guard fires only for a dir that exists
    /// but is not a bundle skill (one `skill_directory` resolve per call, no
    /// assumption that a second resolve returns the same path). (#7963 write-guard)
    fn ensure_editable(&self, target: &SkillRef, dir: &Path) -> Result<(), ServiceError> {
        if has_manifest(dir) {
            Ok(())
        } else {
            Err(ServiceError::NotEditable {
                name: target.to_string(),
                origin: "non-bundle".into(),
            })
        }
    }

    /// Overwrite the `SKILL.md` for a resolved skill.
    pub fn write_skill(&self, target: &SkillRef, doc: &SkillDocument) -> Result<(), ServiceError> {
        let dir = self.skill_directory(target)?;
        if !dir.exists() {
            return Err(ServiceError::NotFound(target.to_string()));
        }
        self.ensure_editable(target, &dir)?;
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
        self.ensure_editable(target, &dir)?;
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

    // #7757: provenance derivation mirrors the resolver's own discriminators.
    #[test]
    fn derive_origin_classifies_each_source() {
        let bundles = vec![BundleSummary {
            alias: "core".into(),
            directory: PathBuf::from("/inst/shared/skills/core"),
            include: vec![],
            exclude: vec![],
        }];
        let mk = |name: &str, tags: &[&str], loc: Option<&str>| crate::skills::Skill {
            name: name.into(),
            description: String::new(),
            version: String::new(),
            author: None,
            tags: tags.iter().map(|s| (*s).to_string()).collect(),
            tools: vec![],
            prompts: vec![],
            slash_options: vec![],
            location: loc.map(PathBuf::from),
            description_localizations: Default::default(),
        };
        assert_eq!(
            SkillsService::derive_origin(&mk("s", &["open-skills"], None), &bundles),
            SkillOrigin::OpenSkills
        );
        assert_eq!(
            SkillsService::derive_origin(&mk("plugin:foo/bar", &[], None), &bundles),
            SkillOrigin::Plugin("foo".into())
        );
        assert_eq!(
            SkillsService::derive_origin(
                &mk("s", &[], Some("/inst/shared/skills/core/s")),
                &bundles
            ),
            SkillOrigin::Bundle("core".into())
        );
        assert_eq!(
            SkillsService::derive_origin(
                &mk("s", &[], Some("/inst/agents/default/workspace/skills/s")),
                &bundles
            ),
            SkillOrigin::Workspace
        );
    }

    // #7757: the effective set unions non-bundle sources (workspace) with
    // bundle skills, tagging origin + editability — the gap that made the
    // dashboard render empty when only workspace skills existed.
    #[test]
    fn resolve_effective_skills_unions_workspace_and_bundle() {
        let (dir, mut cfg) = fixture(&["core"]);
        // install_root_dir() = config_path.parent(); align it with the service
        // install_root so the agent's bundle + workspace dirs resolve here.
        cfg.config_path = dir.path().join("config.toml");
        cfg.agents.insert(
            "default".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                skill_bundles: vec!["core".into()],
                ..Default::default()
            },
        );
        let svc = SkillsService::new(&cfg, dir.path());
        make_skill(&svc, "core", "bundle-skill");
        // A workspace skill on disk (the source the dashboard used to miss).
        let ws = cfg
            .agent_workspace_dir("default")
            .join("skills")
            .join("ws-skill");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(
            ws.join("SKILL.md"),
            "---\nname: ws-skill\ndescription: w\n---\n# ws\n",
        )
        .unwrap();

        let eff = svc.resolve_effective_skills("default").unwrap();
        let by = |n: &str| {
            eff.skills
                .iter()
                .find(|e| e.name == n)
                .unwrap_or_else(|| panic!("missing {n}"))
        };
        assert_eq!(
            by("bundle-skill").origin,
            SkillOrigin::Bundle("core".into())
        );
        assert!(by("bundle-skill").editable);
        assert_eq!(by("bundle-skill").bundle.as_deref(), Some("core"));
        assert_eq!(by("ws-skill").origin, SkillOrigin::Workspace);
        assert!(!by("ws-skill").editable);
        assert!(by("ws-skill").bundle.is_none());
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

    // #7963 write-guard: a write/delete targeting a directory that exists in the
    // bundle dir but lacks a manifest (i.e. not a real bundle skill) is rejected
    // with NotEditable, not the misleading NotFound.
    #[test]
    fn write_skill_rejects_non_bundle_skill() {
        let (dir, cfg) = fixture(&["alpha"]);
        let svc = SkillsService::new(&cfg, dir.path());
        // Create the dir WITHOUT any manifest — exists, but not a bundle skill.
        let skill_dir = dir.path().join("shared/skills/alpha/no-manifest");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let target = SkillRef::new_unchecked("alpha".into(), "no-manifest".into());
        let doc = SkillDocument {
            frontmatter: SkillFrontmatter {
                name: "no-manifest".into(),
                description: "x".into(),
                ..Default::default()
            },
            body: "# x\n".into(),
        };
        let err = svc.write_skill(&target, &doc).unwrap_err();
        assert!(
            matches!(err, ServiceError::NotEditable { .. }),
            "expected NotEditable, got {err:?}"
        );
    }

    #[test]
    fn write_skill_allows_existing_bundle_skill() {
        let (dir, cfg) = fixture(&["alpha"]);
        let svc = SkillsService::new(&cfg, dir.path());
        let target = make_skill(&svc, "alpha", "real");
        let doc = svc.read_skill(&target).unwrap();
        svc.write_skill(&target, &doc)
            .expect("bundle skill is editable");
    }

    #[test]
    fn remove_skill_rejects_non_bundle_skill() {
        let (dir, cfg) = fixture(&["alpha"]);
        let svc = SkillsService::new(&cfg, dir.path());
        let skill_dir = dir.path().join("shared/skills/alpha/no-manifest");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let target = SkillRef::new_unchecked("alpha".into(), "no-manifest".into());
        let err = svc.remove_skill(&target, RemoveMode::Purge).unwrap_err();
        assert!(
            matches!(err, ServiceError::NotEditable { .. }),
            "expected NotEditable, got {err:?}"
        );
        // The non-bundle dir must not be deleted by a rejected remove.
        assert!(skill_dir.exists());
    }

    // #7963 skipped-audit: resolve_effective_skills surfaces audit-dropped
    // candidates so the dashboard can distinguish "none configured" from
    // "all failed audit".
    #[test]
    fn resolve_effective_skills_reports_dropped() {
        let (dir, mut cfg) = fixture(&["core"]);
        cfg.config_path = dir.path().join("config.toml");
        cfg.agents.insert(
            "default".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                skill_bundles: vec!["core".into()],
                ..Default::default()
            },
        );
        let svc = SkillsService::new(&cfg, dir.path());
        // A workspace skill dir with a parse-broken manifest → dropped.
        let ws = cfg
            .agent_workspace_dir("default")
            .join("skills")
            .join("broken");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(
            ws.join("SKILL.toml"),
            "[skill]\nname = \"broken\"\ndescription = \"d\"\nbogus = true\n",
        )
        .unwrap();

        super::super::cache::invalidate();
        let set = svc.resolve_effective_skills("default").unwrap();
        assert!(
            set.skills.iter().all(|s| s.name != "broken"),
            "broken skill must not load"
        );
        assert_eq!(set.dropped.len(), 1, "the broken skill must be reported");
        assert_eq!(set.dropped[0].origin_hint, "workspace");
        assert!(matches!(
            set.dropped[0].reason,
            super::super::SkillDropReason::ManifestParseError(_)
        ));
    }

    #[test]
    fn resolve_effective_skills_empty_vs_all_dropped() {
        let (dir, mut cfg) = fixture(&["core"]);
        cfg.config_path = dir.path().join("config.toml");
        cfg.agents.insert(
            "default".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                skill_bundles: vec!["core".into()],
                ..Default::default()
            },
        );
        let svc = SkillsService::new(&cfg, dir.path());

        // (a) nothing configured → both empty.
        super::super::cache::invalidate();
        let empty = svc.resolve_effective_skills("default").unwrap();
        assert!(empty.skills.is_empty() && empty.dropped.is_empty());

        // (b) one audit-failing skill → skills empty, dropped non-empty.
        let ws = cfg
            .agent_workspace_dir("default")
            .join("skills")
            .join("broken");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(
            ws.join("SKILL.toml"),
            "[skill]\nname = \"broken\"\ndescription = \"d\"\nbogus = true\n",
        )
        .unwrap();
        super::super::cache::invalidate();
        let all_dropped = svc.resolve_effective_skills("default").unwrap();
        assert!(all_dropped.skills.is_empty());
        assert_eq!(all_dropped.dropped.len(), 1);
    }

    // #7963 shadowed-by: a workspace skill that also exists in an assigned
    // bundle wins, and the EffectiveSkill records the shadowed bundle skill.
    #[test]
    fn resolve_effective_skills_records_shadow() {
        let (dir, mut cfg) = fixture(&["core"]);
        cfg.config_path = dir.path().join("config.toml");
        cfg.agents.insert(
            "default".into(),
            zeroclaw_config::schema::AliasedAgentConfig {
                skill_bundles: vec!["core".into()],
                ..Default::default()
            },
        );
        let svc = SkillsService::new(&cfg, dir.path());
        // Bundle skill `foo`.
        make_skill(&svc, "core", "foo");
        // Workspace skill `foo` (same name, higher precedence).
        let ws = cfg
            .agent_workspace_dir("default")
            .join("skills")
            .join("foo");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(
            ws.join("SKILL.md"),
            "---\nname: foo\ndescription: w\n---\n# foo\n",
        )
        .unwrap();

        super::super::cache::invalidate();
        let set = svc.resolve_effective_skills("default").unwrap();
        let foos: Vec<_> = set.skills.iter().filter(|s| s.name == "foo").collect();
        assert_eq!(foos.len(), 1, "only the winning foo is in the set");
        assert_eq!(foos[0].origin, SkillOrigin::Workspace);
        assert_eq!(
            foos[0].shadowed,
            vec![super::super::ShadowedSkill {
                name: "foo".into(),
                origin_hint: "bundle".into(),
            }],
            "the winning workspace foo must record the shadowed bundle foo"
        );
    }
}
