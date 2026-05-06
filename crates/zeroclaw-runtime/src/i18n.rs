//! Fluent-based i18n for tool descriptions.
//!
//! English descriptions are embedded via `include_str!` at compile time.
//! Non-English locales are loaded from disk and override English per-key.

use fluent::{FluentArgs, FluentBundle, FluentResource};
use std::collections::HashMap;
use std::sync::OnceLock;

static DESCRIPTIONS: OnceLock<HashMap<String, String>> = OnceLock::new();
static CLI_STRINGS: OnceLock<HashMap<String, String>> = OnceLock::new();
static CLI_FTL_SOURCES: OnceLock<CliFtlSources> = OnceLock::new();
static LOCALE: OnceLock<String> = OnceLock::new();

struct CliFtlSources {
    locale: String,
    disk: Option<String>,
    builtin: Option<&'static str>,
}

/// Initialize with a specific locale. No-op after first call.
pub fn init(locale: &str) {
    let locale = LOCALE.get_or_init(|| normalize_locale(locale));
    DESCRIPTIONS.get_or_init(|| load_descriptions(locale));
    CLI_STRINGS.get_or_init(|| load_cli_strings(locale));
    CLI_FTL_SOURCES.get_or_init(|| load_cli_ftl_sources(locale));
}

/// Get a tool description by tool name (e.g. "shell", "file_read").
pub fn get_tool_description(tool_name: &str) -> Option<&'static str> {
    let map = DESCRIPTIONS.get_or_init(|| load_descriptions(active_locale()));
    let key = format!("tool-{}", tool_name.replace('_', "-"));
    map.get(&key).map(String::as_str)
}

/// Get a CLI string by key (e.g. "cli-config-about").
pub fn get_cli_string(key: &str) -> Option<String> {
    let map = CLI_STRINGS.get_or_init(|| load_cli_strings(active_locale()));
    map.get(key).cloned()
}

/// Get a CLI string by key and format it with Fluent external arguments.
pub fn get_cli_string_with_args(key: &str, args: &[(&str, &str)]) -> Option<String> {
    format_cli_string_with_args(cli_ftl_sources(), key, args)
}

/// Get a required CLI string by key, reporting missing Fluent strings centrally.
pub fn get_required_cli_string(key: &str) -> String {
    get_cli_string(key).unwrap_or_else(|| missing_cli_string(key))
}

/// Get a required CLI string by key and format it with Fluent external arguments.
pub fn get_required_cli_string_with_args(key: &str, args: &[(&str, &str)]) -> String {
    get_cli_string_with_args(key, args).unwrap_or_else(|| missing_cli_string(key))
}

fn active_locale() -> &'static str {
    LOCALE.get_or_init(detect_locale).as_str()
}

fn cli_ftl_sources() -> &'static CliFtlSources {
    CLI_FTL_SOURCES.get_or_init(|| load_cli_ftl_sources(active_locale()))
}

fn missing_cli_string(key: &str) -> String {
    tracing::warn!(
        error_key = "i18n.missing_cli_string",
        key,
        "missing CLI Fluent string"
    );
    format!("{{{key}}}")
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
    if locale != "en" {
        if let Some(locale_ftl) = builtin_cli_ftl_source(locale) {
            map.extend(format_ftl_messages(locale_ftl, locale));
        }
        if let Some(locale_ftl) = load_ftl_from_disk(locale, "cli.ftl") {
            map.extend(format_ftl_messages(&locale_ftl, locale));
        }
    }
    map
}

fn load_cli_ftl_sources(locale: &str) -> CliFtlSources {
    CliFtlSources {
        locale: locale.to_string(),
        disk: (locale != "en")
            .then(|| load_ftl_from_disk(locale, "cli.ftl"))
            .flatten(),
        builtin: (locale != "en")
            .then(|| builtin_cli_ftl_source(locale))
            .flatten(),
    }
}

fn builtin_cli_ftl_source(locale: &str) -> Option<&'static str> {
    match locale {
        "zh-CN" => Some(include_str!("../locales/zh-CN/cli.ftl")),
        _ => None,
    }
}

