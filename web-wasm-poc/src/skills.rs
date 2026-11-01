use std::collections::HashMap;

use dioxus::prelude::*;

use crate::api::{self, AgentSkillEntry, DroppedSkillEntry, SkillDocument};
use crate::i18n::t;
use crate::skill_card::SkillCard;

fn skill_key(skill: &AgentSkillEntry) -> String {
    if skill.editable {
        if let Some(bundle) = &skill.bundle {
            return format!("{bundle}/{}", skill.name);
        }
    }
    format!(
        "{}:{}/{}",
        skill.origin,
        skill.plugin.as_deref().unwrap_or(""),
        skill.name
    )
}

async fn fetch_skills(
    alias: &str,
) -> Result<(Vec<AgentSkillEntry>, Vec<DroppedSkillEntry>), String> {
    let response = api::agent_skills(alias).await?;
    Ok((response.skills, response.dropped))
}

#[component]
pub fn SkillsPage() -> Element {
    let mut agents: Signal<Vec<String>> = use_signal(Vec::new);
    let mut selected_alias: Signal<String> = use_signal(String::new);
    let mut skills: Signal<Vec<AgentSkillEntry>> = use_signal(Vec::new);
    let mut dropped: Signal<Vec<DroppedSkillEntry>> = use_signal(Vec::new);
    let mut search: Signal<String> = use_signal(String::new);
    let mut loading: Signal<bool> = use_signal(|| true);
    let mut reloading: Signal<bool> = use_signal(|| false);
    let mut error: Signal<Option<String>> = use_signal(|| None);
    let mut expanded_key: Signal<Option<String>> = use_signal(|| None);
    let mut detail_map: Signal<HashMap<String, SkillDocument>> = use_signal(HashMap::new);

    use_effect(move || {
        spawn(async move {
            match api::agent_options().await {
                Ok(response) => {
                    let first = response.agents.first().cloned().unwrap_or_default();
                    let is_empty = response.agents.is_empty();
                    agents.set(response.agents);
                    if !first.is_empty() && selected_alias().is_empty() {
                        selected_alias.set(first);
                    } else if is_empty {
                        loading.set(false);
                    }
                }
                Err(e) => {
                    error.set(Some(e));
                    loading.set(false);
                }
            }
        });
    });

    use_effect(move || {
        let alias = selected_alias();
        if alias.is_empty() {
            return;
        }
        spawn(async move {
            loading.set(true);
            error.set(None);
            expanded_key.set(None);
            match fetch_skills(&alias).await {
                Ok((new_skills, new_dropped)) => {
                    if selected_alias() == alias {
                        skills.set(new_skills);
                        dropped.set(new_dropped);
                    }
                }
                Err(e) => {
                    if selected_alias() == alias {
                        error.set(Some(e));
                    }
                }
            }
            if selected_alias() == alias {
                loading.set(false);
            }
        });
    });

    let query = search().to_lowercase();
    let filtered: Vec<AgentSkillEntry> = skills()
        .iter()
        .filter(|s| {
            s.name.to_lowercase().contains(&query)
                || s.description.to_lowercase().contains(&query)
                || s.origin.to_lowercase().contains(&query)
                || s.bundle
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&query)
                || s.plugin
                    .as_deref()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&query)
        })
        .cloned()
        .collect();

    if loading() {
        return rsx! {
            div { class: "page loading",
                div { class: "spinner" }
            }
        };
    }

    let title = t("skills.title");
    let count = filtered.len();
    let dropped_list = dropped();
    let dropped_count = dropped_list.len();
    let skipped = t("skills.skipped_count");

    rsx! {
        div { class: "page",
            div { class: "header-row",
                div { class: "header-controls",
                    select {
                        class: "input-electric",
                        "aria-label": t("skills.agent"),
                        title: t("skills.agent"),
                        value: "{selected_alias}",
                        onchange: move |e| selected_alias.set(e.value()),
                        for agent in agents().iter() {
                            option { key: "{agent}", value: "{agent}", "{agent}" }
                        }
                    }
                    input {
                        class: "input-electric search-input",
                        "type": "text",
                        "aria-label": t("skills.search"),
                        title: t("skills.search"),
                        value: "{search}",
                        placeholder: t("skills.search"),
                        oninput: move |e| search.set(e.value()),
                    }
                }
                button {
                    class: "btn-electric",
                    disabled: reloading(),
                    title: t("skills.reload"),
                    onclick: move |_| {
                        let alias = selected_alias();
                        if alias.is_empty() { return; }
                        spawn(async move {
                            reloading.set(true);
                            expanded_key.set(None);
                            detail_map.write().clear();
                            match fetch_skills(&alias).await {
                                Ok((new_skills, new_dropped)) => {
                                    if selected_alias() == alias {
                                        skills.set(new_skills);
                                        dropped.set(new_dropped);
                                        error.set(None);
                                    }
                                }
                                Err(e) => error.set(Some(e)),
                            }
                            reloading.set(false);
                        });
                    },
                    span { class: if reloading() { "spin-icon spinning" } else { "spin-icon" } }
                    "{t(\"skills.reload\")}"
                }
            }

            if let Some(err) = error() {
                div { class: "error-banner", "{t(\"skills.load_error\")}: {err}" }
            }

            div { class: "section-header",
                span { class: "section-title", "{title} ({count})" }
            }

            if !dropped_list.is_empty() {
                div { class: "dropped-banner",
                    "{dropped_count} {skipped}"
                    ul {
                        for entry in dropped_list.iter() {
                            li {
                                key: "{entry.origin}/{entry.name}",
                                span { class: "mono", "{entry.name}" }
                                " ({entry.origin}) — {entry.reason}"
                            }
                        }
                    }
                }
            }

            if filtered.is_empty() && dropped_list.is_empty() {
                p { class: "empty-state", "{t(\"skills.empty\")}" }
            }

            div { class: "skill-grid",
                for skill in filtered.iter().cloned() {
                    SkillCardItem {
                        key: "{skill.origin}/{skill.name}",
                        skill,
                        expanded_key,
                        detail_map,
                    }
                }
            }
        }
    }
}

#[component]
fn SkillCardItem(
    skill: AgentSkillEntry,
    mut expanded_key: Signal<Option<String>>,
    mut detail_map: Signal<HashMap<String, SkillDocument>>,
) -> Element {
    let key = skill_key(&skill);
    let is_expanded = expanded_key() == Some(key.clone());
    let detail = detail_map().get(&key).cloned();
    let key_for_toggle = key.clone();

    rsx! {
        SkillCard {
            key: "{key}",
            skill,
            is_expanded,
            detail,
            on_expand: move |s: AgentSkillEntry| {
                let k = skill_key(&s);
                if expanded_key() == Some(k.clone()) {
                    expanded_key.set(None);
                } else {
                    expanded_key.set(Some(key_for_toggle.clone()));
                    if !detail_map().contains_key(&key_for_toggle) {
                        let key_for_spawn = key_for_toggle.clone();
                        spawn(async move {
                            let bundle = s.bundle.clone().unwrap_or_default();
                            if let Ok(doc) = api::read_skill(&bundle, &s.name).await {
                                detail_map.insert(key_for_spawn, doc);
                            }
                        });
                    }
                }
            },
        }
    }
}
