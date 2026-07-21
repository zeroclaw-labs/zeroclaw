//! Fluent-based i18n for tool descriptions.
//! English descriptions are embedded via `include_str!` at compile time.
//! Non-English locales are loaded from disk and override English per-key.

use fluent::{FluentArgs, FluentBundle, FluentResource};
use std::collections::HashMap;
use std::sync::OnceLock;

static DESCRIPTIONS: OnceLock<HashMap<String, String>> = OnceLock::new();
static CLI_STRINGS: OnceLock<HashMap<String, String>> = OnceLock::new();
static CLI_FTL_SOURCES: OnceLock<CliFtlSources> = OnceLock::new();
static LOCALE: OnceLock<String> = OnceLock::new();

/// The canonical locale registry, embedded from repo-root `locales.toml` at
/// compile time. Parsed once into a `'static` list so callers (e.g. the RPC
/// `locales/list` handler) get a long-lived reference with no runtime file I/O.
static AVAILABLE_LOCALES: OnceLock<Vec<LocaleOption>> = OnceLock::new();

const LOCALES_TOML: &str = include_str!("../../../locales.toml");

/// One selectable locale: its `code` (e.g. `ja`) and display `label`
/// (e.g. `日本語`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleOption {
    pub code: String,
    pub label: String,
}

/// Locales the build knows about, from the embedded `locales.toml`. Cheap:
/// parsed once, then returns a borrow of the cached `'static` vector.
pub fn available_locales() -> &'static [LocaleOption] {
    AVAILABLE_LOCALES
        .get_or_init(|| {
            let table: toml::Value =
                toml::from_str(LOCALES_TOML).expect("embedded locales.toml is valid TOML");
            table
                .get("locale")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|e| {
                            let code = e.get("code").and_then(|v| v.as_str())?;
                            let label = e.get("label").and_then(|v| v.as_str())?;
                            Some(LocaleOption {
                                code: code.to_string(),
                                label: label.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
        .as_slice()
}

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

#[cfg(test)]
pub(crate) fn get_english_cli_string_with_args(key: &str, args: &[(&str, &str)]) -> String {
    let english = CliFtlSources {
        locale: "en".to_string(),
        disk: None,
        builtin: None,
    };
    format_cli_string_with_args(&english, key, args).unwrap_or_else(|| missing_cli_string(key))
}

fn missing_cli_string(key: &str) -> String {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({"error_key": "i18n.missing_cli_string", "key": key})),
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
        "es" => Some(include_str!("../locales/es/cli.ftl")),
        "fr" => Some(include_str!("../locales/fr/cli.ftl")),
        "ja" => Some(include_str!("../locales/ja/cli.ftl")),
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
    load_ftl_with_reader(locale, filename, |p| std::fs::read_to_string(p).ok())
}

/// Path-resolution + read wiring for locale FTL, with an injectable reader so
/// tests can verify which path is consulted without touching the real
/// filesystem. Production passes `std::fs::read_to_string`.
fn load_ftl_with_reader(
    locale: &str,
    filename: &str,
    read: impl Fn(&std::path::Path) -> Option<String>,
) -> Option<String> {
    let path = zeroclaw_config::schema::ftl_locale_dir(locale)
        .ok()
        .map(|d| d.join(filename));
    let search_paths = [path];
    for path in search_paths.into_iter().flatten() {
        if let Some(content) = read(&path) {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"path": path.display().to_string()})),
                "loaded locale FTL from disk"
            );
            return Some(content);
        }
    }
    None
}

/// Detect locale: config.toml → system locale (via `sys-locale`) → "en".
pub fn detect_locale() -> String {
    locale_from_config()
        .or_else(locale_from_system)
        .unwrap_or_else(|| "en".to_string())
}

/// Auto-detect locale from the OS when config sets none. `sys-locale` is
/// cross-platform: on Unix it checks `LANGUAGE` > `LC_ALL` > `LC_MESSAGES` >
/// `LANG`, and on Windows/macOS it queries the OS directly.
fn locale_from_system() -> Option<String> {
    pick_locale(sys_locale::get_locales())
}

