use super::Skill;
use serde::Deserialize;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Server-side, post-submit install suggestions for cached skill registry metadata.
///
/// This layer intentionally runs before the normal LLM turn and only returns a
/// suggestion. It does not install, enable, read skill bodies, write memory, or
/// provide composer-time suggestions; richer inline UI needs client/protocol
/// support on top of this server-side path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallableSkillCapability {
    name: String,
    source: String,
    description: String,
    aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallSuggestion {
    name: String,
    source: String,
    matched: String,
}

impl InstallSuggestion {
    pub fn render_user_message(&self) -> String {
        let install_command = format!("zeroclaw skills install {}", self.source);
        crate::i18n::get_required_cli_string_with_args(
            "cli-skills-install-suggestion",
            &[
                ("name", &self.name),
                ("matched", &self.matched),
                ("install_command", &install_command),
            ],
        )
    }
}

pub(crate) fn render_missing_skill_install_suggestion(
    prompt: &str,
    installed_skills: &[Skill],
    workspace_dir: &Path,
    enabled: bool,
) -> Option<String> {
    if !enabled || prompt.trim().is_empty() {
        return None;
    }

    let catalog = load_cached_installable_skill_capabilities(workspace_dir);
    suggest_missing_skill_install(prompt, installed_skills, &catalog)
        .map(|suggestion| suggestion.render_user_message())
}

fn suggest_missing_skill_install(
    prompt: &str,
    installed_skills: &[Skill],
    catalog: &[InstallableSkillCapability],
) -> Option<InstallSuggestion> {
    if prompt.trim().is_empty() {
        return None;
    }

    let normalized_prompt = normalize(prompt);
    for capability in catalog {
        if is_installed_skill(capability, installed_skills) {
            continue;
        }
        if let Some(matched) = matched_metadata_phrase(&normalized_prompt, capability) {
            return Some(InstallSuggestion {
                name: capability.name.clone(),
                source: capability.source.clone(),
                matched,
            });
        }
    }

    None
}

fn load_cached_installable_skill_capabilities(
    workspace_dir: &Path,
) -> Vec<InstallableSkillCapability> {
    let skills_dir = workspace_dir.join("skills-registry").join("skills");
    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return Vec::new();
    };

    let mut capabilities = Vec::new();
    for entry in entries.flatten() {
        let skill_dir = entry.path();
        if !skill_dir.is_dir() {
            continue;
        }

        let Some(source) = skill_dir
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
        else {
            continue;
        };

        if let Some(capability) = load_skill_package_metadata(&skill_dir, &source) {
            capabilities.push(capability);
        }
    }

    capabilities.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.source.cmp(&right.source))
    });
    capabilities
}

fn load_skill_package_metadata(
    skill_dir: &Path,
    source: &str,
) -> Option<InstallableSkillCapability> {
    for manifest_name in ["SKILL.toml", "manifest.toml"] {
        let manifest_path = skill_dir.join(manifest_name);
        if manifest_path.exists() {
            return load_toml_skill_package_metadata(&manifest_path, source);
        }
    }

    let markdown_path = skill_dir.join("SKILL.md");
    if markdown_path.exists() {
        return load_markdown_skill_package_metadata(&markdown_path, source);
    }

    None
}

fn load_toml_skill_package_metadata(
    manifest_path: &Path,
    source: &str,
) -> Option<InstallableSkillCapability> {
    let Ok(manifest) = std::fs::read_to_string(manifest_path) else {
        return None;
    };
    let Ok(manifest) = toml::from_str::<RegistrySkillManifest>(&manifest) else {
        tracing::warn!(
            "failed to parse cached registry skill metadata from {}",
            manifest_path.display()
        );
        return None;
    };

    Some(InstallableSkillCapability {
        name: manifest.skill.name,
        source: source.to_string(),
        description: manifest.skill.description,
        aliases: manifest.skill.aliases,
    })
}

