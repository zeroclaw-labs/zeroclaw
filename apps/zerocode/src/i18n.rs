use fluent::{FluentArgs, FluentResource, concurrent::FluentBundle};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use unic_langid::LanguageIdentifier;

static STRINGS: OnceLock<HashMap<String, String>> = OnceLock::new();
static FTL_BUNDLES: OnceLock<FtlBundles> = OnceLock::new();
static LOCALE: OnceLock<String> = OnceLock::new();
static CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();
static REPORTED_MISSING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

const EN_FTL: &str = include_str!("../locales/en/zerocode.ftl");

struct FtlBundles {
    english: FluentBundle<FluentResource>,
    disk: Option<FluentBundle<FluentResource>>,
}

impl FtlBundles {
    fn format(&self, key: &str, args: &[(&str, &str)]) -> Option<String> {
        self.disk
            .as_ref()
            .and_then(|bundle| format_ftl_message(bundle, key, args))
            .or_else(|| format_ftl_message(&self.english, key, args))
    }
}

/// Initialise i18n with the active locale and the resolved client config dir.
/// The config dir is where downloaded locale FTL is read from (and where the
/// Locale pane writes it), so passing it explicitly keeps the read and write
/// paths consistent with a `--config-dir` flag — no env-var coupling.
pub fn init(locale: &str, config_dir: &std::path::Path) {
    let _ = CONFIG_DIR.set(config_dir.to_path_buf());
    let locale = LOCALE.get_or_init(|| normalize_locale(locale));
    STRINGS.get_or_init(|| load_strings(locale));
    FTL_BUNDLES.get_or_init(|| load_ftl_bundles(locale));
}

pub fn t(key: &str) -> String {
    let map = STRINGS.get_or_init(|| load_strings(active_locale()));
    if let Some(value) = map.get(key) {
        return value.clone();
    }
    record_missing(key);
    format!("{{{key}}}")
}

/// Optional lookup for keys that legitimately may not exist (e.g. derived
/// override keys with a code-side fallback). Returns `None` on miss without
/// recording it as a missing-translation warning.
pub fn try_t(key: &str) -> Option<String> {
    let map = STRINGS.get_or_init(|| load_strings(active_locale()));
    map.get(key).cloned()
}