fn format_cli_string_with_args(
    sources: &CliFtlSources,
    key: &str,
    args: &[(&str, &str)],
) -> Option<String> {
    if let Some(locale_ftl) = sources.disk.as_deref()
        && let Some(value) = format_ftl_message(locale_ftl, &sources.locale, key, args)
    {
        return Some(value);
    }
    if let Some(locale_ftl) = sources.builtin
        && let Some(value) = format_ftl_message(locale_ftl, &sources.locale, key, args)
    {
        return Some(value);
    }
    format_ftl_message(include_str!("../locales/en/cli.ftl"), "en", key, args)
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
    fn cli_string_formats_external_args() {
        let value = format_ftl_message(
            "cli-test = Value { $value }",
            "en",
            "cli-test",
            &[("value", "42")],
        );
        assert_eq!(value.as_deref(), Some("Value 42"));
    }

    #[test]
    fn zh_cn_wechat_translations_preserve_machine_facing_tokens() {
        let zh_cn = include_str!("../locales/zh-CN/cli.ftl");
        let bind = format_ftl_message(
            zh_cn,
            "zh-CN",
            "cli-wechat-send-bind-command",
            &[("command", "/bind")],
        )
        .expect("zh-CN bind command should format");
        assert!(bind.contains("WeChat"));
        assert!(bind.contains("/bind"));
        assert!(bind.contains("<code>"));

        let success = format_ftl_message(zh_cn, "zh-CN", "cli-wechat-bound-success", &[])
            .expect("zh-CN bind success should format");
        assert!(success.contains("WeChat"));
        assert!(success.contains("ZeroClaw"));
    }

    #[test]
    fn zh_cn_cli_strings_load_from_builtin_source() {
        let map = load_cli_strings("zh-CN");
        assert_eq!(
            map.get("cli-wechat-connected").map(String::as_str),
            Some("✅ WeChat 已连接！")
        );

        let sources = load_cli_ftl_sources("zh-CN");
        let value = format_cli_string_with_args(
            &sources,
            "cli-wechat-pairing-required",
            &[("code", "123456")],
        )
        .expect("zh-CN built-in CLI source should format args");
        assert!(value.contains("WeChat"));
        assert!(value.contains("123456"));
        assert!(value.contains("需要绑定"));
    }

    #[test]
    fn argumented_cli_strings_fall_back_from_disk_to_builtin_locale() {
        let sources = CliFtlSources {
            locale: "zh-CN".to_string(),
            disk: Some("cli-wechat-connected = stale workspace override".to_string()),
            builtin: builtin_cli_ftl_source("zh-CN"),
        };

        let overridden = format_cli_string_with_args(&sources, "cli-wechat-connected", &[])
            .expect("disk override should still win when present");
        assert_eq!(overridden, "stale workspace override");

        let built_in = format_cli_string_with_args(
            &sources,
            "cli-wechat-pairing-required",
            &[("code", "123456")],
        )
        .expect("missing disk key should fall back to built-in zh-CN");
        assert!(built_in.contains("123456"));
        assert!(built_in.contains("需要绑定"));
    }

    #[test]
    fn wechat_cli_strings_format_from_fluent() {
        let keys = [
            (
                "cli-wechat-pairing-required",
                &[("code", "123456")][..],
                ["123456"].as_slice(),
            ),
            (
                "cli-wechat-send-bind-command",
                &[("command", "/bind")][..],
                ["WeChat", "/bind", "<code>"].as_slice(),
            ),
            (
                "cli-wechat-qr-login",
                &[("attempt", "1"), ("max", "3")][..],
                ["1", "3"].as_slice(),
            ),
            ("cli-wechat-scan-to-connect", &[][..], ["WeChat"].as_slice()),
            (
                "cli-wechat-qr-url",
                &[("url", "https://example.test/qr")][..],
                ["https://example.test/qr"].as_slice(),
            ),
            (
                "cli-wechat-qr-expired-giving-up",
                &[("max", "3")][..],
                ["3"].as_slice(),
            ),
            ("cli-wechat-qr-fetch-failed", &[][..], ["WeChat"].as_slice()),
            (
                "cli-wechat-qr-fetch-status-failed",
                &[("status", "500"), ("body", "server error")][..],
                ["WeChat", "500", "server error"].as_slice(),
            ),
            (
                "cli-wechat-missing-response-field",
                &[("field", "qrcode")][..],
                ["WeChat", "qrcode"].as_slice(),
            ),
            ("cli-wechat-scanned-confirm", &[][..], [].as_slice()),
            ("cli-wechat-qr-expired-refreshing", &[][..], [].as_slice()),
            (
                "cli-wechat-login-confirmed-missing-field",
                &[("field", "bot_token")][..],
                ["bot_token"].as_slice(),
            ),
            ("cli-wechat-connected", &[][..], ["WeChat"].as_slice()),
            (
                "cli-wechat-bound-success",
                &[][..],
                ["WeChat", "ZeroClaw"].as_slice(),
            ),
            ("cli-wechat-invalid-bind-code", &[][..], [].as_slice()),
        ];
        for source in [
            (include_str!("../locales/en/cli.ftl"), "en"),
            (include_str!("../locales/zh-CN/cli.ftl"), "zh-CN"),
        ] {
            for (key, args, expected_parts) in keys {
                let value = format_ftl_message(source.0, source.1, key, args)
                    .unwrap_or_else(|| panic!("{key} should format in {}", source.1));
                for expected in expected_parts {
                    assert!(
                        value.contains(expected),
                        "{key} in {} should preserve {expected}",
                        source.1
                    );
                }
            }
        }
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