fn load_markdown_skill_package_metadata(
    markdown_path: &Path,
    source: &str,
) -> Option<InstallableSkillCapability> {
    let frontmatter = read_markdown_frontmatter(markdown_path)?;
    let meta = super::parse_simple_frontmatter(&frontmatter);
    let description = meta.description.unwrap_or_default();
    Some(InstallableSkillCapability {
        name: meta.name.unwrap_or_else(|| source.to_string()),
        source: source.to_string(),
        description,
        aliases: Vec::new(),
    })
}

fn read_markdown_frontmatter(markdown_path: &Path) -> Option<String> {
    let file = File::open(markdown_path).ok()?;
    let mut lines = BufReader::new(file).lines();
    let first = lines.next()?.ok()?;
    if first.trim() != "---" {
        return None;
    }

    let mut frontmatter = String::new();
    for line in lines {
        let line = line.ok()?;
        if line.trim() == "---" {
            return Some(frontmatter);
        }
        frontmatter.push_str(&line);
        frontmatter.push('\n');
    }
    None
}

#[derive(Debug, Deserialize)]
struct RegistrySkillManifest {
    skill: RegistrySkillMeta,
}

#[derive(Debug, Deserialize)]
struct RegistrySkillMeta {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    aliases: Vec<String>,
}

fn is_installed_skill(capability: &InstallableSkillCapability, installed_skills: &[Skill]) -> bool {
    let capability_name = normalize(&capability.name);
    let capability_source = normalize(&capability.source);
    installed_skills.iter().any(|skill| {
        let skill_name = normalize(&skill.name);
        let plugin_skill_name = skill
            .name
            .strip_prefix("plugin:")
            .and_then(|qualified| qualified.rsplit_once('/').map(|(_, name)| normalize(name)));
        skill_name == capability_name
            || skill_name == capability_source
            || plugin_skill_name
                .as_deref()
                .is_some_and(|name| name == capability_name || name == capability_source)
    })
}

fn matched_metadata_phrase(
    prompt: &str,
    capability: &InstallableSkillCapability,
) -> Option<String> {
    let mut phrases: Vec<String> = capability
        .aliases
        .iter()
        .chain(std::iter::once(&capability.name))
        .map(|phrase| normalize(phrase))
        .filter(|phrase| phrase.len() >= 3)
        .collect();
    phrases.sort_by_key(|phrase| std::cmp::Reverse(phrase.len()));
    phrases
        .into_iter()
        .find(|phrase| contains_phrase(prompt, phrase))
}

