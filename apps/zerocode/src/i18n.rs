use fluent::{FluentArgs, FluentBundle, FluentResource};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use unic_langid::LanguageIdentifier;

static STRINGS: OnceLock<HashMap<String, String>> = OnceLock::new();
static FTL_SOURCES: OnceLock<FtlSources> = OnceLock::new();
static LOCALE: OnceLock<String> = OnceLock::new();
static CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();
static REPORTED_MISSING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

const EN_FTL: &str = include_str!("../locales/en/zerocode.ftl");

struct FtlSources {
    locale: String,
    disk: Option<String>,
}

/// Initialise i18n with the active locale and the resolved client config dir.
/// The config dir is where downloaded locale FTL is read from (and where the
/// Locale pane writes it), so passing it explicitly keeps the read and write
/// paths consistent with a `--config-dir` flag — no env-var coupling.
pub fn init(locale: &str, config_dir: &std::path::Path) {
    let _ = CONFIG_DIR.set(config_dir.to_path_buf());
    let locale = LOCALE.get_or_init(|| normalize_locale(locale));
    STRINGS.get_or_init(|| load_strings(locale));
    FTL_SOURCES.get_or_init(|| load_ftl_sources(locale));
}

pub fn t(key: &str) -> String {
    let map = STRINGS.get_or_init(|| load_strings(active_locale()));
    if let Some(value) = map.get(key) {
        return value.clone();
    }
    record_missing(key);
    format!("{{{key}}}")
}

pub fn t_args(key: &str, args: &[(&str, &str)]) -> String {
    let sources = FTL_SOURCES.get_or_init(|| load_ftl_sources(active_locale()));
    if let Some(disk) = sources.disk.as_deref()
        && let Some(value) = format_ftl_message(disk, &sources.locale, key, args)
    {
        return value;
    }
    if let Some(value) = format_ftl_message(EN_FTL, "en", key, args) {
        return value;
    }
    record_missing(key);
    format!("{{{key}}}")
}

pub fn detect_locale() -> String {
    locale_from_config().unwrap_or_else(|| "en".to_string())
}

pub fn normalize_locale(raw: &str) -> String {
    raw.split('.').next().unwrap_or(raw).replace('_', "-")
}

fn active_locale() -> &'static str {
    LOCALE.get_or_init(detect_locale).as_str()
}

fn load_strings(locale: &str) -> HashMap<String, String> {
    let mut map = format_ftl_messages(EN_FTL, "en");
    if locale != "en"
        && let Some(disk_ftl) = load_ftl_from_disk(locale)
    {
        map.extend(format_ftl_messages(&disk_ftl, locale));
    }
    map
}

fn format_ftl_messages(ftl_source: &str, locale: &str) -> HashMap<String, String> {
    let resource =
        FluentResource::try_new(ftl_source.to_string()).unwrap_or_else(|(resource, _)| resource);
    let language_identifier: LanguageIdentifier =
        locale.parse().unwrap_or_else(|_| "en".parse().unwrap());
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

/// Disk lookup for a locale's zerocode catalogue. Reads the canonical shared
/// location written by `zeroclaw locales fetch`:
/// `<config_dir>/data/ftl/<locale>/zerocode.ftl`, where `<config_dir>` honors
/// `ZEROCLAW_CONFIG_DIR` and otherwise defaults to `~/.zeroclaw`. This mirrors
/// the runtime loader's path (zeroclaw-config::ftl_locale_dir) — kept inline
/// because zerocode carries no `zeroclaw-*` dependency. `ZEROCODE_LOCALE_DIR`
/// remains an explicit override for testing.
fn load_ftl_from_disk(locale: &str) -> Option<String> {
    let filename = format!("{locale}/zerocode.ftl");
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(explicit) = std::env::var("ZEROCODE_LOCALE_DIR") {
        candidates.push(PathBuf::from(explicit).join(&filename));
    }
    candidates.push(config_dir().join("data").join("ftl").join(&filename));
    for path in candidates {
        if let Ok(content) = std::fs::read_to_string(&path) {
            return Some(content);
        }
    }
    None
}

/// Resolve the ZeroClaw config directory with the same precedence as
/// `client::resolve_config_dir`: the `--config-dir` flag (passed to `init` and
/// cached in `CONFIG_DIR`) first, then `ZEROCLAW_CONFIG_DIR`, then `~/.zeroclaw`.
/// This keeps the FTL read path aligned with the flag the rest of zerocode uses.
fn config_dir() -> PathBuf {
    if let Some(dir) = CONFIG_DIR.get() {
        return dir.clone();
    }
    if let Ok(custom) = std::env::var("ZEROCLAW_CONFIG_DIR") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    directories::BaseDirs::new()
        .map(|b| b.home_dir().join(".zeroclaw"))
        .unwrap_or_else(|| PathBuf::from(".zeroclaw"))
}

fn locale_from_config() -> Option<String> {
    let base = directories::BaseDirs::new()?;
    let candidates = [
        base.config_dir().join("zerocode/zerocode-config.toml"),
        base.home_dir().join(".zeroclaw/zerocode-config.toml"),
        base.home_dir().join(".zeroclaw/config.toml"),
        base.config_dir().join("zeroclaw/config.toml"),
    ];
    for path in &candidates {
        if let Ok(contents) = std::fs::read_to_string(path)
            && let Ok(table) = contents.parse::<toml::Table>()
            && let Some(locale) = table.get("locale").and_then(|v| v.as_str())
        {
            let trimmed = locale.trim();
            if !trimmed.is_empty() {
                return Some(normalize_locale(trimmed));
            }
        }
    }
    None
}

fn load_ftl_sources(locale: &str) -> FtlSources {
    FtlSources {
        locale: locale.to_string(),
        disk: (locale != "en")
            .then(|| load_ftl_from_disk(locale))
            .flatten(),
    }
}

fn format_ftl_message(
    ftl_source: &str,
    locale: &str,
    key: &str,
    args: &[(&str, &str)],
) -> Option<String> {
    let resource =
        FluentResource::try_new(ftl_source.to_string()).unwrap_or_else(|(resource, _)| resource);
    let language_identifier: LanguageIdentifier =
        locale.parse().unwrap_or_else(|_| "en".parse().unwrap());
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

fn record_missing(key: &str) {
    let set = REPORTED_MISSING.get_or_init(|| Mutex::new(HashSet::new()));
    if let Ok(mut guard) = set.lock()
        && guard.insert(key.to_string())
    {
        eprintln!("zerocode: missing i18n key: {key}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn en_catalogue_parses() {
        let map = format_ftl_messages(EN_FTL, "en");
        assert!(map.contains_key("zc-pane-dashboard"));
        assert!(map.contains_key("zc-pane-chat"));
    }

    #[test]
    fn missing_key_returns_brace_form() {
        let value = t("zc-definitely-not-a-real-key");
        assert_eq!(value, "{zc-definitely-not-a-real-key}");
    }

    #[test]
    fn normalize_strips_encoding() {
        assert_eq!(normalize_locale("en_US.UTF-8"), "en-US");
        assert_eq!(normalize_locale("zh_CN.utf8"), "zh-CN");
        assert_eq!(normalize_locale("fr"), "fr");
    }
}
