use crate::api::{AgentSkillEntry, SkillDocument};
use crate::i18n::t;
use dioxus::prelude::*;

fn origin_label(skill: &AgentSkillEntry) -> String {
    match skill.origin.as_str() {
        "plugin" => skill
            .plugin
            .as_deref()
            .map(|p| format!("plugin:{p}"))
            .unwrap_or_else(|| "plugin".to_string()),
        "bundle" => skill.bundle.clone().unwrap_or_else(|| "bundle".to_string()),
        other => other.to_string(),
    }
}

#[component]
pub fn SkillCard(
    skill: AgentSkillEntry,
    is_expanded: bool,
    detail: Option<SkillDocument>,
    on_expand: Option<Callback<AgentSkillEntry>>,
) -> Element {
    let can_expand = skill.editable && on_expand.is_some();
    let skill_for_click = skill.clone();
    let edit_href = if skill.editable {
        skill.bundle.as_deref().map(|bundle| {
            format!(
                "/config/skill_bundles/{}?tab=skills&skill={}",
                crate::api::urlencode(bundle),
                crate::api::urlencode(&skill.name)
            )
        })
    } else {
        None
    };
    let shadowed = skill.shadowed.as_ref().filter(|s| !s.is_empty());
    let shadows_label = t("skills.shadows");
    let shadowed_origins = shadowed.map(|s| {
        s.iter()
            .map(|x| x.origin.clone())
            .collect::<Vec<_>>()
            .join(", ")
    });
    let shadowed_detail = shadowed
        .map(|s| {
            s.iter()
                .map(|x| format!("{}:{}", x.origin, x.name))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();

    rsx! {
        div { class: "card skill-card",
            if can_expand {
                button {
                    class: "card-body expandable",
                    onclick: move |_| {
                        if let Some(cb) = on_expand.as_ref() { cb.call(skill_for_click.clone()); }
                    },
                    div { class: "card-header",
                        div { class: "card-title",
                            svg {
                                class: "card-icon",
                                "xmlns": "http://www.w3.org/2000/svg",
                                width: "16",
                                height: "16",
                                "viewBox": "0 0 24 24",
                                fill: "none",
                                stroke: "var(--pc-accent)",
                                "stroke-width": "2",
                                "stroke-linecap": "round",
                                "stroke-linejoin": "round",
                                path { d: "M2 3h6a4 4 0 0 1 4 4v14a3 3 0 0 0-3-3H2z" },
                                path { d: "M22 3h-6a4 4 0 0 0-4 4v14a3 3 0 0 1 3-3h7z" },
                            }
                            h3 { class: "skill-name", "{skill.name}" }
                        }
                        span { class: if is_expanded { "chevron expanded" } else { "chevron" } }
                    }
                    if !skill.description.is_empty() {
                        p { class: "skill-description", "{skill.description}" }
                    }
                }
            } else {
                div { class: "card-body",
                    div { class: "card-header",
                        div { class: "card-title",
                            svg {
                                class: "card-icon",
                                "xmlns": "http://www.w3.org/2000/svg",
                                width: "16",
                                height: "16",
                                "viewBox": "0 0 24 24",
                                fill: "none",
                                stroke: "var(--pc-accent)",
                                "stroke-width": "2",
                                "stroke-linecap": "round",
                                "stroke-linejoin": "round",
                                path { d: "M2 3h6a4 4 0 0 1 4 4v14a3 3 0 0 0-3-3H2z" },
                                path { d: "M22 3h-6a4 4 0 0 0-4 4v14a3 3 0 0 1 3-3h7z" },
                            }
                            h3 { class: "skill-name", "{skill.name}" }
                        }
                    }
                    if !skill.description.is_empty() {
                        p { class: "skill-description", "{skill.description}" }
                    }
                }
            }
            div { class: "card-meta",
                span { class: "origin-label", "{origin_label(&skill)}" }
                if let Some(origins) = &shadowed_origins {
                    span {
                        class: "shadowed-badge",
                        title: "{shadowed_detail}",
                        "{shadows_label} {origins}"
                    }
                }                if let Some(href) = edit_href {
                    a { class: "edit-link", href: "{href}", "{t(\"common.edit\")}" }
                }
            }
            if can_expand && is_expanded {
                div { class: "card-detail",
                    if let Some(d) = &detail {
                        if let Some(version) = &d.frontmatter.version {
                            div { class: "detail-version", "v{version}" }
                        }
                        if let Some(author) = &d.frontmatter.author {
                            div { class: "detail-author", "{author}" }
                        }
                        if !d.body.is_empty() {
                            p { class: "detail-label", "{t(\"skills.skill_md\")}" }
                            pre { class: "skill-body", "{d.body}" }
                        }
                    }
                }
            }
        }
    }
}
