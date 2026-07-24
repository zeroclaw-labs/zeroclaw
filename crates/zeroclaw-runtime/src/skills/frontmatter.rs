//! Canonical `SKILL.md` frontmatter.

use serde::{Deserialize, Serialize};
use zeroclaw_config::traits::{PropFieldInfo, PropKind};

use super::SkillSlashOption;

// `Eq` is intentionally NOT derived: `slash_options` carries `SkillSlashOption`,
// whose `min`/`max` bounds are `f64` (no total ordering). `PartialEq` is all the
// surfaces (tests, change detection) need.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slash_options: Vec<SkillSlashOption>,
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
                multiline: false,
            },
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
        multiline: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prop_fields_matches_struct() {
        let fields = SkillFrontmatter::prop_fields();
        assert_eq!(
            fields.len(),
            7,
            "SkillFrontmatter::prop_fields drifted from struct definition; \
             update both when adding/removing FLAT fields (slash_options is \
             nested and deliberately excluded)"
        );
        // slash_options must never sneak into the flat form.
        assert!(
            !fields.iter().any(|f| f.name == "slash_options"),
            "slash_options is nested and must stay out of the flat prop_fields form"
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
