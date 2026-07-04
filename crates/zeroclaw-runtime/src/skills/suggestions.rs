use super::{EXTRA_REGISTRY_DIR_PREFIX, SKILLS_REGISTRY_DIR_NAME, Skill};
use serde::Deserialize;
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use zeroclaw_config::schema::ExternalRegistryKind;

#[cfg(feature = "plugins-wasm")]
use zeroclaw_plugins::registry::{
    install_command as plugin_install_command, read_cached_registry_index,
};

/// Server-side, post-submit install suggestions for cached skill/plugin registry metadata.
///
/// This layer intentionally runs before the normal LLM turn and only returns a
/// suggestion. It does not install, enable, read skill bodies, write memory, or
/// provide composer-time suggestions; richer inline UI needs client/protocol
/// support on top of this server-side path.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallableSkillCapability {
    name: String,
    source: String,
    aliases: Vec<String>,
    install_kind: InstallKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum InstallKind {
    Skill,
    #[cfg(feature = "plugins-wasm")]
    Plugin,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallSuggestion {
    name: String,
    source: String,
    matched: String,
    install_kind: InstallKind,
}

impl InstallSuggestion {
    pub fn render_user_message(&self) -> String {
        let (message_key, install_command) = match self.install_kind {
            InstallKind::Skill => (
                "cli-skills-install-suggestion",
                format!("zeroclaw skills install {}", self.source),
            ),
            #[cfg(feature = "plugins-wasm")]
            InstallKind::Plugin => ("cli-plugin-install-suggestion", self.source.clone()),
        };
        crate::i18n::get_required_cli_string_with_args(
            message_key,
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
    installed_runtime_capabilities: &[&str],
    workspace_dir: &Path,
    extra_registries: &[zeroclaw_config::schema::ExternalRegistry],
    enabled: bool,
) -> Option<String> {
    if !enabled || prompt.trim().is_empty() {
        return None;
    }

    let catalog = load_cached_installable_skill_capabilities(workspace_dir, extra_registries);
    let skill_suggestion = suggest_missing_skill_install(
        prompt,
        installed_skills,
        installed_runtime_capabilities,
        &catalog,
    );
    if let Some(suggestion) = skill_suggestion {
        return Some(suggestion.render_user_message());
    }

    #[cfg(feature = "plugins-wasm")]
    {
        let catalog = load_cached_installable_plugin_capabilities(workspace_dir);
        suggest_missing_skill_install(
            prompt,
            installed_skills,
            installed_runtime_capabilities,
            &catalog,
        )
        .map(|suggestion| suggestion.render_user_message())
    }

    #[cfg(not(feature = "plugins-wasm"))]
    None
}

fn suggest_missing_skill_install(
    prompt: &str,
    installed_skills: &[Skill],
    installed_runtime_capabilities: &[&str],
    catalog: &[InstallableSkillCapability],
) -> Option<InstallSuggestion> {
    if prompt.trim().is_empty() {
        return None;
    }

    let normalized_prompt = normalize(prompt);
    let installed_runtime_capabilities =
        normalized_runtime_capabilities(installed_runtime_capabilities);
    for capability in catalog {
        if is_installed_skill(capability, installed_skills)
            || is_installed_runtime_capability(capability, &installed_runtime_capabilities)
        {
            continue;
        }
        if let Some(matched) = matched_metadata_phrase(&normalized_prompt, capability) {
            return Some(InstallSuggestion {
                name: capability.name.clone(),
                source: capability.source.clone(),
                matched,
                install_kind: capability.install_kind,
            });
        }
    }

    None
}

fn load_cached_installable_skill_capabilities(
    workspace_dir: &Path,
    extra_registries: &[zeroclaw_config::schema::ExternalRegistry],
) -> Vec<InstallableSkillCapability> {
    let mut capabilities = Vec::new();
    let skills_dir = workspace_dir.join(SKILLS_REGISTRY_DIR_NAME).join("skills");
    load_cached_registry_skill_capabilities(&skills_dir, None, &mut capabilities);

    for registry in extra_registries
        .iter()
        .filter(|registry| registry.enabled && registry.kind == ExternalRegistryKind::Git)
    {
        if !zeroclaw_config::schema::ExternalRegistry::is_valid_name(&registry.name) {
            continue;
        }
        let skills_dir = workspace_dir
            .join(format!("{}{}", EXTRA_REGISTRY_DIR_PREFIX, registry.name))
            .join("skills");
        load_cached_registry_skill_capabilities(
            &skills_dir,
            Some(&registry.name),
            &mut capabilities,
        );
    }

    capabilities.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.source.cmp(&right.source))
    });
    capabilities
}

