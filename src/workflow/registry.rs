// Tool Registry — permission whitelist for workflow step execution
//
// Each category has a set of allowed tools. Workflows can only invoke
// tools that are whitelisted for their parent category.

use std::collections::{HashMap, HashSet};

use anyhow::{bail, Result};

/// Tool registry with per-category permission whitelist.
pub struct ToolRegistry {
    /// category_key → set of allowed tool names
    permissions: HashMap<String, HashSet<String>>,
}

impl ToolRegistry {
    /// Create an empty registry (all tools denied by default).
    pub fn new() -> Self {
        Self {
            permissions: HashMap::new(),
        }
    }

    /// Create a registry with default category→tool mappings.
    pub fn with_defaults() -> Self {
        let mut reg = Self::new();

        reg.allow("daily", &["calendar", "memo", "search", "web"]);
        reg.allow("shopping", &["web", "browser", "price_compare", "receipt"]);
        reg.allow("document", &["docx", "pdf", "xlsx", "pptx"]);
        reg.allow("coding", &["shell", "editor", "git", "linter"]);
        reg.allow("interpret", &["voice_interpret"]);
        reg.allow("phone", &["phone_router", "stt", "whisper_direct"]);
        reg.allow("image", &["imagegen", "image_edit"]);
        reg.allow("music", &["music_gen", "music_edit"]);
        reg.allow("video", &["video_gen", "video_edit"]);

        reg
    }

    /// Allow specific tools for a category.
    pub fn allow(&mut self, category: &str, tools: &[&str]) {
        let set = self
            .permissions
            .entry(category.to_string())
            .or_default();
        for tool in tools {
            set.insert((*tool).to_string());
        }
    }

    /// Check if a tool is permitted for the given category context.
    pub fn check_permission(&self, tool: &str, category: &str) -> Result<()> {
        if let Some(allowed) = self.permissions.get(category) {
            if allowed.contains(tool) {
                return Ok(());
            }
            bail!(
                "tool '{tool}' is not permitted for category '{category}'"
            );
        }
        // No explicit permissions for this category — deny by default
        bail!("no tool permissions defined for category '{category}'");
    }

    /// List all tools allowed for a category.
    pub fn tools_for_category(&self, category: &str) -> Vec<&str> {
        self.permissions
            .get(category)
            .map(|set| set.iter().map(|s| s.as_str()).collect())
            .unwrap_or_default()
    }

    /// List all registered categories.
    pub fn categories(&self) -> Vec<&str> {
        self.permissions.keys().map(|s| s.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_registry_denies_all() {
        let reg = ToolRegistry::new();
        assert!(reg.check_permission("shell", "coding").is_err());
    }

    #[test]
    fn default_registry_allows_known() {
        let reg = ToolRegistry::with_defaults();
        assert!(reg.check_permission("shell", "coding").is_ok());
        assert!(reg.check_permission("calendar", "daily").is_ok());
        assert!(reg.check_permission("pdf", "document").is_ok());
    }

    #[test]
    fn default_registry_denies_cross_category() {
        let reg = ToolRegistry::with_defaults();
        assert!(reg.check_permission("shell", "daily").is_err());
        assert!(reg.check_permission("calendar", "coding").is_err());
    }

    #[test]
    fn custom_permission() {
        let mut reg = ToolRegistry::new();
        reg.allow("custom_cat", &["custom_tool"]);
        assert!(reg.check_permission("custom_tool", "custom_cat").is_ok());
        assert!(reg.check_permission("other_tool", "custom_cat").is_err());
    }

    #[test]
    fn tools_for_category_list() {
        let reg = ToolRegistry::with_defaults();
        let tools = reg.tools_for_category("coding");
        assert!(tools.contains(&"shell"));
        assert!(tools.contains(&"git"));
    }

    #[test]
    fn unknown_category_returns_empty() {
        let reg = ToolRegistry::with_defaults();
        assert!(reg.tools_for_category("nonexistent").is_empty());
    }

    #[test]
    fn categories_list() {
        let reg = ToolRegistry::with_defaults();
        let cats = reg.categories();
        assert!(cats.contains(&"daily"));
        assert!(cats.contains(&"coding"));
        assert_eq!(cats.len(), 9);
    }
}