fn normalize(input: &str) -> String {
    input
        .split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

fn contains_phrase(haystack: &str, needle: &str) -> bool {
    let haystack_words = haystack.split_whitespace().collect::<Vec<_>>();
    let needle_words = needle.split_whitespace().collect::<Vec<_>>();
    if needle_words.is_empty() {
        return false;
    }
    haystack_words
        .windows(needle_words.len())
        .any(|window| window == needle_words.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed_skill(name: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: "Installed capability".to_string(),
            version: "0.1.0".to_string(),
            author: None,
            tags: vec![],
            tools: vec![],
            prompts: vec![],
            location: None,
        }
    }

    fn catalog_entry(name: &str, aliases: &[&str]) -> InstallableSkillCapability {
        InstallableSkillCapability {
            name: name.to_string(),
            source: name.to_string(),
            description: "Registry metadata description".to_string(),
            aliases: aliases.iter().map(|alias| alias.to_string()).collect(),
        }
    }

    #[test]
    fn installed_capability_proceeds_without_suggestion() {
        let installed = vec![installed_skill("calendar")];
        let catalog = vec![catalog_entry("calendar", &["calendar"])];

        let suggestion = suggest_missing_skill_install(
            "please use calendar to schedule this",
            &installed,
            &catalog,
        );

        assert!(suggestion.is_none());
    }

    #[test]
    fn plugin_shipped_installed_capability_proceeds_without_suggestion() {
        let installed = vec![installed_skill("plugin:my-toolkit/calendar")];
        let catalog = vec![catalog_entry("calendar", &["calendar"])];

        let suggestion = suggest_missing_skill_install(
            "please use calendar to schedule this",
            &installed,
            &catalog,
        );

        assert!(suggestion.is_none());
    }

    #[test]
    fn missing_high_confidence_capability_returns_install_suggestion() {
        let catalog = vec![catalog_entry("calendar", &["calendar", "google calendar"])];

        let suggestion = suggest_missing_skill_install(
            "please use google calendar to schedule this meeting",
            &[],
            &catalog,
        )
        .expect("missing high-confidence skill should suggest installation");

        assert_eq!(suggestion.name, "calendar");
        assert_eq!(suggestion.source, "calendar");
        assert_eq!(suggestion.matched, "google calendar");
        assert!(
            suggestion
                .render_user_message()
                .contains("zeroclaw skills install calendar")
        );
    }

    #[test]
    fn low_confidence_prompt_proceeds_normally() {
        let catalog = vec![catalog_entry("calendar", &["calendar"])];

        let suggestion = suggest_missing_skill_install("summarize the design notes", &[], &catalog);

        assert!(suggestion.is_none());
    }

    #[test]
    fn disabled_config_proceeds_without_reading_registry() {
        let dir = tempfile::tempdir().unwrap();

        let suggestion = render_missing_skill_install_suggestion(
            "use calendar to schedule this",
            &[],
            dir.path(),
            false,
        );

        assert!(suggestion.is_none());
    }

    #[test]
    fn cached_registry_catalog_uses_skill_toml_metadata_without_reading_markdown_body() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("skills-registry/skills/calendar");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.toml"),
            r#"
[skill]
name = "calendar"
description = "Schedule meetings and inspect availability"
version = "0.1.0"
aliases = ["google calendar"]
tags = ["scheduling"]
"#,
        )
        .unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "This body-only secret phrase must not be used for matching.",
        )
        .unwrap();

        let catalog = load_cached_installable_skill_capabilities(dir.path());

        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name, "calendar");
        assert_eq!(catalog[0].source, "calendar");
        assert!(catalog[0].description.contains("Schedule meetings"));

        let body_only_match = suggest_missing_skill_install(
            "please use body only secret phrase for this",
            &[],
            &catalog,
        );
        assert!(body_only_match.is_none());

        let suggestion = render_missing_skill_install_suggestion(
            "please use google calendar to schedule this meeting",
            &[],
            dir.path(),
            true,
        )
        .expect("cached registry metadata should render a suggestion");
        assert!(suggestion.contains("calendar"));
        assert!(suggestion.contains("zeroclaw skills install calendar"));
        assert!(!suggestion.contains("body-only secret phrase"));
        assert!(!dir.path().join("skills").exists());
    }

    #[test]
    fn cached_registry_catalog_supports_manifest_toml_packages() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("skills-registry/skills/release-check");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("manifest.toml"),
            r#"
[skill]
name = "release-check"
description = "Check release readiness"
aliases = ["release check"]
"#,
        )
        .unwrap();

        let catalog = load_cached_installable_skill_capabilities(dir.path());
        let suggestion = suggest_missing_skill_install(
            "please run a release check before tagging",
            &[],
            &catalog,
        );

        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].source, "release-check");
        assert!(suggestion.is_some());
    }

    #[test]
    fn cached_registry_catalog_supports_markdown_frontmatter_without_body_matching() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("skills-registry/skills/screenshot-helper");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            r#"---
name: screenshot-helper
description: Capture screenshots
tags: [browser]
---

This body-only browser automation phrase must not be used for matching.
"#,
        )
        .unwrap();

        let catalog = load_cached_installable_skill_capabilities(dir.path());
        let suggestion =
            suggest_missing_skill_install("please use screenshot helper here", &[], &catalog);
        let body_only_match = suggest_missing_skill_install(
            "please use browser automation phrase here",
            &[],
            &catalog,
        );

        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name, "screenshot-helper");
        assert!(suggestion.is_some());
        assert!(body_only_match.is_none());
    }
}
