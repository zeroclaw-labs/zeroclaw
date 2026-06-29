//! Fluent helpers for onboarding-facing strings.
//!
//! Onboarding strings serve every surface (CLI, RPC, web), so they live in
//! their own `onboard.ftl` catalogue, not the CLI one. This crate depends on
//! `zeroclaw-runtime`, so locale detection and the registry are reused from
//! there rather than duplicated.

use fluent::{FluentArgs, FluentBundle, FluentResource};
use std::collections::HashMap;
use std::sync::OnceLock;

static ONBOARD_STRINGS: OnceLock<HashMap<String, String>> = OnceLock::new();
static ONBOARD_FTL_SOURCES: OnceLock<OnboardFtlSources> = OnceLock::new();
static LOCALE: OnceLock<String> = OnceLock::new();

const EN_ONBOARD_FTL: &str = include_str!("../../zeroclaw-runtime/locales/en/onboard.ftl");

struct OnboardFtlSources {
    locale: String,
    disk: Option<String>,
}

pub fn get_onboard_string(key: &str) -> Option<String> {
    let map = ONBOARD_STRINGS.get_or_init(|| load_onboard_strings(active_locale()));
    map.get(key).cloned()
}

pub fn get_required_onboard_string(key: &str) -> String {
    get_onboard_string(key).unwrap_or_else(|| missing_onboard_string(key))
}

pub fn get_required_onboard_string_with_args(key: &str, args: &[(&str, &str)]) -> String {
    format_onboard_string_with_args(onboard_ftl_sources(), key, args)
        .unwrap_or_else(|| missing_onboard_string(key))
}

fn active_locale() -> &'static str {
    LOCALE
        .get_or_init(zeroclaw_runtime::i18n::detect_locale)
        .as_str()
}

fn onboard_ftl_sources() -> &'static OnboardFtlSources {
    ONBOARD_FTL_SOURCES.get_or_init(|| load_onboard_ftl_sources(active_locale()))
}

fn load_onboard_strings(locale: &str) -> HashMap<String, String> {
    let mut map = format_ftl_messages(EN_ONBOARD_FTL, "en");
    if locale != "en"
        && let Some(locale_ftl) = load_ftl_from_disk(locale)
    {
        map.extend(format_ftl_messages(&locale_ftl, locale));
    }
    map
}

fn load_onboard_ftl_sources(locale: &str) -> OnboardFtlSources {
    OnboardFtlSources {
        locale: locale.to_string(),
        disk: (locale != "en")
            .then(|| load_ftl_from_disk(locale))
            .flatten(),
    }
}

fn format_onboard_string_with_args(
    sources: &OnboardFtlSources,
    key: &str,
    args: &[(&str, &str)],
) -> Option<String> {
    if let Some(locale_ftl) = sources.disk.as_deref()
        && let Some(value) = format_ftl_message(locale_ftl, &sources.locale, key, args)
    {
        return Some(value);
    }
    format_ftl_message(EN_ONBOARD_FTL, "en", key, args)
}

fn format_ftl_messages(ftl_source: &str, locale: &str) -> HashMap<String, String> {
    let resource =
        FluentResource::try_new(ftl_source.to_string()).unwrap_or_else(|(resource, _)| resource);
    let language_identifier = locale.parse().unwrap_or_else(|_| "en".parse().unwrap());
    let mut bundle = FluentBundle::new(vec![language_identifier]);
    bundle.set_use_isolating(false);
    let _ = bundle.add_resource(resource);

    let mut map = HashMap::new();
    for line in ftl_source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('-') {
            continue;
        }
        if let Some(identifier) = trimmed.split(" =").next()
            && let Some(message) = bundle.get_message(identifier)
            && let Some(pattern) = message.value()
        {
            let mut errors = vec![];
            let value = bundle.format_pattern(pattern, None, &mut errors);
            if errors.is_empty() {
                map.insert(identifier.to_string(), value.into_owned());
            }
        }
    }
    map
}

fn format_ftl_message(
    ftl_source: &str,
    locale: &str,
    key: &str,
    args: &[(&str, &str)],
) -> Option<String> {
    let resource =
        FluentResource::try_new(ftl_source.to_string()).unwrap_or_else(|(resource, _)| resource);
    let language_identifier = locale.parse().unwrap_or_else(|_| "en".parse().unwrap());
    let mut bundle = FluentBundle::new(vec![language_identifier]);
    bundle.set_use_isolating(false);
    let _ = bundle.add_resource(resource);

    let message = bundle.get_message(key)?;
    let pattern = message.value()?;
    let mut fluent_args = FluentArgs::new();
    for (name, value) in args {
        fluent_args.set(*name, *value);
    }
    let mut errors = vec![];
    let value = bundle.format_pattern(pattern, Some(&fluent_args), &mut errors);
    if errors.is_empty() {
        Some(value.into_owned())
    } else {
        None
    }
}

fn load_ftl_from_disk(locale: &str) -> Option<String> {
    let path = zeroclaw_config::schema::ftl_locale_dir(locale)
        .ok()
        .map(|d| d.join("onboard.ftl"))?;
    std::fs::read_to_string(path).ok()
}

fn missing_onboard_string(key: &str) -> String {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(
                ::serde_json::json!({"error_key": "i18n.missing_onboard_string", "key": key})
            ),
        "missing onboard Fluent string"
    );
    format!("{{{key}}}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn english_onboard_string_with_args(key: &str, args: &[(&str, &str)]) -> String {
        let english = OnboardFtlSources {
            locale: "en".to_string(),
            disk: None,
        };
        format_onboard_string_with_args(&english, key, args)
            .unwrap_or_else(|| missing_onboard_string(key))
    }

    #[test]
    fn locale_prompt_resolves_from_the_onboard_catalogue() {
        let map = format_ftl_messages(EN_ONBOARD_FTL, "en");
        assert!(
            map.contains_key("onboard-flow-locale-prompt"),
            "the locale selector prompt must be in the onboard catalogue"
        );
    }

    #[test]
    fn no_configurable_fields_error_formats_the_section_argument() {
        let value = english_onboard_string_with_args(
            "onboard-flow-no-fields",
            &[("section", "channels.matrix.home")],
        );
        assert!(value.contains("channels.matrix.home"));
    }

    #[test]
    fn completion_message_formats_the_configured_summary() {
        let value = english_onboard_string_with_args(
            "onboard-flow-completed",
            &[("items", "channel:home")],
        );
        assert!(value.contains("channel:home"));
    }

    #[test]
    fn every_locale_catalogue_has_the_same_message_ids() {
        let template = zeroclaw_config::schema::FTL_CATALOGS
            .iter()
            .find(|(name, _, _)| *name == "onboard")
            .map(|(_, path_template, _)| *path_template)
            .expect("onboard catalogue is registered in FTL_CATALOGS");
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root above the crate");

        let mut reference_ids: Option<std::collections::BTreeSet<String>> = None;
        for locale in zeroclaw_runtime::i18n::available_locales() {
            let path = repo_root.join(template.replace("{locale}", &locale.code));
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("missing onboard catalogue for {}", locale.code));
            let ids: std::collections::BTreeSet<String> =
                format_ftl_messages(&source, &locale.code)
                    .into_keys()
                    .collect();
            assert!(!ids.is_empty(), "{} catalogue must define ids", locale.code);
            match &reference_ids {
                None => reference_ids = Some(ids),
                Some(reference) => assert_eq!(
                    &ids, reference,
                    "{} onboard catalogue must carry the same ids as the first locale",
                    locale.code
                ),
            }
        }
        assert!(
            reference_ids.is_some(),
            "registry must list at least one locale"
        );
    }
}