fn load_cached_registry_skill_capabilities(
    skills_dir: &Path,
    registry_name: Option<&str>,
    capabilities: &mut Vec<InstallableSkillCapability>,
) {
    let Ok(entries) = std::fs::read_dir(skills_dir) else {
        return;
    };

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

        let skill_name = source;
        let source = match registry_name {
            Some(registry_name) => {
                if !super::is_registry_source(&skill_name) {
                    continue;
                }
                format!("registry:{registry_name}/{skill_name}")
            }
            None => skill_name.clone(),
        };

        if let Some(capability) = load_skill_package_metadata(&skill_dir, &source, &skill_name) {
            capabilities.push(capability);
        }
    }
}

fn load_skill_package_metadata(
    skill_dir: &Path,
    source: &str,
    fallback_name: &str,
) -> Option<InstallableSkillCapability> {
    for manifest_name in ["SKILL.toml", "manifest.toml"] {
        let manifest_path = skill_dir.join(manifest_name);
        if manifest_path.exists() {
            return load_toml_skill_package_metadata(&manifest_path, source);
        }
    }

    let markdown_path = skill_dir.join("SKILL.md");
    if markdown_path.exists() {
        return load_markdown_skill_package_metadata(&markdown_path, source, fallback_name);
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
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"path": manifest_path.display().to_string()})),
            "failed to parse cached registry skill metadata"
        );
        return None;
    };

    Some(InstallableSkillCapability {
        name: manifest.skill.name,
        source: source.to_string(),
        aliases: manifest.skill.aliases,
        install_kind: InstallKind::Skill,
    })
}

fn load_markdown_skill_package_metadata(
    markdown_path: &Path,
    source: &str,
    fallback_name: &str,
) -> Option<InstallableSkillCapability> {
    let frontmatter = read_markdown_frontmatter(markdown_path)?;
    let meta = super::parse_simple_frontmatter(&frontmatter);
    Some(InstallableSkillCapability {
        name: meta.name.unwrap_or_else(|| fallback_name.to_string()),
        source: source.to_string(),
        aliases: Vec::new(),
        install_kind: InstallKind::Skill,
    })
}

#[cfg(feature = "plugins-wasm")]
fn load_cached_installable_plugin_capabilities(
    workspace_dir: &Path,
) -> Vec<InstallableSkillCapability> {
    let index = match read_cached_registry_index(workspace_dir) {
        Ok(Some(index)) => index,
        Ok(None) => return Vec::new(),
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"error": error.to_string()})),
                "failed to parse cached plugin registry metadata"
            );
            return Vec::new();
        }
    };
    let registry_url = index.registry_url.as_deref();

    index
        .plugins
        .into_iter()
        .map(|entry| {
            let description = entry.description.clone().unwrap_or_default();
            InstallableSkillCapability {
                name: entry.name.clone(),
                source: plugin_install_command(&entry, registry_url),
                aliases: vec![description],
                install_kind: InstallKind::Plugin,
            }
        })
        .collect()
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

fn normalized_runtime_capabilities(installed_runtime_capabilities: &[&str]) -> HashSet<String> {
    installed_runtime_capabilities
        .iter()
        .map(|capability| normalize(capability))
        .filter(|capability| !capability.is_empty())
        .collect()
}

