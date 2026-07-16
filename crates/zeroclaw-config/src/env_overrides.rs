//! V0.8.0 env-var override mechanism.

use crate::schema::Config;
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

const PREFIX: &str = "ZEROCLAW_";
const SEP: &str = "__";

static NON_OVERRIDABLE_PATHS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| HashSet::from(["schema_version"]));

#[derive(Debug, Default, Clone)]
pub struct AppliedOverrides {
    pub paths: HashSet<String>,
    pub snapshots: HashMap<String, String>,
}

/// Apply every `ZEROCLAW_<lowercase>` env var to `config`. Returns the set of
/// dotted prop-paths that were overridden plus the pre-override raw values
/// for each. Hard-errors on any env var that doesn't resolve to a known
/// schema path or whose alias fails validation.
pub fn apply_env_overrides(config: &mut Config) -> Result<AppliedOverrides> {
    let mut entries: Vec<(String, String, String)> = std::env::vars()
        .filter_map(|(k, v)| {
            let tail = k.strip_prefix(PREFIX)?;
            (!tail.is_empty()
                && tail
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
            .then(|| (k.clone(), v, tail.to_string()))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut paths: HashSet<String> = HashSet::with_capacity(entries.len());
    let mut snapshots: HashMap<String, String> = HashMap::with_capacity(entries.len());
    for (env_name, value, tail) in entries {
        let path = resolve_path(&tail, config)
            .with_context(|| format!("{env_name} did not resolve to a schema path"))?;
        if NON_OVERRIDABLE_PATHS.contains(path.as_str()) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"env_var": env_name, "path": path})),
                "env override rejected: field is not overridable"
            );
            anyhow::bail!("{env_name} -> {path}: this field is not overridable via env vars");
        }
        // Snapshot the pre-override raw value via TOML serde walk. Bypasses
        // `Config::get_prop`'s unconditional secret mask: secret fields on
        // `config` carry plaintext (post-`decrypt_secrets`), so the snapshot
        // captures the real value that should be restored at save time.
        let snapshot = raw_value_for_path(config, &path).unwrap_or_default();
        snapshots.insert(path.clone(), snapshot);

        config
            .set_prop(&path, &value)
            .with_context(|| format!("{env_name} → {path}"))?;
        if Config::prop_is_secret(&path) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"path": path, "env_var": env_name})),
                "Secret applied from env override"
            );
        } else {
            ::zeroclaw_log::record!(
                DEBUG,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"path": path, "env_var": env_name})),
                "Env override applied"
            );
        }
        paths.insert(path);
    }
    if !paths.is_empty() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"count": paths.len()})),
            "Applied env-var config overrides"
        );
    }
    Ok(AppliedOverrides { paths, snapshots })
}

/// Walk an env-var tail against the schema. Map-keyed positions consume one
/// `__`-delimited alias token (which may contain single `_` per the alias
/// validator); everything else resolves via `prop_fields()` lookup.
fn resolve_path(tail: &str, config: &mut Config) -> Result<String> {
    let mut sections = Config::map_key_sections();
    sections.sort_by_key(|s| std::cmp::Reverse(s.path.len()));
    for section in sections {
        let env_pfx: String = section.path.replace('.', SEP);
        let with_sep = format!("{env_pfx}{SEP}");
        let Some(rest) = tail.strip_prefix(&with_sep) else {
            continue;
        };
        let mut parts = rest.splitn(2, SEP);
        let alias = parts.next().filter(|s| !s.is_empty()).ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"section": section.path, "tail": tail})),
                "env override path missing alias segment"
            );
            anyhow::Error::msg(format!("missing alias after `{}`", section.path))
        })?;
        let inner = parts.next().unwrap_or("");
        // Propagate the alias-validator's specific error so operators see
        // *why* their alias was rejected (leading underscore, uppercase, …)
        // instead of the generic "Unknown property" that would surface from
        // a downstream `set_prop` against a non-existent map key.
        config.create_map_key(section.path, alias).map_err(|e| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({
                        "section": section.path,
                        "alias": alias,
                        "error": format!("{}", e),
                    })),
                "env override alias rejected by validator"
            );
            anyhow::Error::msg(format!(
                "invalid alias `{alias}` for `{}`: {e}",
                section.path
            ))
        })?;
        let path = if inner.is_empty() {
            format!("{}.{}", section.path, alias)
        } else {
            // Inner segments are `__`-separated snake-case field names — the
            // same casing the prop-path uses, so join them verbatim.
            let inner_path = inner.split(SEP).collect::<Vec<_>>().join(".");
            format!("{}.{}.{}", section.path, alias, inner_path)
        };
        return Ok(path);
    }

    // Non-map path: prop_fields() entries are dotted snake-case field
    // names. Convert to env-form (`.` → `__`) and compare.
    config
        .prop_fields()
        .into_iter()
        .find(|f| f.name.replace('.', SEP) == tail)
        .map(|f| f.name)
        .ok_or_else(|| {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(::serde_json::json!({"tail": tail})),
                "env override path does not match any schema field"
            );
            anyhow::Error::msg(format!("no schema field has env-form `{tail}`"))
        })
}