pub fn t_args(key: &str, args: &[(&str, &str)]) -> String {
    let bundles = FTL_BUNDLES.get_or_init(|| load_ftl_bundles(active_locale()));
    if let Some(value) = bundles.format(key, args) {
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
    let bundle = build_ftl_bundle(ftl_source, locale);
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
pub(crate) fn config_dir() -> PathBuf {
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
    locale_from_config_dir(&config_dir())
}

/// Path-pure core of [`locale_from_config`]: read the `locale` key from
/// `<dir>/zerocode-config.toml`. Kept separate so the read path can be tested
/// against the writer's filename without touching process-global state.
fn locale_from_config_dir(dir: &std::path::Path) -> Option<String> {
    let contents = std::fs::read_to_string(dir.join("zerocode-config.toml")).ok()?;
    let table = contents.parse::<toml::Table>().ok()?;
    let locale = table.get("locale").and_then(|v| v.as_str())?;
    let trimmed = locale.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(normalize_locale(trimmed))
}

fn load_ftl_bundles(locale: &str) -> FtlBundles {
    // Match the existing process-lifetime locale snapshot, caching structure,
    // not formatted labels: callers supply new argument values on each draw.
    FtlBundles {
        english: build_ftl_bundle(EN_FTL, "en"),
        disk: (locale != "en")
            .then(|| load_ftl_from_disk(locale))
            .flatten()
            .map(|source| build_ftl_bundle(&source, locale)),
    }
}

fn build_ftl_bundle(ftl_source: &str, locale: &str) -> FluentBundle<FluentResource> {
    let resource =
        FluentResource::try_new(ftl_source.to_string()).unwrap_or_else(|(resource, _)| resource);
    let language_identifier: LanguageIdentifier = match locale.parse() {
        Ok(identifier) => identifier,
        Err(_) => "en"
            .parse()
            .expect("static English Fluent locale must parse"),
    };
    let mut bundle = FluentBundle::new_concurrent(vec![language_identifier]);
    bundle.set_use_isolating(false);
    let _ = bundle.add_resource(resource);
    bundle
}

fn format_ftl_message(
    bundle: &FluentBundle<FluentResource>,
    key: &str,
    args: &[(&str, &str)],
) -> Option<String> {
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
    fn cached_bundles_format_fresh_arguments_and_preserve_fallback() {
        let bundles = FtlBundles {
            english: build_ftl_bundle(
                "greeting = Hello { $name }\nmissing = English { $name }\ninvalid = English { $name }\nattribute-only = English { $name }\n",
                "en",
            ),
            disk: Some(build_ftl_bundle(
                "greeting = Bonjour { $name }\ninvalid = { $unavailable }\nattribute-only =\n    .label = Locale attribute\n",
                "fr",
            )),
        };
        for name in ["first", "second"] {
            let args = [("name", name)];
            assert_eq!(
                bundles.format("greeting", &args),
                Some(format!("Bonjour {name}"))
            );
            for key in ["missing", "invalid", "attribute-only"] {
                assert_eq!(bundles.format(key, &args), Some(format!("English {name}")));
            }
        }
        assert_eq!(bundles.format("greeting", &[]), None);
        assert_eq!(bundles.format("absent", &[]), None);
    }

    #[test]
    fn cached_bundles_recover_valid_messages_from_malformed_resource() {
        let source = "valid = Value { $value }\nbroken = {\n";
        assert!(FluentResource::try_new(source.to_string()).is_err());
        let bundle = build_ftl_bundle(source, "not a valid locale");
        assert_eq!(
            bundle.locales,
            vec!["en".parse::<LanguageIdentifier>().unwrap()]
        );
        for value in ["one", "two"] {
            assert_eq!(
                format_ftl_message(&bundle, "valid", &[("value", value)]),
                Some(format!("Value {value}"))
            );
        }
        assert_eq!(format_ftl_message(&bundle, "broken", &[]), None);
    }

    #[test]
    fn t_args_reuses_initialized_bundles() {
        let key = "zc-error-daemon-version-mismatch";
        let first_args = [("client_version", "0.8.1"), ("server_version", "0.8.0")];
        let first = t_args(key, &first_args);
        let bundles = FTL_BUNDLES.get().expect("t_args initializes its bundles");
        let pattern = bundles.english.get_message(key).unwrap().value().unwrap();
        assert_eq!(first, bundles.format(key, &first_args).unwrap());

        let next_args = [("client_version", "0.8.3"), ("server_version", "0.8.2")];
        assert_eq!(
            t_args(key, &next_args),
            bundles.format(key, &next_args).unwrap()
        );
        let reused = FTL_BUNDLES.get().unwrap();
        assert!(std::ptr::eq(bundles, reused));
        assert!(std::ptr::eq(
            pattern,
            reused.english.get_message(key).unwrap().value().unwrap()
        ));
        assert_eq!(
            t_args("zc-definitely-not-a-real-key", &[]),
            "{zc-definitely-not-a-real-key}"
        );
    }

    #[test]
    fn cached_bundle_supports_concurrent_arguments() {
        let bundle = build_ftl_bundle("value = Current { $value }", "en");
        std::thread::scope(|scope| {
            for value in ["first", "second"] {
                let bundle = &bundle;
                scope.spawn(move || {
                    assert_eq!(
                        format_ftl_message(bundle, "value", &[("value", value)]),
                        Some(format!("Current {value}"))
                    );
                });
            }
        });
    }

    #[test]
    #[ignore = "bounded formatting timing comparison; run explicitly with --ignored --nocapture"]
    fn parameterized_bundle_timing() {
        use std::hint::black_box;
        use std::time::Instant;

        const ITERATIONS: usize = 200;
        let key = "zc-error-daemon-version-mismatch";
        let args = [("client_version", "0.8.1"), ("server_version", "0.8.0")];
        let bundle = build_ftl_bundle(EN_FTL, "en");
        let expected = format_ftl_message(&bundle, key, &args).unwrap();
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            let fresh = build_ftl_bundle(black_box(EN_FTL), "en");
            assert_eq!(
                format_ftl_message(&fresh, key, black_box(&args)).unwrap(),
                expected
            );
        }
        let reparsed = start.elapsed();
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            assert_eq!(
                format_ftl_message(black_box(&bundle), key, black_box(&args)).unwrap(),
                expected
            );
        }
        eprintln!(
            "{ITERATIONS} formats: reparse={reparsed:?}, cached={:?}",
            start.elapsed()
        );
    }

    #[test]
    fn en_catalogue_parses() {
        let map = format_ftl_messages(EN_FTL, "en");
        assert!(map.contains_key("zc-pane-dashboard"));
        assert!(map.contains_key("zc-pane-chat"));
        let mismatch = format_ftl_message(
            &build_ftl_bundle(EN_FTL, "en"),
            "zc-error-daemon-version-mismatch",
            &[("client_version", "0.8.1"), ("server_version", "0.8.0")],
        )
        .unwrap();
        assert!(mismatch.contains("0.8.1"));
        assert!(mismatch.contains("0.8.0"));
    }

    // Every Config-pane key the zerocode UI section renders must resolve
    // through the *same* Fluent bundle the TUI uses, never falling back to the
    // raw `{key}` identifier. Code and catalog can drift independently, so this
    // pins the exact keys `zerocode_pane.rs` looks up for the Todo tracker UI.
    #[test]
    fn todo_tracker_config_keys_resolve() {
        let map = format_ftl_messages(EN_FTL, "en");
        const KEYS: &[&str] = &[
            // Section tabs
            "zc-zerocode-tab-todo-tracker",
            // Todo tracker section
            "zc-zerocode-tracker-title",
            "zc-zerocode-tracker-enabled",
            "zc-zerocode-tracker-enabled-at-start",
            "zc-zerocode-tracker-location",
            "zc-zerocode-tracker-width",
            "zc-zerocode-tracker-max-height",
            "zc-zerocode-tracker-saved",
            "zc-zerocode-tracker-saved-env-override",
            "zc-zerocode-tracker-saved-resolve-error",
            "zc-zerocode-tracker-saved-still-invalid",
            "zc-zerocode-tracker-edit-refused",
            "zc-zerocode-tracker-edit-number",
            "zc-zerocode-tracker-edit-bool",
            "zc-zerocode-tracker-edit-location",
            // Shared Config-pane validation/status keys
            "zc-zerocode-config-invalid-number",
            "zc-zerocode-config-positive-required",
            "zc-zerocode-config-save-mismatch",
            // Help hints
            "zc-zerocode-help-todo-tracker",
        ];
        for key in KEYS {
            let value = map
                .get(*key)
                .unwrap_or_else(|| panic!("catalog missing Config-pane key `{key}`"));
            assert!(
                !value.is_empty(),
                "catalog key `{key}` resolved to an empty string"
            );
            // `t()` must not fall back to the raw `{key}` brace form.
            assert_ne!(
                t(key),
                format!("{{{key}}}"),
                "key `{key}` renders as its raw identifier instead of a translation"
            );
        }
        let save_failed = format_ftl_message(
            &build_ftl_bundle(EN_FTL, "en"),
            "zc-zerocode-config-save-failed",
            &[("error", "disk unavailable")],
        )
        .expect("argument-bearing Config-pane save error key must format");
        assert!(save_failed.contains("disk unavailable"));
        assert_ne!(
            t_args(
                "zc-zerocode-config-save-failed",
                &[("error", "disk unavailable")]
            ),
            "{zc-zerocode-config-save-failed}"
        );

        // The malformed-section prompt carries the parser detail, so it is
        // argument-bearing too and cannot be checked by the no-arg loop above.
        let load_error = format_ftl_message(
            &build_ftl_bundle(EN_FTL, "en"),
            "zc-zerocode-tracker-load-error",
            &[("error", "invalid type: string")],
        )
        .expect("argument-bearing tracker load error key must format");
        assert!(load_error.contains("invalid type: string"));
        assert!(load_error.contains("[todotracker]"));
    }

    #[test]
    fn argument_messages_format_in_all_builtin_catalogues() {
        let catalogues = [
            ("en", EN_FTL),
            ("es", include_str!("../locales/es/zerocode.ftl")),
            ("fr", include_str!("../locales/fr/zerocode.ftl")),
            ("ja", include_str!("../locales/ja/zerocode.ftl")),
            ("zh-CN", include_str!("../locales/zh-CN/zerocode.ftl")),
        ];

        for (locale, source) in catalogues {
            let bundle = build_ftl_bundle(source, locale);
            let timeout = format_ftl_message(
                &bundle,
                "zc-error-daemon-initialize-timeout",
                &[("seconds", "10")],
            )
            .unwrap_or_else(|| panic!("timeout message must format for {locale}"));
            assert!(timeout.contains("10"));

            let controls = format_ftl_message(
                &bundle,
                "zc-app-help-controls",
                &[("up", "↑"), ("down", "↓"), ("cancel", "Esc")],
            )
            .unwrap_or_else(|| panic!("help controls must format for {locale}"));
            assert!(controls.contains('↑'));
            assert!(controls.contains('↓'));
            assert!(controls.contains("Esc"));
        }
    }

    #[test]
    fn spawned_daemon_startup_failure_formats_in_all_builtin_catalogues() {
        let catalogues = [
            ("en", EN_FTL),
            ("es", include_str!("../locales/es/zerocode.ftl")),
            ("fr", include_str!("../locales/fr/zerocode.ftl")),
            ("ja", include_str!("../locales/ja/zerocode.ftl")),
            ("zh-CN", include_str!("../locales/zh-CN/zerocode.ftl")),
        ];

        for (locale, source) in catalogues {
            let failure = format_ftl_message(
                &build_ftl_bundle(source, locale),
                "zc-error-spawned-daemon-startup",
                &[("details", "test failure")],
            )
            .unwrap_or_else(|| panic!("spawned-daemon failure must format for {locale}"));
            assert!(failure.contains("test failure"));
        }
    }

    #[test]
    fn doctor_persistence_keys_present_in_all_builtin_catalogues() {
        // The Doctor view surfaces four persistence keys in the detail panel.
        // Every shipped catalogue must define them so the operator-facing
        // diagnostics never fall back to a bare `{key}` placeholder.
        let catalogues = [
            ("en", EN_FTL),
            ("es", include_str!("../locales/es/zerocode.ftl")),
            ("fr", include_str!("../locales/fr/zerocode.ftl")),
            ("ja", include_str!("../locales/ja/zerocode.ftl")),
            ("zh-CN", include_str!("../locales/zh-CN/zerocode.ftl")),
        ];

        for (locale, source) in catalogues {
            let bundle = build_ftl_bundle(source, locale);
            for key in [
                "zc-doctor-error-daemon-timeout",
                "zc-doctor-partial-banner",
                "zc-doctor-partial-hint",
            ] {
                assert!(
                    format_ftl_message(&bundle, key, &[]).is_some(),
                    "{key} must be defined for {locale}"
                );
            }

            let log_path = format_ftl_message(
                &bundle,
                "zc-doctor-log-path",
                &[("path", "/tmp/trace-2026-08-01.jsonl")],
            )
            .unwrap_or_else(|| panic!("zc-doctor-log-path must format for {locale}"));
            assert!(
                log_path.contains("/tmp/trace-2026-08-01.jsonl"),
                "zc-doctor-log-path must embed the resolved path for {locale}: {log_path}"
            );
        }
    }

    #[test]
    fn daemon_process_labels_are_explicit_in_all_builtin_catalogues() {
        let catalogues = [
            ("en", EN_FTL, "Daemon Memory", "Daemon CPU"),
            (
                "es",
                include_str!("../locales/es/zerocode.ftl"),
                "Memoria del demonio",
                "CPU del demonio",
            ),
            (
                "fr",
                include_str!("../locales/fr/zerocode.ftl"),
                "Mémoire du démon",
                "CPU du démon",
            ),
            (
                "ja",
                include_str!("../locales/ja/zerocode.ftl"),
                "デーモンメモリ",
                "デーモン CPU",
            ),
            (
                "zh-CN",
                include_str!("../locales/zh-CN/zerocode.ftl"),
                "守护进程内存",
                "守护进程 CPU",
            ),
        ];

        for (locale, source, expected_memory, expected_cpu) in catalogues {
            let bundle = build_ftl_bundle(source, locale);
            assert_eq!(
                format_ftl_message(&bundle, "zc-dashboard-label-daemon-memory", &[]).as_deref(),
                Some(expected_memory),
                "daemon memory label for {locale}"
            );
            assert_eq!(
                format_ftl_message(&bundle, "zc-dashboard-label-daemon-cpu", &[]).as_deref(),
                Some(expected_cpu),
                "daemon CPU label for {locale}"
            );
        }
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

    #[test]
    fn locale_round_trips_through_writer_path() {
        let dir = tempfile::tempdir().unwrap();
        crate::config::persist_locale(dir.path(), "zh-CN").unwrap();
        assert_eq!(
            locale_from_config_dir(dir.path()),
            Some("zh-CN".to_string()),
            "i18n must read the locale from the same file the Locale pane writes"
        );
    }

    #[test]
    fn locale_from_config_dir_none_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(locale_from_config_dir(dir.path()), None);
    }
}