fn is_installed_runtime_capability(
    capability: &InstallableSkillCapability,
    installed_runtime_capabilities: &HashSet<String>,
) -> bool {
    std::iter::once(&capability.name)
        .chain(std::iter::once(&capability.source))
        .chain(capability.aliases.iter())
        .map(|name| normalize(name))
        .filter(|name| !name.is_empty())
        .any(|name| installed_runtime_capabilities.contains(&name))
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
            description_localizations: Default::default(),
            version: "0.1.0".to_string(),
            author: None,
            tags: vec![],
            tools: vec![],
            prompts: vec![],
            slash_options: Vec::new(),
            location: None,
        }
    }

    fn catalog_entry(name: &str, aliases: &[&str]) -> InstallableSkillCapability {
        InstallableSkillCapability {
            name: name.to_string(),
            source: name.to_string(),
            aliases: aliases.iter().map(|alias| alias.to_string()).collect(),
            install_kind: InstallKind::Skill,
        }
    }

    fn extra_registry(name: &str, enabled: bool) -> zeroclaw_config::schema::ExternalRegistry {
        extra_registry_with_kind(name, "git", enabled)
    }

    fn extra_registry_with_kind(
        name: &str,
        kind: &str,
        enabled: bool,
    ) -> zeroclaw_config::schema::ExternalRegistry {
        zeroclaw_config::schema::ExternalRegistry {
            name: name.to_string(),
            url: format!("file:///tmp/{name}"),
            kind: kind.to_string().into(),
            enabled,
        }
    }

    #[test]
    fn installed_capability_proceeds_without_suggestion() {
        let installed = vec![installed_skill("calendar")];
        let catalog = vec![catalog_entry("calendar", &["calendar"])];

        let suggestion = suggest_missing_skill_install(
            "please use calendar to schedule this",
            &installed,
            &[],
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
            &[],
            &catalog,
        );

        assert!(suggestion.is_none());
    }

    #[test]
    fn installed_runtime_capability_proceeds_without_suggestion() {
        let catalog = vec![catalog_entry("calendar", &["google calendar"])];

        let suggestion = suggest_missing_skill_install(
            "please use google calendar to schedule this meeting",
            &[],
            &["google calendar"],
            &catalog,
        );

        assert!(suggestion.is_none());
    }

    #[test]
    fn installed_runtime_capability_matches_normalized_tool_names() {
        let catalog = vec![catalog_entry("calendar", &["google calendar"])];

        for runtime_capability in ["google_calendar", "google-calendar"] {
            let suggestion = suggest_missing_skill_install(
                "please use google calendar to schedule this meeting",
                &[],
                &[runtime_capability],
                &catalog,
            );

            assert!(
                suggestion.is_none(),
                "{runtime_capability} should suppress the install suggestion"
            );
        }
    }

    #[test]
    fn unrelated_runtime_capability_does_not_suppress_suggestion() {
        let catalog = vec![catalog_entry("calendar", &["google calendar"])];

        let suggestion = suggest_missing_skill_install(
            "please use google calendar to schedule this meeting",
            &[],
            &["google_workspace"],
            &catalog,
        );

        assert!(
            suggestion.is_some(),
            "unrelated runtime capabilities must not suppress missing-skill suggestions"
        );
    }

    #[test]
    fn missing_high_confidence_capability_returns_install_suggestion() {
        let catalog = vec![catalog_entry("calendar", &["calendar", "google calendar"])];

        let suggestion = suggest_missing_skill_install(
            "please use google calendar to schedule this meeting",
            &[],
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

        let suggestion =
            suggest_missing_skill_install("summarize the design notes", &[], &[], &catalog);

        assert!(suggestion.is_none());
    }

    #[test]
    fn disabled_config_proceeds_without_reading_registry() {
        let dir = tempfile::tempdir().unwrap();

        let suggestion = render_missing_skill_install_suggestion(
            "use calendar to schedule this",
            &[],
            &[],
            dir.path(),
            &[],
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

        let catalog = load_cached_installable_skill_capabilities(dir.path(), &[]);

        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name, "calendar");
        assert_eq!(catalog[0].source, "calendar");

        let body_only_match = suggest_missing_skill_install(
            "please use body only secret phrase for this",
            &[],
            &[],
            &catalog,
        );
        assert!(body_only_match.is_none());

        let suggestion = render_missing_skill_install_suggestion(
            "please use google calendar to schedule this meeting",
            &[],
            &[],
            dir.path(),
            &[],
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

        let catalog = load_cached_installable_skill_capabilities(dir.path(), &[]);
        let suggestion = suggest_missing_skill_install(
            "please run a release check before tagging",
            &[],
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

        let catalog = load_cached_installable_skill_capabilities(dir.path(), &[]);
        let suggestion =
            suggest_missing_skill_install("please use screenshot helper here", &[], &[], &catalog);
        let body_only_match = suggest_missing_skill_install(
            "please use browser automation phrase here",
            &[],
            &[],
            &catalog,
        );

        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name, "screenshot-helper");
        assert!(suggestion.is_some());
        assert!(body_only_match.is_none());
    }

    #[test]
    fn cached_registry_catalog_includes_enabled_extra_registries() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("extra-registry-acme/skills/team-calendar");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.toml"),
            r#"
[skill]
name = "team-calendar"
description = "Schedule meetings on the team calendar"
aliases = ["team calendar"]
"#,
        )
        .unwrap();

        let catalog =
            load_cached_installable_skill_capabilities(dir.path(), &[extra_registry("acme", true)]);
        let suggestion = suggest_missing_skill_install(
            "please use the team calendar to schedule this",
            &[],
            &[],
            &catalog,
        )
        .expect("enabled cached extra registry metadata should suggest installation");

        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].name, "team-calendar");
        assert_eq!(catalog[0].source, "registry:acme/team-calendar");
        assert_eq!(suggestion.source, "registry:acme/team-calendar");
        assert!(
            suggestion
                .render_user_message()
                .contains("zeroclaw skills install registry:acme/team-calendar")
        );
    }

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn cached_plugin_registry_metadata_returns_plugin_install_suggestion() {
        let dir = tempfile::tempdir().unwrap();
        let registry_dir = dir.path().join("plugin-registry");
        std::fs::create_dir_all(&registry_dir).unwrap();
        std::fs::write(
            registry_dir.join("registry.json"),
            r#"
{
  "registry_url": "https://example.invalid/registry.json",
  "plugins": [
    {
      "name": "team-calendar",
      "version": "0.2.0",
      "description": "Schedule meetings on the team calendar",
      "capabilities": ["tool"],
      "url": "https://example.invalid/team-calendar-0.2.0.zip"
    }
  ]
}
"#,
        )
        .unwrap();

        let suggestion = render_missing_skill_install_suggestion(
            "please use the team calendar to schedule this",
            &[],
            &[],
            dir.path(),
            &[],
            true,
        )
        .expect("cached plugin registry metadata should suggest plugin installation");

        assert!(suggestion.contains("team-calendar"));
        assert!(suggestion.contains("team calendar"));
        assert!(suggestion.contains(
            "zeroclaw plugin install team-calendar@0.2.0 --registry https://example.invalid/registry.json"
        ));
    }

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn skill_registry_metadata_takes_precedence_over_plugin_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills-registry/skills/team-calendar");
        std::fs::create_dir_all(&skills_dir).unwrap();
        std::fs::write(
            skills_dir.join("SKILL.toml"),
            r#"
[skill]
name = "team-calendar"
description = "Schedule meetings on the team calendar"
aliases = ["team calendar"]
"#,
        )
        .unwrap();
        let registry_dir = dir.path().join("plugin-registry");
        std::fs::create_dir_all(&registry_dir).unwrap();
        std::fs::write(
            registry_dir.join("registry.json"),
            r#"
{
  "plugins": [
    {
      "name": "team-calendar-plugin",
      "version": "0.2.0",
      "description": "Schedule meetings on the team calendar",
      "capabilities": ["tool"],
      "url": "https://example.invalid/team-calendar-0.2.0.zip"
    }
  ]
}
"#,
        )
        .unwrap();

        let suggestion = render_missing_skill_install_suggestion(
            "please use the team calendar to schedule this",
            &[],
            &[],
            dir.path(),
            &[],
            true,
        )
        .expect("skill registry metadata should suggest installation first");

        assert!(suggestion.contains("zeroclaw skills install"));
        assert!(!suggestion.contains("zeroclaw plugin install"));
    }

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn cached_plugin_registry_metadata_does_not_suggest_installed_runtime_capability() {
        let dir = tempfile::tempdir().unwrap();
        let registry_dir = dir.path().join("plugin-registry");
        std::fs::create_dir_all(&registry_dir).unwrap();
        std::fs::write(
            registry_dir.join("registry.json"),
            r#"
{
  "plugins": [
    {
      "name": "team-calendar",
      "version": "0.2.0",
      "description": "Schedule meetings on the team calendar",
      "capabilities": ["tool"],
      "url": "https://example.invalid/team-calendar-0.2.0.zip"
    }
  ]
}
"#,
        )
        .unwrap();

        let suggestion = render_missing_skill_install_suggestion(
            "please use the team calendar to schedule this",
            &[],
            &["team_calendar"],
            dir.path(),
            &[],
            true,
        );

        assert!(suggestion.is_none());
    }

    #[cfg(feature = "plugins-wasm")]
    #[test]
    fn disabled_config_does_not_read_cached_plugin_registry() {
        let dir = tempfile::tempdir().unwrap();
        let registry_dir = dir.path().join("plugin-registry");
        std::fs::create_dir_all(&registry_dir).unwrap();
        std::fs::write(registry_dir.join("registry.json"), "{ not json").unwrap();

        let suggestion = render_missing_skill_install_suggestion(
            "please use the team calendar to schedule this",
            &[],
            &[],
            dir.path(),
            &[],
            false,
        );

        assert!(suggestion.is_none());
    }

    #[test]
    fn cached_registry_catalog_skips_disabled_extra_registries() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("extra-registry-acme/skills/team-calendar");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            r#"---