pub(crate) fn raw_value_for_path(source: &Config, path: &str) -> Option<String> {
    let table = toml::Value::try_from(source).ok()?;
    let mut current: &toml::Value = &table;
    for segment in path.split('.') {
        let tbl = current.as_table()?;
        current = match tbl.get(segment) {
            Some(v) => v,
            None => tbl.get(&segment.replace('-', "_"))?,
        };
    }
    Some(match current {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

pub fn mask_env_overrides_for_save(
    config_to_save: &mut Config,
    snapshots: &HashMap<String, String>,
) -> Result<()> {
    for (path, value) in snapshots {
        if let Err(err) = config_to_save.set_prop(path, value) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"path": path, "error": format!("{}", err)})),
                "Save-mask reset failed; field retains default"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) async fn env_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

/// Resolve the effective OpenAI STT API key, applying the precedence:
/// explicit config value > `TRANSCRIPTION_API_KEY` env > `OPENAI_API_KEY` env.
///
/// This is a config-layer, stateless resolver. It never writes into a
/// config struct — the caller owns the returned `Option<String>` and
/// decides whether to construct a provider. The environment value is the
/// single source of truth for the override; it is never fanned out into
/// multiple mutable config fields.
pub fn resolve_openai_stt_api_key(existing: Option<&str>) -> Option<String> {
    if let Some(key) = existing {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    for name in &["TRANSCRIPTION_API_KEY", "OPENAI_API_KEY"] {
        if let Ok(val) = std::env::var(name) {
            let trimmed = val.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

/// Legacy provider-native env-var fallbacks applied after
/// [`apply_env_overrides`] so that `ZEROCLAW_*` values always win.
///
/// Checks whether `TRANSCRIPTION_API_KEY` or `OPENAI_API_KEY` is set
/// and logs a note for operator visibility.  The actual credential
/// resolution is done on-demand by [`resolve_openai_stt_api_key`],
/// which is called during provider construction — no value is ever
/// written into the [`Config`] struct by this function, so the
/// single-source-of-truth rule is preserved and save/reload cycles
/// never persist the environment credential.
///
/// Returns an empty `Vec` (no injection paths).  The caller's merge
/// loop is a deliberate no-op — retain it at the call site for
/// future legacy-fallback additions that need injection machinery.
pub fn apply_legacy_env_fallbacks(_config: &mut Config) -> Vec<(String, String)> {
    let value = find_first_env(&["TRANSCRIPTION_API_KEY", "OPENAI_API_KEY"]);
    if value.is_some() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            "Legacy env vars (TRANSCRIPTION_API_KEY / OPENAI_API_KEY) \
             detected; credentials resolved on demand during provider construction"
        );
    }
    Vec::new()
}

/// Returns the value of the first matching env var from `names`,
/// checked in order so the first entry takes priority.
fn find_first_env(names: &[&str]) -> Option<String> {
    for name in names {
        if let Ok(value) = std::env::var(name) {
            let trimmed = value.trim().to_string();
            if !trimmed.is_empty() {
                return Some(trimmed);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Config;

    /// RAII-ish helper: removes the named var on drop so failed asserts don't
    /// leak state into sibling tests.
    struct EnvVarGuard(&'static str);
    impl EnvVarGuard {
        fn set(name: &'static str, value: &str) -> Self {
            // SAFETY: tests serialize on `env_test_lock()`.
            unsafe { std::env::set_var(name, value) };
            Self(name)
        }
    }
    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: tests serialize on `env_test_lock()`.
            unsafe { std::env::remove_var(self.0) };
        }
    }

    /// RAII fixture that fully owns the legacy env-var namespace
    /// (`TRANSCRIPTION_API_KEY`, `OPENAI_API_KEY`) while the shared
    /// `env_test_lock()` is held. Snapshots pre-existing values on
    /// construction, clears them both, and restores originals on drop.
    /// Tests that exercise `apply_legacy_env_fallbacks` use this instead
    /// of bare `EnvVarGuard` so their behaviour is deterministic
    /// regardless of what the dev or CI shell has set.
    struct LegacyEnvFixture {
        saved_transcription: Option<String>,
        saved_openai: Option<String>,
    }
    impl LegacyEnvFixture {
        /// Clear both legacy env vars and snapshot their previous values
        /// (if any). Callers then set exactly the vars their test needs.
        fn isolate() -> Self {
            // SAFETY: caller (the test function) holds `env_test_lock()`.
            let saved_transcription = std::env::var("TRANSCRIPTION_API_KEY").ok();
            let saved_openai = std::env::var("OPENAI_API_KEY").ok();
            unsafe {
                std::env::remove_var("TRANSCRIPTION_API_KEY");
                std::env::remove_var("OPENAI_API_KEY");
            }
            Self {
                saved_transcription,
                saved_openai,
            }
        }
    }
    impl Drop for LegacyEnvFixture {
        fn drop(&mut self) {
            // SAFETY: caller holds `env_test_lock()`.
            unsafe {
                match &self.saved_transcription {
                    Some(v) => std::env::set_var("TRANSCRIPTION_API_KEY", v),
                    None => std::env::remove_var("TRANSCRIPTION_API_KEY"),
                }
                match &self.saved_openai {
                    Some(v) => std::env::set_var("OPENAI_API_KEY", v),
                    None => std::env::remove_var("OPENAI_API_KEY"),
                }
            }
        }
    }

    // ── apply_env_overrides tests ─────────────────────────────────

    #[tokio::test]
    async fn walker_resolves_typed_family_alias_default() {
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set(
            "ZEROCLAW_providers__models__anthropic__default__api_key",
            "sk-ant-fixture",
        );

        let mut config = Config::default();
        let applied = apply_env_overrides(&mut config).expect("apply succeeds");

        assert!(
            applied
                .paths
                .contains("providers.models.anthropic.default.api_key"),
            "kebab-translated path should be recorded: {:?}",
            applied.paths,
        );
        // Secret field round-trips through set_prop into the typed alias.
        assert_eq!(
            config
                .providers
                .models
                .anthropic
                .get("default")
                .and_then(|c| c.base.api_key.as_deref()),
            Some("sk-ant-fixture"),
        );
    }

    #[tokio::test]
    async fn walker_accepts_alias_with_underscore() {
        let _guard = super::env_test_lock().await;
        let _v1 = EnvVarGuard::set(
            "ZEROCLAW_providers__models__openrouter__prod_v2__api_key",
            "sk-or-fixture",
        );
        let _v2 = EnvVarGuard::set(
            "ZEROCLAW_providers__models__openrouter__prod_v2__model",
            "anthropic/claude-sonnet-4-6",
        );

        let mut config = Config::default();
        let applied = apply_env_overrides(&mut config).expect("apply succeeds");

        assert!(
            applied
                .paths
                .contains("providers.models.openrouter.prod_v2.api_key"),
        );
        assert!(
            applied
                .paths
                .contains("providers.models.openrouter.prod_v2.model"),
        );
        let entry = config
            .providers
            .models
            .openrouter
            .get("prod_v2")
            .expect("alias created");
        assert_eq!(entry.base.api_key.as_deref(), Some("sk-or-fixture"));
        assert_eq!(
            entry.base.model.as_deref(),
            Some("anthropic/claude-sonnet-4-6"),
        );
    }

    #[tokio::test]
    async fn walker_resolves_non_map_gateway_path() {
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set("ZEROCLAW_gateway__request_timeout_secs", "120");

        let mut config = Config::default();
        let applied = apply_env_overrides(&mut config).expect("apply succeeds");

        assert!(applied.paths.contains("gateway.request_timeout_secs"));
        assert_eq!(config.gateway.request_timeout_secs, 120);
    }

    #[tokio::test]
    async fn walker_rejects_unknown_path() {
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set("ZEROCLAW_no__such__field", "x");

        let mut config = Config::default();
        let err = apply_env_overrides(&mut config).expect_err("must hard-error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ZEROCLAW_no__such__field") && msg.contains("did not resolve"),
            "error must name the env var and the failure: {msg}",
        );
    }

    #[tokio::test]
    async fn walker_propagates_alias_validator_error() {
        let _guard = super::env_test_lock().await;
        // `_invalid` starts with `_`, which the alias validator rejects.
        // The walker's tail filter accepts `[a-z0-9_]+` so this gets past
        // the prefilter, and the failure must surface as the validator's
        // specific message — not a generic "Unknown property".
        let _v = EnvVarGuard::set(
            "ZEROCLAW_providers__models__anthropic___invalid__api_key",
            "x",
        );

        let mut config = Config::default();
        let err = apply_env_overrides(&mut config).expect_err("must hard-error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("invalid alias") && msg.contains("_invalid"),
            "error must surface the alias validator's message: {msg}",
        );
    }

    #[tokio::test]
    async fn mask_restores_pre_override_snapshot_for_non_secret() {
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set("ZEROCLAW_gateway__request_timeout_secs", "999");

        let mut config = Config::default();
        let original_timeout = config.gateway.request_timeout_secs;
        let applied = apply_env_overrides(&mut config).expect("apply succeeds");
        assert_eq!(config.gateway.request_timeout_secs, 999);

        let mut to_save = config.clone();
        mask_env_overrides_for_save(&mut to_save, &applied.snapshots).expect("mask succeeds");
        assert_eq!(
            to_save.gateway.request_timeout_secs, original_timeout,
            "non-secret path resets to pre-override snapshot",
        );
        // In-memory config is unchanged — env value still effective for the
        // running process.
        assert_eq!(config.gateway.request_timeout_secs, 999);
    }

    #[tokio::test]
    async fn mask_restores_pre_override_plaintext_for_secret() {
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set(
            "ZEROCLAW_providers__models__anthropic__default__api_key",
            "sk-ant-from-env",
        );

        // Pre-existing alias with a real plaintext credential (the state
        // after `Config::load_or_init` calls `decrypt_secrets`).
        let mut config = Config::default();
        config
            .providers
            .models
            .ensure("anthropic", "default")
            .expect("typed slot")
            .api_key = Some("sk-ant-on-disk".to_string());

        let applied = apply_env_overrides(&mut config).expect("apply succeeds");
        assert!(
            applied
                .paths
                .contains("providers.models.anthropic.default.api_key"),
        );
        // Env value is live in memory.
        assert_eq!(
            config
                .providers
                .models
                .anthropic
                .get("default")
                .and_then(|c| c.base.api_key.as_deref()),
            Some("sk-ant-from-env"),
        );

        // Save-bound clone restores the pre-override plaintext, NOT the
        // display mask. This is the regression bar for the data-loss bug
        // identified inreview.
        let mut to_save = config.clone();
        mask_env_overrides_for_save(&mut to_save, &applied.snapshots).expect("mask succeeds");
        assert_eq!(
            to_save
                .providers
                .models
                .anthropic
                .get("default")
                .and_then(|c| c.base.api_key.as_deref()),
            Some("sk-ant-on-disk"),
            "secret resets to pre-override plaintext (not the `**** (encrypted)` mask)",
        );
        assert_ne!(
            to_save
                .providers
                .models
                .anthropic
                .get("default")
                .and_then(|c| c.base.api_key.as_deref()),
            Some("**** (encrypted)"),
            "must not corrupt the field with the display mask",
        );
    }

    #[tokio::test]
    async fn schema_version_override_rejected() {
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set("ZEROCLAW_schema_version", "99");

        let mut config = Config::default();
        let err = apply_env_overrides(&mut config).expect_err("must hard-error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("schema_version") && msg.contains("not overridable"),
            "error must name the path and the reason: {msg}",
        );
    }

    // ── resolve_openai_stt_api_key tests ──────────────────────────

    #[tokio::test]
    async fn resolver_returns_config_key_when_set() {
        let _guard = super::env_test_lock().await;
        let _fixture = LegacyEnvFixture::isolate();
        let _v = EnvVarGuard::set("TRANSCRIPTION_API_KEY", "sk-env");
        assert_eq!(
            resolve_openai_stt_api_key(Some("sk-config")),
            Some("sk-config".to_string()),
        );
    }

    #[tokio::test]
    async fn resolver_returns_transcription_api_key_env() {
        let _guard = super::env_test_lock().await;
        let _fixture = LegacyEnvFixture::isolate();
        let _v = EnvVarGuard::set("TRANSCRIPTION_API_KEY", "sk-transcription-test");
        assert_eq!(
            resolve_openai_stt_api_key(None),
            Some("sk-transcription-test".to_string()),
        );
    }

    #[tokio::test]
    async fn resolver_returns_openai_api_key_env() {
        let _guard = super::env_test_lock().await;
        let _fixture = LegacyEnvFixture::isolate();
        let _v = EnvVarGuard::set("OPENAI_API_KEY", "sk-openai-test");
        assert_eq!(
            resolve_openai_stt_api_key(None),
            Some("sk-openai-test".to_string()),
        );
    }

    #[tokio::test]
    async fn resolver_config_wins_over_env() {
        let _guard = super::env_test_lock().await;
        let _fixture = LegacyEnvFixture::isolate();
        let _v = EnvVarGuard::set("TRANSCRIPTION_API_KEY", "sk-env-value");
        assert_eq!(
            resolve_openai_stt_api_key(Some("sk-config-value")),
            Some("sk-config-value".to_string()),
        );
    }

    #[tokio::test]
    async fn resolver_empty_config_key_falls_back_to_env() {
        let _guard = super::env_test_lock().await;
        let _fixture = LegacyEnvFixture::isolate();
        let _v = EnvVarGuard::set("TRANSCRIPTION_API_KEY", "sk-from-env");
        assert_eq!(
            resolve_openai_stt_api_key(Some("")),
            Some("sk-from-env".to_string()),
        );
        assert_eq!(
            resolve_openai_stt_api_key(Some("   ")),
            Some("sk-from-env".to_string()),
        );
    }

    #[tokio::test]
    async fn resolver_no_env_and_no_config_returns_none() {
        let _guard = super::env_test_lock().await;
        let _fixture = LegacyEnvFixture::isolate();
        assert_eq!(resolve_openai_stt_api_key(None), None);
        assert_eq!(resolve_openai_stt_api_key(Some("")), None);
    }

    #[tokio::test]
    async fn resolver_skips_empty_env_values() {
        let _guard = super::env_test_lock().await;
        let _fixture = LegacyEnvFixture::isolate();
        let _v = EnvVarGuard::set("TRANSCRIPTION_API_KEY", "   ");
        assert_eq!(resolve_openai_stt_api_key(None), None);
    }

    // ── apply_legacy_env_fallbacks tests ──────────────────────────

    #[tokio::test]
    async fn legacy_fallback_no_env_does_nothing() {
        let _guard = super::env_test_lock().await;
        let _fixture = LegacyEnvFixture::isolate();

        let mut config = Config::default();
        assert!(
            config.transcription.openai.is_none(),
            "precondition: openai must be None"
        );
        let results = apply_legacy_env_fallbacks(&mut config);
        assert!(results.is_empty(), "no fallbacks should apply: {results:?}");
        // Must NOT materialize the Option field when no env var is set.
        assert!(
            config.transcription.openai.is_none(),
            "must not create [transcription.openai] when no env var is set"
        );
    }

    #[tokio::test]
    async fn legacy_fallback_does_not_mutate_config() {
        let _guard = super::env_test_lock().await;
        let _fixture = LegacyEnvFixture::isolate();
        let _v = EnvVarGuard::set("TRANSCRIPTION_API_KEY", "sk-env-value");

        let mut config = Config::default();
        config.transcription.openai = Some(crate::schema::OpenAiSttConfig {
            api_key: Some("sk-config-value".to_string()),
            model: "whisper-1".to_string(),
        });

        let results = apply_legacy_env_fallbacks(&mut config);
        assert!(
            results.is_empty(),
            "must return empty vec (no injection): {results:?}"
        );
        assert_eq!(
            config
                .transcription
                .openai
                .as_ref()
                .unwrap()
                .api_key
                .as_deref(),
            Some("sk-config-value"),
            "config must remain unmodified",
        );
        // The table must NOT be materialized when it starts as None.
        let mut cfg2 = Config::default();
        assert!(cfg2.transcription.openai.is_none());
        let _ = apply_legacy_env_fallbacks(&mut cfg2);
        assert!(
            cfg2.transcription.openai.is_none(),
            "must not materialize [transcription.openai]",
        );
    }

    #[tokio::test]
    async fn env_only_path_default_model_is_whisper_1() {
        let _guard = super::env_test_lock().await;
        let _fixture = LegacyEnvFixture::isolate();

        let mut config = Config::default();
        // No [transcription.openai] table at all.
        assert!(config.transcription.openai.is_none());
        // `apply_legacy_env_fallbacks` must not materialize the table.
        let results = apply_legacy_env_fallbacks(&mut config);
        assert!(results.is_empty());
        assert!(config.transcription.openai.is_none());

        // The Default impl on OpenAiSttConfig must produce model = "whisper-1"
        // (not an empty String).
        let default_cfg = crate::schema::OpenAiSttConfig::default();
        assert_eq!(default_cfg.model, "whisper-1");
        assert!(default_cfg.api_key.is_none());
    }
}
