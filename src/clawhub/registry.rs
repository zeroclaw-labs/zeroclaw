// src/clawhub/registry.rs
//! Local registry for installed ClawHub skills

use crate::clawhub::types::{ClawHubRegistry, InstalledSkill};
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Local registry for installed ClawHub skills
pub struct Registry {
    registry_path: PathBuf,
    registry: ClawHubRegistry,
}

impl Registry {
    /// Create a new registry at the given config directory
    pub fn new(config_dir: &Path) -> Self {
        let registry_path = config_dir.join("clawhub_skills.json");
        let registry = if registry_path.exists() {
            let content = std::fs::read_to_string(&registry_path).unwrap_or_default();
            serde_json::from_str(&content).unwrap_or_default()
        } else {
            ClawHubRegistry::default()
        };

        Self {
            registry_path,
            registry,
        }
    }

    /// List all installed skills
    pub fn list_skills(&self) -> &[InstalledSkill] {
        &self.registry.skills
    }

    /// Add or update a skill in the registry
    pub fn add_skill(
        &mut self,
        slug: &str,
        name: &str,
        version: &str,
        source_url: &str,
    ) -> Result<()> {
        if let Some(existing) = self.registry.skills.iter_mut().find(|s| s.slug == slug) {
            existing.version = version.to_string();
            existing.updated_at = Some(chrono::Utc::now().to_rfc3339());
        } else {
            let skill = InstalledSkill {
                slug: slug.to_string(),
                name: name.to_string(),
                version: version.to_string(),
                source_url: source_url.to_string(),
                installed_at: chrono::Utc::now().to_rfc3339(),
                updated_at: None,
            };
            self.registry.skills.push(skill);
        }
        self.save()
    }

    /// Remove a skill from the registry
    pub fn remove_skill(&mut self, slug: &str) -> Result<()> {
        self.registry.skills.retain(|s| s.slug != slug);
        self.save()
    }

    /// Check if a skill is installed
    pub fn is_installed(&self, slug: &str) -> bool {
        self.registry.skills.iter().any(|s| s.slug == slug)
    }

    /// Save the registry to disk
    fn save(&self) -> Result<()> {
        if let Some(parent) = self.registry_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = serde_json::to_string_pretty(&self.registry)?;
        std::fs::write(&self.registry_path, content)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_registry_new_creates_empty() {
        let dir = tempdir().unwrap();
        let registry = Registry::new(dir.path());
        assert!(registry.list_skills().is_empty());
    }

    #[test]
    fn test_registry_add_and_list() {
        let dir = tempdir().unwrap();
        let mut registry = Registry::new(dir.path());

        registry
            .add_skill("test-skill", "Test Skill", "1.0.0", "https://example.com/test")
            .unwrap();

        let skills = registry.list_skills();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].slug, "test-skill");
        assert_eq!(skills[0].name, "Test Skill");
    }

    #[test]
    fn test_registry_is_installed() {
        let dir = tempdir().unwrap();
        let mut registry = Registry::new(dir.path());

        assert!(!registry.is_installed("test-skill"));

        registry
            .add_skill("test-skill", "Test Skill", "1.0.0", "https://example.com/test")
            .unwrap();

        assert!(registry.is_installed("test-skill"));
    }

    #[test]
    fn test_registry_update_existing() {
        let dir = tempdir().unwrap();
        let mut registry = Registry::new(dir.path());

        registry
            .add_skill("test-skill", "Test Skill", "1.0.0", "https://example.com/test")
            .unwrap();
        registry
            .add_skill("test-skill", "Test Skill", "2.0.0", "https://example.com/test")
            .unwrap();

        let skills = registry.list_skills();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].version, "2.0.0");
    }

    #[test]
    fn test_registry_remove_skill() {
        let dir = tempdir().unwrap();
        let mut registry = Registry::new(dir.path());

        registry
            .add_skill("test-skill", "Test Skill", "1.0.0", "https://example.com/test")
            .unwrap();
        assert!(registry.is_installed("test-skill"));

        registry.remove_skill("test-skill").unwrap();
        assert!(!registry.is_installed("test-skill"));
    }
}