description: Team calendar scheduling
---
"#,
        )
        .unwrap();

        let catalog = load_cached_installable_skill_capabilities(
            dir.path(),
            &[extra_registry("acme", false)],
        );

        assert!(catalog.is_empty());
    }

    #[test]
    fn cached_registry_catalog_skips_non_git_extra_registries() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("extra-registry-acme/skills/team-calendar");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.toml"),
            r#"
[skill]
name = "team-calendar"
description = "Schedule meetings on the team calendar"
aliases = ["team calendar"]
"#,
        )
        .unwrap();

        let catalog = load_cached_installable_skill_capabilities(
            dir.path(),
            &[extra_registry_with_kind("acme", "http", true)],
        );

        assert!(catalog.is_empty());
    }

    #[test]
    fn cached_registry_catalog_skips_invalid_extra_registry_skill_sources() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("extra-registry-acme/skills/team.calendar");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.toml"),
            r#"
[skill]
name = "team-calendar"
description = "Schedule meetings on the team calendar"
aliases = ["team calendar"]
"#,
        )
        .unwrap();

        let catalog =
            load_cached_installable_skill_capabilities(dir.path(), &[extra_registry("acme", true)]);
        let suggestion =
            suggest_missing_skill_install("please use the team calendar", &[], &[], &catalog);

        assert!(catalog.is_empty());
        assert!(suggestion.is_none());
    }

    /// Regression: a capability in the raw registry but absent from the
    /// effective tool set must not suppress install suggestions.
    ///
    /// Before the fix, `process_message` built `runtime_capability_names`
    /// from the raw `tools_registry` (all registered tools regardless of
    /// exclusion). A tool excluded for the current turn was still treated
    /// as "installed", causing `suggest_missing_skill_install` to skip the
    /// suggestion. Using `effective_tool_names` instead ensures that only
    /// tools available for this turn suppress suggestions.
    ///
    /// This test demonstrates the two outcomes:
    /// - passing the raw name suppresses the suggestion (old behavior);
    /// - omitting it (as the effective set does) returns the suggestion.
    #[test]
    fn excluded_tool_does_not_suppress_missing_skill_suggestion() {
        let catalog = vec![catalog_entry("calendar", &["calendar", "google calendar"])];

        // With the excluded tool in runtime capabilities (old behavior: raw
        // registry), the suggestion is suppressed because "calendar" is
        // treated as already installed.
        let suppressed = suggest_missing_skill_install(
            "please use google calendar to schedule this meeting",
            &[],
            &["calendar"],
            &catalog,
        );
        assert!(
            suppressed.is_none(),
            "raw registry including excluded tool should suppress suggestion — this is the old bug"
        );

        // Without the excluded tool in runtime capabilities (new behavior:
        // effective tool set), the suggestion is returned because "calendar"
        // is not considered available for this turn.
        let suggestion = suggest_missing_skill_install(
            "please use google calendar to schedule this meeting",
            &[],
            &["shell", "file_read"],
            &catalog,
        );
        assert!(
            suggestion.is_some(),
            "effective tool set excluding the capability must return the suggestion"
        );
        let suggestion = suggestion.expect("suggestion should be present");
        assert_eq!(suggestion.name, "calendar");
        assert!(suggestion.render_user_message().contains("calendar"));
    }
}
