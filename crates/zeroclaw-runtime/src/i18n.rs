//! Fluent-based i18n for tool descriptions.
//!
//! English descriptions are embedded via `include_str!` at compile time.
//! Non-English locales are loaded from disk and override English per-key.

use fluent::{FluentBundle, FluentResource};
use std::collections::HashMap;
use std::sync::OnceLock;

static DESCRIPTIONS: OnceLock<HashMap<String, String>> = OnceLock::new();
static CLI_STRINGS: OnceLock<HashMap<String, String>> = OnceLock::new();

/// Initialize with a specific locale. No-op after first call.
pub fn init(locale: &str) {
    DESCRIPTIONS.get_or_init(|| load_descriptions(locale));
    CLI_STRINGS.get_or_init(|| load_cli_strings(locale));
}

/// Get a tool description by tool name (e.g. "shell", "file_read").
pub fn get_tool_description(tool_name: &str) -> Option<&'static str> {
    let map = DESCRIPTIONS.get_or_init(|| load_descriptions(&detect_locale()));
    let key = format!("tool-{}", tool_name.replace('_', "-"));
    map.get(&key).map(String::as_str)
}

/// Get a CLI string by key (e.g. "cli-config-about").
pub fn get_cli_string(key: &str) -> Option<String> {
    let map = CLI_STRINGS.get_or_init(|| load_cli_strings(&detect_locale()));
    map.get(key).cloned()
}

fn load_descriptions(locale: &str) -> HashMap<String, String> {
    let mut map = format_ftl_messages(include_str!("../locales/en/tools.ftl"), "en");
    if locale != "en"
        && let Some(locale_ftl) = load_ftl_from_disk(locale, "tools.ftl")
    {
        map.extend(format_ftl_messages(&locale_ftl, locale));
    }
    map
}

fn load_cli_strings(locale: &str) -> HashMap<String, String> {
    let mut map = format_ftl_messages(include_str!("../locales/en/cli.ftl"), "en");
    if locale != "en"
        && let Some(locale_ftl) = load_ftl_from_disk(locale, "cli.ftl")
    {
        map.extend(format_ftl_messages(&locale_ftl, locale));
    }
    map
}

fn format_ftl_messages(ftl_source: &str, locale: &str) -> HashMap<String, String> {
    let resource =
        FluentResource::try_new(ftl_source.to_string()).unwrap_or_else(|(resource, _)| resource);
    let language_identifier = locale.parse().unwrap_or_else(|_| "en".parse().unwrap());
    let mut bundle = FluentBundle::new(vec![language_identifier]);
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
            map.insert(identifier.to_string(), value.into_owned());
        }
    }
    map
}

fn load_ftl_from_disk(locale: &str, filename: &str) -> Option<String> {
    let workspace_path =
        workspace_dir_from_config().map(|d| d.join("locales").join(locale).join(filename));
    let search_paths = [workspace_path];
    for path in search_paths.into_iter().flatten() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            tracing::debug!(path = %path.display(), "loaded locale FTL from disk");
            return Some(content);
        }
    }
    None
}

/// Detect locale: config.toml → "en".
pub fn detect_locale() -> String {
    locale_from_config().unwrap_or_else(|| "en".to_string())
}

fn read_config_table() -> Option<toml::Table> {
    let base = directories::BaseDirs::new()?;
    let candidates = [
        base.home_dir().join(".zeroclaw/config.toml"),
        base.config_dir().join("zeroclaw/config.toml"),
    ];
    for path in &candidates {
        if let Ok(contents) = std::fs::read_to_string(path) {
            return contents.parse().ok();
        }
    }
    None
}

fn locale_from_config() -> Option<String> {
    let table = read_config_table()?;
    let locale = table.get("locale")?.as_str()?.trim().to_string();
    if locale.is_empty() {
        return None;
    }
    Some(normalize_locale(&locale))
}

fn workspace_dir_from_config() -> Option<std::path::PathBuf> {
    if let Some(dir) = read_config_table()
        .as_ref()
        .and_then(|t| t.get("workspace_dir"))
        .and_then(|v| v.as_str())
    {
        return Some(std::path::PathBuf::from(dir));
    }
    Some(
        directories::BaseDirs::new()?
            .home_dir()
            .join(".zeroclaw/workspace"),
    )
}

/// Normalize "zh_CN.UTF-8" → "zh-CN".
pub fn normalize_locale(raw: &str) -> String {
    raw.split('.').next().unwrap_or(raw).replace('_', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn english_descriptions_are_embedded() {
        let map = format_ftl_messages(include_str!("../locales/en/tools.ftl"), "en");
        assert!(map.contains_key("tool-shell"));
        assert!(map.contains_key("tool-file-read"));
        assert!(!map.contains_key("tool-nonexistent"));
    }

    #[test]
    fn unknown_locale_falls_back_to_english() {
        let map = load_descriptions("xx-FAKE");
        assert!(map.contains_key("tool-shell"));
    }

    #[test]
    fn normalize_locale_strips_encoding() {
        assert_eq!(normalize_locale("en_US.UTF-8"), "en-US");
        assert_eq!(normalize_locale("zh_CN.utf8"), "zh-CN");
        assert_eq!(normalize_locale("fr"), "fr");
    }

    #[test]
    fn detect_locale_defaults_to_en_without_config() {
        // Locale is config-only. Without a config.toml present, must return "en".
        assert_eq!(detect_locale(), "en");
    }
}
