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
//! on `zeroclaw skills add`, and the TUI form. Adding a flat scalar field here
//! = all three surfaces gain it via `prop_fields`.
//!
//! The one exception is `slash_options`: a nested `slash_options:` YAML list
//! (typed Discord slash-command parameters). It is the reason this file keeps
//! a flat *scalar* schema while still expressing structured options — the
//! nesting is parsed/serialized by the shared helper in [`super::document`],
//! not the flat parser, and it is deliberately excluded from `prop_fields`
//! (the flat form can't render a nested list; the dashboard gets a bespoke
//! editor for it).

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
    /// Free-form tags from the YAML `tags:` list. Drive skill tiering and
    /// opt-in surfaces — notably the `slash` tag, which exposes the skill as a
    /// Discord slash command (zeroclaw-labs/zeroclaw#7490). Loader-managed tags
    /// such as `open-skills` also live here. Round-tripped so editing a skill no
    /// longer silently strips its tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Typed slash-command options a `slash`-tagged skill exposes (Discord
    /// dropdowns / ranges / autocomplete). The one genuinely *nested* field —
    /// a `slash_options:` YAML list of maps, each with an optional `choices`
    /// sub-list. It is parsed/serialized by the shared nested helper in
    /// [`super::document`] (the flat scalar parser leaves it untouched) and is
    /// deliberately excluded from [`SkillFrontmatter::prop_fields`] because the
    /// flat dashboard/CLI/TUI form can't express nesting; the dashboard gets a
    /// bespoke editor for it instead. See [`SkillSlashOption`].
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prop_fields_matches_struct() {
        // Drift check: when a FLAT scalar field is added to SkillFrontmatter,
        // prop_fields must be updated to match. The struct has 8 fields, but
        // `slash_options` is INTENTIONALLY excluded from prop_fields — it is a
        // nested list the flat dashboard/CLI/TUI form can't render, so it gets
        // a bespoke editor instead. Hence 7 flat fields surfaced here.
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
