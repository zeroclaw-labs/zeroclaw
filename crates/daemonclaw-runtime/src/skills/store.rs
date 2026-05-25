use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use super::types::{
    AgentSkill, AgentSkillFrontmatter, AgentSkillMeta, SkillCategory, SkillSource,
    parse_skill_md, render_skill_md, validate_frontmatter, validate_skill_name,
};

/// Filesystem-backed skill store with three-category layout:
///
/// ```text
/// skills/
/// ├── bundled/           # Read-only, shipped with binary
/// ├── imported/          # Read-only, installed by operator
/// ├── agent/             # Read-write, agent-created
/// └── .archive/          # Curator-archived skills (recoverable)
/// ```
pub struct SkillStore {
    base_dir: PathBuf,
}

impl SkillStore {
    pub fn new(workspace_dir: &Path) -> Self {
        Self {
            base_dir: workspace_dir.join("skills"),
        }
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    fn category_dir(&self, category: SkillCategory) -> PathBuf {
        match category {
            SkillCategory::Bundled => self.base_dir.join("bundled"),
            SkillCategory::Imported => self.base_dir.join("imported"),
            SkillCategory::Agent => self.base_dir.join("agent"),
        }
    }

    fn archive_dir(&self) -> PathBuf {
        self.base_dir.join(".archive")
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        for cat in [SkillCategory::Bundled, SkillCategory::Imported, SkillCategory::Agent] {
            let dir = self.category_dir(cat);
            if !dir.exists() {
                std::fs::create_dir_all(&dir)
                    .with_context(|| format!("create {}", dir.display()))?;
            }
        }
        let archive = self.archive_dir();
        if !archive.exists() {
            std::fs::create_dir_all(&archive)
                .with_context(|| format!("create {}", archive.display()))?;
        }
        Ok(())
    }

    /// Load a single skill from a directory containing SKILL.md.
    fn load_from_dir(dir: &Path, category: SkillCategory) -> Result<AgentSkill> {
        let skill_md = dir.join("SKILL.md");
        let content = std::fs::read_to_string(&skill_md)
            .with_context(|| format!("read {}", skill_md.display()))?;
        let (frontmatter, body) = parse_skill_md(&content)?;
        let dir_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let errors = validate_frontmatter(&frontmatter, Some(dir_name));
        if !errors.is_empty() {
            let msgs: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
            anyhow::bail!("skill validation failed: {}", msgs.join("; "));
        }
        Ok(AgentSkill {
            frontmatter,
            body,
            category,
            dir_path: dir.to_path_buf(),
        })
    }

    /// List all skills in a given category.
    fn list_category(&self, category: SkillCategory) -> Vec<AgentSkill> {
        let dir = self.category_dir(category);
        if !dir.exists() {
            return Vec::new();
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut skills = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if !path.join("SKILL.md").exists() {
                continue;
            }
            match Self::load_from_dir(&path, category) {
                Ok(skill) => skills.push(skill),
                Err(e) => {
                    tracing::warn!(
                        "skipping skill {}: {e}",
                        path.display()
                    );
                }
            }
        }
        skills
    }

    /// List all skills across all categories.
    pub fn list_all(&self) -> Vec<AgentSkill> {
        let mut all = Vec::new();
        for cat in [SkillCategory::Bundled, SkillCategory::Imported, SkillCategory::Agent] {
            all.extend(self.list_category(cat));
        }
        all
    }

    /// List only agent-created skills.
    pub fn list_agent(&self) -> Vec<AgentSkill> {
        self.list_category(SkillCategory::Agent)
    }

    /// List archived skills.
    pub fn list_archived(&self) -> Vec<AgentSkill> {
        let dir = self.archive_dir();
        if !dir.exists() {
            return Vec::new();
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut skills = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() || !path.join("SKILL.md").exists() {
                continue;
            }
            match Self::load_from_dir(&path, SkillCategory::Agent) {
                Ok(mut skill) => {
                    skill.dir_path = path;
                    skills.push(skill);
                }
                Err(e) => {
                    tracing::warn!("skipping archived skill {}: {e}", path.display());
                }
            }
        }
        skills
    }

    /// Load a specific skill by name, searching all categories.
    pub fn get(&self, name: &str) -> Result<Option<AgentSkill>> {
        for cat in [SkillCategory::Bundled, SkillCategory::Imported, SkillCategory::Agent] {
            let dir = self.category_dir(cat).join(name);
            if dir.join("SKILL.md").exists() {
                return Ok(Some(Self::load_from_dir(&dir, cat)?));
            }
        }
        Ok(None)
    }

    /// Load a specific agent skill by name.
    pub fn get_agent(&self, name: &str) -> Result<Option<AgentSkill>> {
        let dir = self.category_dir(SkillCategory::Agent).join(name);
        if dir.join("SKILL.md").exists() {
            Ok(Some(Self::load_from_dir(&dir, SkillCategory::Agent)?))
        } else {
            Ok(None)
        }
    }

    /// Create a new agent skill. Fails if a skill with the same name exists.
    pub fn create(&self, frontmatter: &AgentSkillFrontmatter, body: &str) -> Result<PathBuf> {
        validate_skill_name(&frontmatter.name)?;
        let errors = validate_frontmatter(frontmatter, None);
        if !errors.is_empty() {
            let msgs: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
            anyhow::bail!("validation failed: {}", msgs.join("; "));
        }

        let skill_dir = self.category_dir(SkillCategory::Agent).join(&frontmatter.name);
        if skill_dir.exists() {
            anyhow::bail!("skill '{}' already exists", frontmatter.name);
        }
        std::fs::create_dir_all(&skill_dir)
            .with_context(|| format!("create {}", skill_dir.display()))?;

        let content = render_skill_md(frontmatter, body)?;
        let skill_md = skill_dir.join("SKILL.md");
        std::fs::write(&skill_md, &content)
            .with_context(|| format!("write {}", skill_md.display()))?;

        Ok(skill_dir)
    }

    /// Write a full SKILL.md for an existing agent skill (atomic: write to .tmp, rename).
    pub fn write_agent(&self, name: &str, frontmatter: &AgentSkillFrontmatter, body: &str) -> Result<()> {
        let skill_dir = self.category_dir(SkillCategory::Agent).join(name);
        if !skill_dir.exists() {
            anyhow::bail!("agent skill '{}' does not exist", name);
        }
        let content = render_skill_md(frontmatter, body)?;
        let tmp = skill_dir.join("SKILL.md.tmp");
        let target = skill_dir.join("SKILL.md");
        std::fs::write(&tmp, &content)
            .with_context(|| format!("write {}", tmp.display()))?;
        std::fs::rename(&tmp, &target)
            .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;
        Ok(())
    }

    /// Archive an agent skill (move to .archive/).
    pub fn archive(&self, name: &str) -> Result<()> {
        let src = self.category_dir(SkillCategory::Agent).join(name);
        if !src.exists() {
            anyhow::bail!("agent skill '{}' does not exist", name);
        }
        let archive = self.archive_dir();
        if !archive.exists() {
            std::fs::create_dir_all(&archive)?;
        }
        let dest = archive.join(name);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)
                .with_context(|| format!("remove old archive {}", dest.display()))?;
        }
        std::fs::rename(&src, &dest)
            .with_context(|| format!("archive {} -> {}", src.display(), dest.display()))?;
        Ok(())
    }

    /// Restore an archived skill back to agent/.
    pub fn restore(&self, name: &str) -> Result<()> {
        let src = self.archive_dir().join(name);
        if !src.exists() {
            anyhow::bail!("archived skill '{}' does not exist", name);
        }
        let dest = self.category_dir(SkillCategory::Agent).join(name);
        if dest.exists() {
            anyhow::bail!("skill '{}' already exists in agent/", name);
        }
        std::fs::rename(&src, &dest)
            .with_context(|| format!("restore {} -> {}", src.display(), dest.display()))?;
        Ok(())
    }

    /// Check if a skill directory has an `.active` lockfile.
    pub fn is_active(dir: &Path) -> bool {
        dir.join(".active").exists()
    }

    /// Write an `.active` lockfile for a skill.
    pub fn set_active(dir: &Path) -> Result<()> {
        std::fs::write(dir.join(".active"), "")
            .with_context(|| format!("write .active in {}", dir.display()))
    }

    /// Remove the `.active` lockfile.
    pub fn clear_active(dir: &Path) -> Result<()> {
        let p = dir.join(".active");
        if p.exists() {
            std::fs::remove_file(&p)
                .with_context(|| format!("remove {}", p.display()))?;
        }
        Ok(())
    }

    /// Remove stale `.active` files across all agent skills.
    pub fn cleanup_stale_active_files(&self) {
        let agent_dir = self.category_dir(SkillCategory::Agent);
        if !agent_dir.exists() {
            return;
        }
        let Ok(entries) = std::fs::read_dir(&agent_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let active = path.join(".active");
                if active.exists() {
                    if let Err(e) = std::fs::remove_file(&active) {
                        tracing::warn!("failed to remove stale .active in {}: {e}", path.display());
                    } else {
                        tracing::info!("removed stale .active from {}", path.display());
                    }
                }
            }
        }
    }

    /// Install a skill from a local directory into imported/.
    pub fn install_local(&self, source_dir: &Path) -> Result<String> {
        let skill_md = source_dir.join("SKILL.md");
        if !skill_md.exists() {
            anyhow::bail!("source directory does not contain SKILL.md");
        }
        let content = std::fs::read_to_string(&skill_md)?;
        let (fm, _body) = parse_skill_md(&content)?;
        let errors = validate_frontmatter(&fm, None);
        if !errors.is_empty() {
            let msgs: Vec<String> = errors.iter().map(|e| e.to_string()).collect();
            anyhow::bail!("skill validation failed: {}", msgs.join("; "));
        }
        let dir_name = source_dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !dir_name.is_empty() && dir_name != fm.name {
            anyhow::bail!(
                "directory name '{}' does not match skill name '{}'",
                dir_name, fm.name
            );
        }

        let dest = self.category_dir(SkillCategory::Imported).join(&fm.name);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)?;
        }
        copy_dir_recursive(source_dir, &dest)?;
        Ok(fm.name)
    }

    /// Install a skill from a git URL into imported/.
    pub fn install_git(&self, url: &str) -> Result<String> {
        if !has_cmd("git") {
            anyhow::bail!("git is not available on PATH; cannot install from git URL");
        }
        let tmp = tempfile::tempdir()?;
        let status = std::process::Command::new("git")
            .args(["clone", "--depth", "1", url, &tmp.path().display().to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()
            .context("failed to run git clone")?;
        if !status.success() {
            anyhow::bail!("git clone failed with exit code {}", status);
        }
        let result = self.install_local(tmp.path());
        // tempdir is cleaned up on drop
        result
    }

    /// Build a compact skill catalog for system prompt injection.
    ///
    /// Returns a formatted string listing all skills with name, category, and
    /// description — suitable for progressive disclosure where the agent calls
    /// `skill_activate` to load full instructions.
    pub fn build_catalog(&self) -> String {
        use std::fmt::Write;

        let skills = self.list_all();
        if skills.is_empty() {
            return String::new();
        }

        let mut out = String::from(
            "## Agent Skill Catalog\n\n\
             Skills listed below are available for activation. \
             Call `skill_activate` with a skill name to load its full instructions. \
             Call `skill_manage` to create, update, archive, or restore agent skills.\n\n\
             <skill_catalog>\n",
        );

        for skill in &skills {
            let _ = writeln!(
                out,
                "  <skill name=\"{}\" category=\"{}\">{}</skill>",
                skill.name(),
                skill.category,
                skill.description(),
            );
        }

        out.push_str("</skill_catalog>");
        out
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn has_cmd(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store() -> (TempDir, SkillStore) {
        let tmp = TempDir::new().unwrap();
        let store = SkillStore::new(tmp.path());
        store.ensure_dirs().unwrap();
        (tmp, store)
    }

    fn sample_frontmatter(name: &str) -> AgentSkillFrontmatter {
        AgentSkillFrontmatter {
            name: name.to_string(),
            description: "A test skill for unit tests.".into(),
            license: None,
            metadata: AgentSkillMeta {
                created: Some("2026-05-18T00:00:00Z".into()),
                updated: Some("2026-05-18T00:00:00Z".into()),
                ..AgentSkillMeta::default()
            },
        }
    }

    #[test]
    fn ensure_dirs_creates_layout() {
        let (tmp, _store) = test_store();
        assert!(tmp.path().join("skills/bundled").is_dir());
        assert!(tmp.path().join("skills/imported").is_dir());
        assert!(tmp.path().join("skills/agent").is_dir());
        assert!(tmp.path().join("skills/.archive").is_dir());
    }

    #[test]
    fn create_and_get_agent_skill() {
        let (_tmp, store) = test_store();
        let fm = sample_frontmatter("test-skill");
        let body = "# Test Skill\n\n## Procedure\n1. Do thing.\n";
        store.create(&fm, body).unwrap();

        let loaded = store.get("test-skill").unwrap().unwrap();
        assert_eq!(loaded.name(), "test-skill");
        assert_eq!(loaded.category, SkillCategory::Agent);
        assert!(loaded.body.contains("## Procedure"));
    }

    #[test]
    fn create_duplicate_fails() {
        let (_tmp, store) = test_store();
        let fm = sample_frontmatter("dup-skill");
        store.create(&fm, "body").unwrap();
        assert!(store.create(&fm, "body").is_err());
    }

    #[test]
    fn list_all_and_by_category() {
        let (_tmp, store) = test_store();
        store.create(&sample_frontmatter("skill-a"), "body a").unwrap();
        store.create(&sample_frontmatter("skill-b"), "body b").unwrap();

        let all = store.list_all();
        assert_eq!(all.len(), 2);
        let agent = store.list_agent();
        assert_eq!(agent.len(), 2);
    }

    #[test]
    fn archive_and_restore() {
        let (_tmp, store) = test_store();
        store.create(&sample_frontmatter("archivable"), "body").unwrap();
        assert!(store.get_agent("archivable").unwrap().is_some());

        store.archive("archivable").unwrap();
        assert!(store.get_agent("archivable").unwrap().is_none());
        let archived = store.list_archived();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].name(), "archivable");

        store.restore("archivable").unwrap();
        assert!(store.get_agent("archivable").unwrap().is_some());
        assert!(store.list_archived().is_empty());
    }

    #[test]
    fn write_agent_atomic() {
        let (_tmp, store) = test_store();
        let fm = sample_frontmatter("writable");
        store.create(&fm, "original body").unwrap();

        let mut updated_fm = fm.clone();
        updated_fm.metadata.usage_count = 5;
        store.write_agent("writable", &updated_fm, "updated body").unwrap();

        let loaded = store.get_agent("writable").unwrap().unwrap();
        assert_eq!(loaded.meta().usage_count, 5);
        assert!(loaded.body.contains("updated body"));
    }

    #[test]
    fn active_lockfile_lifecycle() {
        let (_tmp, store) = test_store();
        store.create(&sample_frontmatter("lockable"), "body").unwrap();
        let dir = store.category_dir(SkillCategory::Agent).join("lockable");

        assert!(!SkillStore::is_active(&dir));
        SkillStore::set_active(&dir).unwrap();
        assert!(SkillStore::is_active(&dir));
        SkillStore::clear_active(&dir).unwrap();
        assert!(!SkillStore::is_active(&dir));
    }

    #[test]
    fn cleanup_stale_active_files() {
        let (_tmp, store) = test_store();
        store.create(&sample_frontmatter("stale-active"), "body").unwrap();
        let dir = store.category_dir(SkillCategory::Agent).join("stale-active");
        SkillStore::set_active(&dir).unwrap();
        assert!(SkillStore::is_active(&dir));

        store.cleanup_stale_active_files();
        assert!(!SkillStore::is_active(&dir));
    }

    #[test]
    fn install_local_skill() {
        let (_tmp, store) = test_store();
        let src = TempDir::new().unwrap();
        let skill_dir = src.path().join("my-local-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let fm = sample_frontmatter("my-local-skill");
        let content = render_skill_md(&fm, "# Local Skill\n").unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();

        let name = store.install_local(&skill_dir).unwrap();
        assert_eq!(name, "my-local-skill");

        let skills = store.list_all();
        assert!(skills.iter().any(|s| s.name() == "my-local-skill"));
    }

    #[test]
    fn install_local_missing_skill_md() {
        let (_tmp, store) = test_store();
        let src = TempDir::new().unwrap();
        let skill_dir = src.path().join("no-skill-md");
        std::fs::create_dir_all(&skill_dir).unwrap();

        let err = store.install_local(&skill_dir).unwrap_err();
        assert!(err.to_string().contains("SKILL.md"), "got: {err}");
    }

    #[test]
    fn install_local_name_mismatch() {
        let (_tmp, store) = test_store();
        let src = TempDir::new().unwrap();
        let skill_dir = src.path().join("wrong-dir-name");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let fm = sample_frontmatter("actual-name");
        let content = render_skill_md(&fm, "body").unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), content).unwrap();

        let err = store.install_local(&skill_dir).unwrap_err();
        assert!(
            err.to_string().contains("does not match"),
            "got: {err}"
        );
    }

    #[test]
    fn install_local_overwrites_existing() {
        let (_tmp, store) = test_store();
        let src = TempDir::new().unwrap();
        let skill_dir = src.path().join("overwrite-me");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let fm = sample_frontmatter("overwrite-me");
        let content = render_skill_md(&fm, "version 1").unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), &content).unwrap();

        store.install_local(&skill_dir).unwrap();

        let content2 = render_skill_md(&fm, "version 2").unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), &content2).unwrap();
        store.install_local(&skill_dir).unwrap();

        let imported = store.category_dir(SkillCategory::Imported).join("overwrite-me");
        let disk = std::fs::read_to_string(imported.join("SKILL.md")).unwrap();
        assert!(disk.contains("version 2"));
    }

    #[test]
    fn install_git_bad_url_fails() {
        let (_tmp, store) = test_store();
        let result = store.install_git("https://invalid.example.com/nonexistent-repo.git");
        assert!(result.is_err());
    }

    #[test]
    fn build_catalog_empty() {
        let (_tmp, store) = test_store();
        let catalog = store.build_catalog();
        assert!(catalog.is_empty());
    }

    #[test]
    fn build_catalog_with_skills() {
        let (_tmp, store) = test_store();
        store.create(&sample_frontmatter("alpha"), "body a").unwrap();
        store.create(&sample_frontmatter("beta"), "body b").unwrap();

        let catalog = store.build_catalog();
        assert!(catalog.contains("<skill_catalog>"));
        assert!(catalog.contains("</skill_catalog>"));
        assert!(catalog.contains("alpha"));
        assert!(catalog.contains("beta"));
        assert!(catalog.contains("skill_activate"));
        assert!(catalog.contains("skill_manage"));
    }
}
