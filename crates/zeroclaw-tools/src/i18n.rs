//! Fluent helpers for tool-facing strings.
//!
//! `zeroclaw-runtime` also reads `tools.ftl`, but this crate cannot depend on
//! runtime because runtime depends on tool implementations. Keep this helper
//! local and narrow so tools can route their own schemas and results through
//! the same catalogue without reversing the crate graph.

use fluent::{FluentArgs, FluentBundle, FluentResource};
use std::collections::HashMap;
use std::sync::OnceLock;

static TOOL_STRINGS: OnceLock<HashMap<String, String>> = OnceLock::new();
static TOOL_FTL_SOURCES: OnceLock<ToolFtlSources> = OnceLock::new();
static LOCALE: OnceLock<String> = OnceLock::new();

const EN_TOOLS_FTL: &str = include_str!("../../zeroclaw-runtime/locales/en/tools.ftl");

struct ToolFtlSources {
    locale: String,
    disk: Option<String>,
}

pub(crate) fn get_tool_string(key: &str) -> Option<String> {
    let map = TOOL_STRINGS.get_or_init(|| load_tool_strings(active_locale()));
    map.get(key).cloned()
}

pub(crate) fn get_required_tool_string(key: &str) -> String {
    get_tool_string(key).unwrap_or_else(|| missing_tool_string(key))
}

pub(crate) fn get_required_tool_string_with_args(key: &str, args: &[(&str, &str)]) -> String {
    format_tool_string_with_args(tool_ftl_sources(), key, args)
        .unwrap_or_else(|| missing_tool_string(key))
}

fn active_locale() -> &'static str {
    LOCALE.get_or_init(detect_locale).as_str()
}

fn tool_ftl_sources() -> &'static ToolFtlSources {
    TOOL_FTL_SOURCES.get_or_init(|| load_tool_ftl_sources(active_locale()))
}

fn load_tool_strings(locale: &str) -> HashMap<String, String> {
    let mut map = format_ftl_messages(EN_TOOLS_FTL, "en");
    if locale != "en"
        && let Some(locale_ftl) = load_ftl_from_disk(locale)
    {
        map.extend(format_ftl_messages(&locale_ftl, locale));
    }
    map
}

fn load_tool_ftl_sources(locale: &str) -> ToolFtlSources {
    ToolFtlSources {
        locale: locale.to_string(),
        disk: (locale != "en")
            .then(|| load_ftl_from_disk(locale))
            .flatten(),
    }
}

fn format_tool_string_with_args(
    sources: &ToolFtlSources,
    key: &str,
    args: &[(&str, &str)],
) -> Option<String> {
    if let Some(locale_ftl) = sources.disk.as_deref()
        && let Some(value) = format_ftl_message(locale_ftl, &sources.locale, key, args)
    {
        return Some(value);
    }
    format_ftl_message(EN_TOOLS_FTL, "en", key, args)
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
        .map(|d| d.join("tools.ftl"))?;
    std::fs::read_to_string(path).ok()
}

fn missing_tool_string(key: &str) -> String {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({"error_key": "i18n.missing_tool_string", "key": key})),
        "missing tool Fluent string"
    );
    format!("{{{key}}}")
}

fn detect_locale() -> String {
    if let Ok(custom) = std::env::var("ZEROCLAW_CONFIG_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            let path = std::path::PathBuf::from(trimmed).join("config.toml");
            if let Some(locale) = std::fs::read_to_string(path)
                .ok()
                .and_then(|c| c.parse::<toml::Table>().ok())
                .and_then(locale_from_table)
            {
                return locale;
            }
        }
    }

    if let Some(base) = directories::BaseDirs::new() {
        let candidates = [
            base.home_dir().join(".zeroclaw/config.toml"),
            base.config_dir().join("zeroclaw/config.toml"),
        ];
        for path in candidates {
            if let Some(locale) = std::fs::read_to_string(path)
                .ok()
                .and_then(|c| c.parse::<toml::Table>().ok())
                .and_then(locale_from_table)
            {
                return locale;
            }
        }
    }

    locale_from_system_env().unwrap_or_else(|| "en".to_string())
}

/// Auto-detect locale from the host environment when config sets none.
/// Same gettext-style precedence as `zeroclaw-runtime`'s `i18n::detect_locale`:
/// `ZEROCLAW_LOCALE` first, then the POSIX locale variables in their usual
/// precedence order (`LC_ALL` > `LC_MESSAGES` > `LANG`), falling back to an
/// OS-level locale query for platforms (notably native Windows consoles)
/// that don't set any of those env vars.
fn locale_from_system_env() -> Option<String> {
    ["ZEROCLAW_LOCALE", "LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|var| {
            std::env::var(var)
                .ok()
                .and_then(|v| normalized_env_locale(&v))
        })
        .or_else(locale_from_os)
}

/// Cross-platform OS locale query (Windows `GetUserDefaultLocaleName`, macOS
/// `NSLocale`, Linux env vars) via `sys-locale` — catches hosts that skip the
/// POSIX locale env vars entirely.
fn locale_from_os() -> Option<String> {
    sys_locale::get_locale().and_then(|raw| normalized_env_locale(&raw))
}

/// Pure: normalize a raw env-var locale value, rejecting the POSIX
/// "no locale configured" sentinels ("", "C", "POSIX").
fn normalized_env_locale(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("c")
        || trimmed.eq_ignore_ascii_case("posix")
    {
        return None;
    }
    Some(normalize_locale(trimmed))
}

fn locale_from_table(table: toml::Table) -> Option<String> {
    let locale = table.get("locale")?.as_str()?.trim();
    (!locale.is_empty()).then(|| normalize_locale(locale))
}

fn normalize_locale(raw: &str) -> String {
    raw.split('.').next().unwrap_or(raw).replace('_', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_env_locale_rejects_posix_sentinels() {
        assert_eq!(normalized_env_locale(""), None);
        assert_eq!(normalized_env_locale("   "), None);
        assert_eq!(normalized_env_locale("C"), None);
        assert_eq!(normalized_env_locale("posix"), None);
    }

    #[test]
    fn normalized_env_locale_normalizes_posix_format() {
        assert_eq!(
            normalized_env_locale("zh_CN.UTF-8"),
            Some("zh-CN".to_string())
        );
        assert_eq!(normalized_env_locale(" ja "), Some("ja".to_string()));
    }

    #[test]
    fn file_download_schema_strings_are_in_tools_catalogue() {
        assert!(
            get_required_tool_string("tool-file-download-param-document-id").contains("document")
        );
        let value = get_required_tool_string_with_args(
            "tool-file-download-error-status",
            &[("status", "404 Not Found")],
        );
        assert!(value.contains("404 Not Found"));
    }
}