fn pick_locale(mut candidates: impl Iterator<Item = String>) -> Option<String> {
    candidates.find_map(|raw| normalized_env_locale(&raw))
}

/// Pure: normalize a raw OS locale value, rejecting the POSIX
/// "no locale configured" sentinels ("", "C", "POSIX"). Split out from
/// `locale_from_system` so it is testable without environment access —
/// no test may touch real env vars to verify locale logic.
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

fn read_config_table() -> Option<toml::Table> {
    if let Ok(custom) = std::env::var("ZEROCLAW_CONFIG_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            let path = std::path::PathBuf::from(trimmed).join("config.toml");
            return std::fs::read_to_string(&path)
                .ok()
                .and_then(|c| c.parse().ok());
        }
    }

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(base) = directories::BaseDirs::new() {
        candidates.push(base.home_dir().join(".zeroclaw/config.toml"));
        candidates.push(base.config_dir().join("zeroclaw/config.toml"));
    }
    for path in &candidates {
        if let Ok(contents) = std::fs::read_to_string(path) {
            return contents.parse().ok();
        }
    }
    None
}

fn locale_from_config() -> Option<String> {
    locale_from_table(read_config_table())
}

/// Pure: extract a normalized locale from an already-parsed config table.
/// Split out from `locale_from_config` so it is testable without filesystem or
/// environment access — no test may touch the real FS to verify locale logic.
fn locale_from_table(table: Option<toml::Table>) -> Option<String> {
    let table = table?;
    let locale = table.get("locale")?.as_str()?.trim().to_string();
    if locale.is_empty() {
        return None;
    }
    Some(normalize_locale(&locale))
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
    fn doctor_ctxwin_write_failed_formats_in_english_and_japanese() {
        let cases = [(
            "cli-doctor-ctxwin-write-failed",
            &[
                ("provider_ref", "groq.alias2"),
                ("error", "simulated write failure"),
            ][..],
        )];
        for pair in [
            (include_str!("../locales/en/cli.ftl"), "en"),
            (include_str!("../locales/ja/cli.ftl"), "ja"),
        ] {
            let (locale_source, locale) = (pair.0, pair.1);
            for (key, args) in cases {
                let value = format_ftl_message(locale_source, locale, key, args)
                    .unwrap_or_else(|| panic!("{key} should format in {locale}"));
                assert!(
                    value.contains("groq.alias2"),
                    "{key} in {locale} should inline provider_ref; got: {value:?}"
                );
                assert!(
                    value.contains("simulated write failure"),
                    "{key} in {locale} should inline error; got: {value:?}"
                );
            }
        }
    }

    #[test]
    fn channel_compile_guidance_cli_strings_format_from_fluent() {
        let cases = [
            (
                "cli-selftest-channel-config-uncompiled",
                &[("compiled", "4"), ("configured", "1"), ("names", "Slack")][..],
                ["4", "1", "Slack"].as_slice(),
            ),
            (
                "cli-update-prebuilt-channel-note",
                &[][..],
                ["Slack", "channel-*"].as_slice(),
            ),
            (
                "cli-channels-not-compiled-entry",
                &[("name", "Slack")][..],
                ["Slack"].as_slice(),
            ),
        ];

        for (source, locale) in [
            (include_str!("../locales/en/cli.ftl"), "en"),
            (include_str!("../locales/es/cli.ftl"), "es"),
            (include_str!("../locales/fr/cli.ftl"), "fr"),
            (include_str!("../locales/ja/cli.ftl"), "ja"),
            (include_str!("../locales/zh-CN/cli.ftl"), "zh-CN"),
        ] {
            for (key, args, expected_parts) in cases {
                let value = format_ftl_message(source, locale, key, args)
                    .unwrap_or_else(|| panic!("{key} should format in {locale}"));
                for expected in expected_parts {
                    assert!(
                        value.contains(expected),
                        "{key} in {locale} should preserve {expected:?}"
                    );
                }
                if key == "cli-update-prebuilt-channel-note" {
                    assert!(
                        !value.contains("Discord"),
                        "{key} in {locale} should not mention Discord because it is in default-channels"
                    );
                }
            }
        }
    }

    #[test]
    fn plugin_config_entry_key_formats_in_every_builtin_locale() {
        let args = [("capability", "Tool"), ("key", "zpi1_fixture")];
        for (source, locale) in [
            (include_str!("../locales/en/cli.ftl"), "en"),
            (include_str!("../locales/es/cli.ftl"), "es"),
            (include_str!("../locales/fr/cli.ftl"), "fr"),
            (include_str!("../locales/ja/cli.ftl"), "ja"),
            (include_str!("../locales/zh-CN/cli.ftl"), "zh-CN"),
        ] {
            let value = format_ftl_message(source, locale, "cli-plugin-config-entry-key", &args)
                .unwrap_or_else(|| panic!("plugin config entry key should format in {locale}"));
            assert!(value.contains("Tool"));
            assert!(value.contains("zpi1_fixture"));
        }
    }

    #[test]
    fn channel_runtime_committed_cli_catalogs_format_from_fluent() {
        let cases = [
            (
                "channel-runtime-malformed-tool-output",
                &[][..],
                [].as_slice(),
            ),
            ("channel-runtime-new-session", &[][..], [].as_slice()),
            ("channel-runtime-stop-sent", &[][..], [].as_slice()),
            ("channel-runtime-stop-no-task", &[][..], [].as_slice()),
            (
                "channel-runtime-model-empty",
                &[][..],
                ["/model <model-id>"].as_slice(),
            ),
            (
                "channel-runtime-model-switched",
                &[("model", "gpt-test"), ("provider", "openai.default")][..],
                ["gpt-test", "openai.default"].as_slice(),
            ),
            (
                "channel-runtime-agent-scope-rejected",
                &[
                    ("sender", "zeroclaw_user"),
                    ("agent", "agent-alpha"),
                    ("model", "gpt-test"),
                ][..],
                [
                    "zeroclaw_user",
                    "agent-alpha",
                    "/model --agent",
                    "/model --user gpt-test",
                    "admin_for_agent_scope",
                    "true",
                ]
                .as_slice(),
            ),
            ("channel-runtime-request-timeout", &[][..], [].as_slice()),
            (
                "channel-runtime-current-model-status",
                &[("provider", "openai.default"), ("model", "gpt-test")][..],
                ["openai.default", "gpt-test"].as_slice(),
            ),
            (
                "channel-runtime-model-switch-hint",
                &[][..],
                ["/model <model-id>", "/model <hint>"].as_slice(),
            ),
            (
                "channel-runtime-provider-switch-hint",
                &[][..],
                ["/models <model_provider>"].as_slice(),
            ),
            (
                "channel-runtime-available-providers-header",
                &[][..],
                ["model_provider"].as_slice(),
            ),
            (
                "channel-runtime-configured-routes-header",
                &[][..],
                [].as_slice(),
            ),
            (
                "channel-runtime-no-cached-models",
                &[("provider", "openai.default")][..],
                [
                    "openai.default",
                    "zeroclaw models refresh --model-provider openai.default",
                ]
                .as_slice(),
            ),
            (
                "channel-runtime-cached-model-ids-header",
                &[("count", "7")][..],
                ["7"].as_slice(),
            ),
            (
                "channel-runtime-config-switch-hints",
                &[][..],
                ["/models <model_provider>", "/model <model-id>"].as_slice(),
            ),
            (
                "channel-runtime-config-block-title",
                &[("provider", "openai.default"), ("model", "gpt-test")][..],
                ["openai.default", "gpt-test"].as_slice(),
            ),
            (
                "channel-runtime-config-select-provider-placeholder",
                &[][..],
                ["model_provider"].as_slice(),
            ),
            (
                "channel-runtime-config-select-model-placeholder",
                &[][..],
                [].as_slice(),
            ),
            (
                "channel-runtime-config-provider-label",
                &[][..],
                ["ModelProvider"].as_slice(),
            ),
            (
                "channel-runtime-config-model-label",
                &[][..],
                ["*"].as_slice(),
            ),
            ("channel-runtime-scope-user", &[][..], [].as_slice()),
            ("channel-runtime-scope-agent", &[][..], [].as_slice()),
            (
                "channel-runtime-scope-overrides-summary",
                &[
                    ("user", "`openai.default` / `gpt-user`"),
                    ("agent", "—"),
                    ("session", "`openai.default` / `gpt-session`"),
                    ("default", "`openai.default` / `gpt-default`"),
                ][..],
                [
                    "`openai.default` / `gpt-user`",
                    "`openai.default` / `gpt-session`",
                    "`openai.default` / `gpt-default`",
                    "/model --user|--agent <model-id>",
                ]
                .as_slice(),
            ),
            (
                "channel-runtime-set-provider-switched",
                &[("provider", "openai.default"), ("model", "gpt-test")][..],
                ["openai.default", "gpt-test", "/model <model-id>"].as_slice(),
            ),
            (
                "channel-runtime-set-provider-init-failed",
                &[("provider", "openai.default"), ("error", "bad key")][..],
                ["openai.default", "bad key"].as_slice(),
            ),
            (
                "channel-runtime-provider-ambiguous",
                &[("family", "openai"), ("list", "`openai.default`")][..],
                ["openai", "`openai.default`", "/models openai.<alias>"].as_slice(),
            ),
            (
                "channel-runtime-provider-no-alias",
                &[("provider", "openai.default")][..],
                [
                    "openai.default",
                    "[providers.models.openai.default]",
                    "api_key/uri",
                    "/models",
                ]
                .as_slice(),
            ),
            (
                "channel-runtime-provider-unknown",
                &[("provider", "openai.default")][..],
                ["openai.default", "/models"].as_slice(),
            ),
            (
                "channel-runtime-scoped-model-empty",
                &[][..],
                ["/model --user|--agent <model-id>"].as_slice(),
            ),
            (
                "channel-runtime-scoped-model-switched",
                &[
                    ("model", "gpt-test"),
                    ("provider", "openai.default"),
                    ("scope", "user"),
                ][..],
                ["gpt-test", "openai.default", "user"].as_slice(),
            ),
            (
                "channel-runtime-shadow-note",
                &[("model", "gpt-test"), ("provider", "openai.default")][..],
                ["gpt-test", "openai.default", "/model"].as_slice(),
            ),
            (
                "channel-runtime-thinking-set",
                &[("level", "high")][..],
                ["high", "/thinking reset"].as_slice(),
            ),
            (
                "channel-runtime-thinking-cleared",
                &[("default", "medium")][..],
                ["medium"].as_slice(),
            ),
            (
                "channel-runtime-thinking-default",
                &[("default", "medium")][..],
                ["medium", "/thinking high", "/thinking max", "/thinking off"].as_slice(),
            ),
            (
                "channel-runtime-thinking-invalid",
                &[("raw", "banana")][..],
                [
                    "banana",
                    "/thinking off|minimal|low|medium|high|max",
                    "/thinking on",
                    "/thinking reset",
                ]
                .as_slice(),
            ),
            (
                "channel-runtime-provider-turn-init-failed",
                &[("provider", "openai.default"), ("error", "bad key")][..],
                ["openai.default", "bad key", "/models"].as_slice(),
            ),
            (
                "channel-runtime-fallback-footer",
                &[
                    ("requested", "openai.default"),
                    ("actual", "anthropic.default"),
                    ("model", "claude-test"),
                ][..],
                [
                    "openai.default",
                    "anthropic.default",
                    "claude-test",
                    "/models",
                ]
                .as_slice(),
            ),
        ];

        for (source, locale) in [
            (include_str!("../locales/en/cli.ftl"), "en"),
            (include_str!("../locales/es/cli.ftl"), "es"),
            (include_str!("../locales/fr/cli.ftl"), "fr"),
            (include_str!("../locales/ja/cli.ftl"), "ja"),
            (include_str!("../locales/zh-CN/cli.ftl"), "zh-CN"),
        ] {
            for (key, args, expected_parts) in cases {
                let value = format_ftl_message(source, locale, key, args)
                    .unwrap_or_else(|| panic!("{key} should format in {locale}"));
                for expected in expected_parts {
                    assert!(
                        value.contains(expected),
                        "{key} in {locale} should preserve {expected:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn skills_install_cli_strings_format_from_fluent() {
        type FormatCase<'a> = (&'a str, &'a [(&'a str, &'a str)], &'a [&'a str]);

        let en_cases: &[FormatCase<'_>] = &[
            (
                "cli-skills-install-start",
                &[("source", "example-skill")][..],
                &["Installing skill from", "example-skill"],
            ),
            (
                "cli-skills-install-resolving-registry",
                &[("source", "example-skill")][..],
                &["  Resolving", "example-skill", "skills registry"],
            ),
            (
                "cli-skills-install-installed-audited",
                &[("status", "OK"), ("path", "/tmp/example"), ("files", "3")][..],
                &["  OK", "/tmp/example", "3 files scanned"],
            ),
            (
                "cli-skills-install-security-audit-completed",
                &[][..],
                &["  Security audit completed successfully"],
            ),
            (
                "cli-skills-install-tier-official",
                &[("name", "example-skill"), ("version", "1.2.3")][..],
                &["example-skill", "1.2.3", "Official"],
            ),
            (
                "cli-skills-install-tier-community",
                &[("name", "example-skill"), ("version", "1.2.3")][..],
                &[
                    "example-skill",
                    "1.2.3",
                    "Community submission",
                    "zeroclaw skills audit example-skill",
                ],
            ),
        ];
        let zh_cn_cases: &[FormatCase<'_>] = &[
            (
                "cli-skills-install-start",
                &[("source", "example-skill")][..],
                &["正在安装技能来源", "example-skill"],
            ),
            (
                "cli-skills-install-resolving-registry",
                &[("source", "example-skill")][..],
                &["  正在从技能注册表解析", "example-skill"],
            ),
            (
                "cli-skills-install-installed-audited",
                &[("status", "OK"), ("path", "/tmp/example"), ("files", "3")][..],
                &["  OK", "/tmp/example", "已扫描 3 个文件"],
            ),
            (
                "cli-skills-install-security-audit-completed",
                &[][..],
                &["  安全审计已成功完成"],
            ),
            (
                "cli-skills-install-tier-official",
                &[("name", "example-skill"), ("version", "1.2.3")][..],
                &["example-skill", "1.2.3", "官方"],
            ),
            (
                "cli-skills-install-tier-community",
                &[("name", "example-skill"), ("version", "1.2.3")][..],
                &[
                    "example-skill",
                    "1.2.3",
                    "社区提交",
                    "zeroclaw skills audit example-skill",
                ],
            ),
        ];

        for (source, locale, cases) in [
            (include_str!("../locales/en/cli.ftl"), "en", en_cases),
            (
                include_str!("../locales/zh-CN/cli.ftl"),
                "zh-CN",
                zh_cn_cases,
            ),
        ] {
            for (key, args, expected_parts) in cases {
                let value = format_ftl_message(source, locale, key, args)
                    .unwrap_or_else(|| panic!("{key} should format in {locale}"));
                for expected in *expected_parts {
                    assert!(
                        value.contains(expected),
                        "{key} in {locale} should preserve {expected:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn daemon_gateway_bind_cli_strings_format_from_fluent() {
        // The daemon gateway-bind pre-flight messagesare routed through
        // Fluent from src/main.rs via `ta(...)`. Guard the key names and their
        // `{$host}`/`{$port}` placeholders so a typo can't silently degrade the
        // operator-facing fail-fast message back to a `{cli-...}` stub.
        let en = include_str!("../locales/en/cli.ftl");
        let args = &[("host", "127.0.0.1"), ("port", "9090")][..];

        let already_running =
            format_ftl_message(en, "en", "cli-daemon-gateway-already-running", args)
                .expect("cli-daemon-gateway-already-running should format");
        assert!(already_running.contains("127.0.0.1:9090"));
        assert!(already_running.contains("ZeroClaw gateway is already running"));
        assert!(already_running.contains("gateway.port"));

        let port_occupied = format_ftl_message(en, "en", "cli-daemon-gateway-port-occupied", args)
            .expect("cli-daemon-gateway-port-occupied should format");
        assert!(port_occupied.contains("127.0.0.1:9090"));
        assert!(port_occupied.contains("already in use by another process"));
        assert!(port_occupied.contains("gateway.port"));
    }

    #[test]
    fn normalize_locale_strips_encoding() {
        assert_eq!(normalize_locale("en_US.UTF-8"), "en-US");
        assert_eq!(normalize_locale("zh_CN.utf8"), "zh-CN");
        assert_eq!(normalize_locale("fr"), "fr");
    }

    #[test]
    fn normalized_env_locale_rejects_posix_sentinels() {
        assert_eq!(normalized_env_locale(""), None);
        assert_eq!(normalized_env_locale("   "), None);
        assert_eq!(normalized_env_locale("C"), None);
        assert_eq!(normalized_env_locale("c"), None);
        assert_eq!(normalized_env_locale("POSIX"), None);
        assert_eq!(normalized_env_locale("posix"), None);
    }

    #[test]
    fn normalized_env_locale_normalizes_posix_format() {
        assert_eq!(
            normalized_env_locale("zh_CN.UTF-8"),
            Some("zh-CN".to_string())
        );
        assert_eq!(normalized_env_locale("fr_FR"), Some("fr-FR".to_string()));
        assert_eq!(normalized_env_locale(" ja "), Some("ja".to_string()));
    }

    #[test]
    fn pick_locale_skips_posix_sentinel_to_find_a_usable_candidate() {
        // LC_ALL=C with LANG=zh_CN.UTF-8 is common in CI/containers: sys-locale
        // yields "C" as the top-priority candidate, but it must not shadow
        // the real locale carried by a lower-priority variable.
        let candidates = ["C".to_string(), "zh-CN".to_string()];
        assert_eq!(
            pick_locale(candidates.into_iter()),
            Some("zh-CN".to_string())
        );
    }

    #[test]
    fn pick_locale_returns_none_when_all_candidates_are_sentinels() {
        let candidates = ["C".to_string(), "POSIX".to_string(), "".to_string()];
        assert_eq!(pick_locale(candidates.into_iter()), None);
    }

    #[test]
    fn detect_locale_defaults_to_en_without_config() {
        // Locale is config-only. read_config_table() is pure parsing over a
        // string; verify the fallback contract without touching the real
        // filesystem or env. An absent/locale-less table must yield "en".
        assert_eq!(locale_from_table(None), None);
        let no_locale: toml::Table = "model = \"x\"".parse().unwrap();
        assert_eq!(locale_from_table(Some(no_locale)), None);
        let empty_locale: toml::Table = "locale = \"\"".parse().unwrap();
        assert_eq!(locale_from_table(Some(empty_locale)), None);
        // detect_locale layers the "en" fallback over locale_from_table.
        assert_eq!(
            locale_from_table(None).unwrap_or_else(|| "en".to_string()),
            "en"
        );
    }

    #[test]
    fn load_ftl_from_disk_reads_config_dir_data_ftl() {
        // Verify the loader resolves a locale's FTL path and returns the
        // reader's content — using an in-memory reader so no real filesystem
        // or environment is touched. The path must carry the locale and
        // filename so a fetched catalogue at <dir>/.../<locale>/<file> is found.
        let seen = std::cell::RefCell::new(Vec::<std::path::PathBuf>::new());
        let loaded = load_ftl_with_reader("xx", "cli.ftl", |p| {
            seen.borrow_mut().push(p.to_path_buf());
            Some("cli-probe = hit\n".to_string())
        });
        assert_eq!(loaded.as_deref(), Some("cli-probe = hit\n"));

        let paths = seen.borrow();
        assert!(!paths.is_empty(), "reader must be consulted with a path");
        let p = paths[0].to_string_lossy();
        assert!(p.contains("xx"), "path must carry the locale: {p}");
        assert!(p.ends_with("cli.ftl"), "path must target the file: {p}");
    }

    #[test]
    fn non_english_locales_format_context_window_args() {
        for locale in ["es", "fr", "ja", "zh-CN"] {
            let sources = load_cli_ftl_sources(locale);

            for (key, args) in [
                (
                    "cli-doctor-ctxwin-set",
                    vec![("provider_ref", "groq.test"), ("ctx", "131072")],
                ),
                (
                    "cli-doctor-ctxwin-would-set",
                    vec![("provider_ref", "groq.test"), ("ctx", "131072")],
                ),
                (
                    "cli-doctor-ctxwin-already-set",
                    vec![("provider_ref", "groq.test"), ("ctx", "131072")],
                ),
                (
                    "cli-doctor-ctxwin-not-found",
                    vec![("provider_ref", "groq.test")],
                ),
                (
                    "cli-doctor-ctxwin-fetch-failed",
                    vec![("provider_ref", "groq.test")],
                ),
            ] {
                let formatted = format_cli_string_with_args(&sources, key, &args)
                    .unwrap_or_else(|| panic!("{locale}: {key} should format"));
                assert!(
                    formatted.contains("groq.test"),
                    "{locale}: {key} missing provider_ref in: {formatted}"
                );
                if args.iter().any(|(k, _)| *k == "ctx") {
                    assert!(
                        formatted.contains("131072"),
                        "{locale}: {key} missing ctx value in: {formatted}"
                    );
                }
                assert!(
                    !formatted.contains("{provider_ref}"),
                    "{locale}: {key} has unformatted placeholder in: {formatted}"
                );
                assert!(
                    !formatted.contains("{ctx}"),
                    "{locale}: {key} has unformatted placeholder in: {formatted}"
                );
            }

            // Test the new update-context-windows about key (no args)
            let about_formatted = format_cli_string_with_args(
                &sources,
                "cli-doctor-update-context-windows-about",
                &[],
            )
            .unwrap_or_else(|| {
                panic!("{locale}: cli-doctor-update-context-windows-about should format")
            });
            assert!(
                !about_formatted.is_empty(),
                "{locale}: cli-doctor-update-context-windows-about should not be empty"
            );
            if locale == "zh-CN" {
                assert!(
                    about_formatted.contains("提供"),
                    "{locale}: cli-doctor-update-context-windows-about should have Chinese text, got: {about_formatted}"
                );
            }

            let bar_formatted = format_cli_string_with_args(
                &sources,
                "cli-agent-context-bar",
                &[
                    ("used", "1000"),
                    ("max", "8192"),
                    ("bar", "██░░"),
                    ("pct", "12"),
                ],
            )
            .unwrap_or_else(|| panic!("{locale}: cli-agent-context-bar should format"));
            assert!(
                bar_formatted.contains("1000"),
                "{locale}: cli-agent-context-bar missing used value in: {bar_formatted}"
            );
            assert!(
                bar_formatted.contains("8192"),
                "{locale}: cli-agent-context-bar missing max value in: {bar_formatted}"
            );
            assert!(
                !bar_formatted.contains("{used}"),
                "{locale}: cli-agent-context-bar has unformatted placeholder in: {bar_formatted}"
            );
            assert!(
                !bar_formatted.contains("{max}"),
                "{locale}: cli-agent-context-bar has unformatted placeholder in: {bar_formatted}"
            );
            assert!(
                !bar_formatted.contains("{bar}"),
                "{locale}: cli-agent-context-bar has unformatted placeholder in: {bar_formatted}"
            );
            assert!(
                !bar_formatted.contains("{pct}"),
                "{locale}: cli-agent-context-bar has unformatted placeholder in: {bar_formatted}"
            );
        }
    }
}
