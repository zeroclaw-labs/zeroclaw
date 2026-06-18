//! Canonical `SKILL.md` frontmatter.
//!
//! Per the open Agent Skills spec (agentskills.io), `name` and `description`
//! are required; everything else is conventional. We keep the shape **flat**
//! — `license`, `author`, `version`, `category` at the top level — so the
//! existing hand-rolled parser in `super::parse_simple_frontmatter` (which
//! deliberately avoids a full YAML dep) covers every field. The
//! `zeroclaw-labs/zeroclaw-skills` registry nests these under a `metadata:`
//! block; that registry is ours and follows this flat shape going forward.
//!
//! The struct is the single source of truth: [`SkillFrontmatter::prop_fields`]
//! enumerates the same field set that drives the dashboard form, CLI flags
//! on `zeroclaw skills add`, and the TUI form. Adding a field here = all
//! three surfaces gain it via `prop_fields`.

use serde::{Deserialize, Serialize};
use zeroclaw_config::traits::{PropFieldInfo, PropKind};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Free-form tags from the YAML `tags:` list. Drive skill tiering and
    /// opt-in surfaces — notably the `slash` tag, which exposes the skill as a
    /// Discord slash command (zeroclaw-labs/zeroclaw#7490). Loader-managed tags
    /// such as `open-skills` also live here. Round-tripped so editing a skill no
    /// longer silently strips its tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// When `true`, this skill's instructions should always be injected into the
    /// system prompt, even in compact prompt mode. Use sparingly for critical
    /// skills that must be followed at all times.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub always: bool,
}

impl SkillFrontmatter {
    /// Field set in canonical order. Surfaces iterate this to build flag
    /// lists / forms / pickers. Drift-checked by `prop_fields_matches_struct`.
    pub fn prop_fields() -> Vec<PropFieldInfo> {
        vec![
            field(
                "name",
                "String",
                true,
                "Skill identifier (lowercase, hyphens only).",
            ),
            field(
                "description",
                "String",
                true,
                "What the skill does and when to use it. Written in third person; injected into the system prompt for skill discovery.",
            ),
            field(
                "license",
                "Option<String>",
                false,
                "SPDX license identifier (e.g. MIT).",
            ),
            field(
                "author",
                "Option<String>",
                false,
                "Skill author handle or organisation.",
            ),
            field(
                "version",
                "Option<String>",
                false,
                "SemVer version of the skill. Defaults to 0.1.0 on scaffold.",
            ),
            field(
                "category",
                "Option<String>",
                false,
                "Skill category for registry grouping (e.g. coding, ops).",
            ),
            PropFieldInfo {
                name: "tags".to_string(),
                category: "skill-frontmatter",
                display_value: String::new(),
                type_hint: "Vec<String>",
                kind: PropKind::StringArray,
                is_secret: false,
                enum_variants: None,
                description: "Free-form tags. The `slash` tag opts the skill into Discord slash commands (zeroclaw-labs/zeroclaw#7490); others drive tiering / registry grouping.",
                derived_from_secret: false,
                credential_class: None,
                tab: zeroclaw_config::config::ConfigTab::None,
                alias_source: None,
            },
            field(
                "always",
                "bool",
                false,
                "When true, always inject this skill's instructions even in compact prompt mode.",
            ),
        ]
    }
}

fn field(
    name: &'static str,
    type_hint: &'static str,
    required: bool,
    description: &'static str,
) -> PropFieldInfo {
    PropFieldInfo {
        name: name.to_string(),
        category: "skill-frontmatter",
        display_value: if required {
            String::from("<required>")
        } else {
            String::new()
        },
        type_hint,
        kind: PropKind::String,
        is_secret: false,
        enum_variants: None,
        description,
        derived_from_secret: false,
        credential_class: None,
        tab: zeroclaw_config::config::ConfigTab::None,
        alias_source: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prop_fields_matches_struct() {
        // Drift check: when a field is added to SkillFrontmatter, prop_fields
        // must be updated to match. The expected count tracks every field.
        let fields = SkillFrontmatter::prop_fields();
        assert_eq!(
            fields.len(),
            8,
            "SkillFrontmatter::prop_fields drifted from struct definition; \
             update both when adding/removing fields"
        );
    }

    #[test]
    fn serializes_minimal_skill_without_optional_fields() {
        let fm = SkillFrontmatter {
            name: "code-review".into(),
            description: "Review pull requests.".into(),
            ..Default::default()
        };
        let json = serde_json::to_value(&fm).unwrap();
        assert_eq!(json["name"], "code-review");
        assert_eq!(json["description"], "Review pull requests.");
        assert!(json.get("license").is_none());
        assert!(json.get("author").is_none());
    }
}
