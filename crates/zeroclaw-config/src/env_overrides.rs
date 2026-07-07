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

/// Legacy provider-native env-var fallbacks applied after
/// [`apply_env_overrides`] so that `ZEROCLAW_*` values always win.
///
/// Maps `TRANSCRIPTION_API_KEY` → all known openai STT config paths, and
/// `OPENAI_API_KEY` as a secondary fallback. Unlike the `ZEROCLAW_*`
/// grammar these are **not** schema-derived — they are a curated list of
/// well-known env vars that operators may already have set.
///
/// The value is injected into:
///
/// 1. The legacy `[transcription.openai].api_key` field.
/// 2. Every existing typed alias under `[providers.transcription.openai.<alias>]`
///    whose `base.api_key` is currently unset.
///
/// Each injection uses the same snapshot / mask machinery as
/// [`apply_env_overrides`] so env-injected values never leak to disk on
/// `save()`.  Explicit config and `ZEROCLAW_*` overrides take priority
/// because injection is skipped when the target field is already populated.
///
/// Returns the set of paths actually overridden this way. Callers merge
/// these into the config's `pre_override_snapshots` and
/// `env_overridden_paths`.
pub fn apply_legacy_env_fallbacks(config: &mut Config) -> Vec<(String, String)> {
    /// Env var names to check, in priority order.
    const ENV_NAMES: &[&str] = &["TRANSCRIPTION_API_KEY", "OPENAI_API_KEY"];

    let mut results: Vec<(String, String)> = Vec::new();

    // Check whether any env var exists and has content.
    let value = find_first_env(ENV_NAMES);
    let Some(value) = value else {
        return results;
    };

    // ── 1. Legacy path: transcription.openai.api_key ──────────────
    // Ensure the Option field is initialized — set_prop cannot drill into
    // a None Option<T> field on Configurable derive types.
    let legacy_path = "transcription.openai.api_key";
    let current = raw_value_for_path(config, legacy_path);
    if !matches!(current.as_deref(), Some(v) if !v.is_empty()) {
        config
            .transcription
            .openai
            .get_or_insert_with(Default::default);
        inject_into(config, legacy_path, &value, &mut results);
    }

    // ── 2. Typed paths: providers.transcription.openai.<alias>.api_key ──
    // Note: api_key is a flattened field from TranscriptionProviderConfig,
    // so it appears at the alias level (without `base.` prefix) in the
    // Configurable derive's prop_fields and set_prop routing.
    // Collect keys first to avoid borrow conflict with inject_into.
    let typed_aliases: Vec<String> = config
        .providers
        .transcription
        .openai
        .keys()
        .cloned()
        .collect();
    for alias in &typed_aliases {
        let typed_path = format!("providers.transcription.openai.{alias}.api_key");
        let current = raw_value_for_path(config, &typed_path);
        if !matches!(current.as_deref(), Some(v) if !v.is_empty()) {
            inject_into(config, &typed_path, &value, &mut results);
        }
    }

    results
}

/// Helper: snapshot, set_prop, and record one injection.
fn inject_into(config: &mut Config, path: &str, value: &str, results: &mut Vec<(String, String)>) {
    let snapshot = raw_value_for_path(config, path).unwrap_or_default();
    if let Err(e) = config.set_prop(path, value) {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(::serde_json::json!({"path": path, "error": format!("{}", e)})),
            "Legacy env fallback: set_prop failed"
        );
        return;
    }
    results.push((path.to_string(), snapshot));

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({"path": path})),
        "Legacy env fallback applied"
    );
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

    // ── apply_legacy_env_fallbacks tests ──────────────────────────

    #[tokio::test]
    async fn legacy_fallback_transcription_api_key_sets_config() {
        let _guard = super::env_test_lock().await;
        let _fixture = LegacyEnvFixture::isolate();
        let _v = EnvVarGuard::set("TRANSCRIPTION_API_KEY", "sk-transcription-test");

        let mut config = Config::default();
        let results = apply_legacy_env_fallbacks(&mut config);

        // Ensure the env var was mapped to the config path.
        let path = "transcription.openai.api_key";
        assert!(
            results.iter().any(|(p, _)| p == path),
            "expected {path} in results: {results:?}"
        );
        // The snapshot should be empty (field was None before injection).
        let snapshot = results.iter().find(|(p, _)| p == path).unwrap().1.clone();
        assert_eq!(snapshot, "", "pre-override snapshot must be empty");

        // The config field must now carry the value.
        assert_eq!(
            config
                .transcription
                .openai
                .as_ref()
                .unwrap()
                .api_key
                .as_deref(),
            Some("sk-transcription-test"),
        );
    }

    #[tokio::test]
    async fn legacy_fallback_openai_api_key_sets_config() {
        let _guard = super::env_test_lock().await;
        let _fixture = LegacyEnvFixture::isolate();
        let _v = EnvVarGuard::set("OPENAI_API_KEY", "sk-openai-test");

        let mut config = Config::default();
        let results = apply_legacy_env_fallbacks(&mut config);

        let path = "transcription.openai.api_key";
        assert!(
            results.iter().any(|(p, _)| p == path),
            "expected {path} in results: {results:?}"
        );

        assert_eq!(
            config
                .transcription
                .openai
                .as_ref()
                .unwrap()
                .api_key
                .as_deref(),
            Some("sk-openai-test"),
        );
    }

    #[tokio::test]
    async fn legacy_fallback_zoclaw_and_legacy_are_independent() {
        let _guard = super::env_test_lock().await;
        let _fixture = LegacyEnvFixture::isolate();
        // ZEROCLAW_* through the typed provider path sets
        // providers.transcription.openai.default.api_key.
        // TRANSCRIPTION_API_KEY sets transcription.openai.api_key.
        // They are different config fields — both should apply.
        let _v1 = EnvVarGuard::set(
            "ZEROCLAW_providers__transcription__openai__default__api_key",
            "sk-pipeline-value",
        );
        let _v2 = EnvVarGuard::set("TRANSCRIPTION_API_KEY", "sk-legacy-value");

        let mut config = Config::default();
        // Ensure the typed provider alias slot exists.
        config.providers.transcription.openai.insert(
            "default".to_string(),
            crate::schema::OpenAiTranscriptionProviderConfig {
                base: crate::schema::TranscriptionProviderConfig {
                    api_key: None,
                    ..Default::default()
                },
                model: None,
            },
        );
        // apply_env_overrides first (handles ZEROCLAW_* via typed path)
        let _applied = apply_env_overrides(&mut config).expect("apply succeeds");
        // then legacy fallbacks
        let results = apply_legacy_env_fallbacks(&mut config);

        // The typed provider slot received the ZEROCLAW_* value.
        assert_eq!(
            config
                .providers
                .transcription
                .openai
                .get("default")
                .and_then(|c| c.base.api_key.as_deref()),
            Some("sk-pipeline-value"),
            "ZEROCLAW_* value must reach the typed provider slot",
        );
        // The legacy config field received TRANSCRIPTION_API_KEY.
        let path = "transcription.openai.api_key";
        assert!(
            results.iter().any(|(p, _)| p == path),
            "legacy fallback should apply to {path}: {results:?}"
        );
        assert_eq!(
            config
                .transcription
                .openai
                .as_ref()
                .unwrap()
                .api_key
                .as_deref(),
            Some("sk-legacy-value"),
        );
    }

    #[tokio::test]
    async fn legacy_fallback_injects_into_typed_alias() {
        let _guard = super::env_test_lock().await;
        let _fixture = LegacyEnvFixture::isolate();
        let _v = EnvVarGuard::set("TRANSCRIPTION_API_KEY", "sk-typed-inject");

        let mut config = Config::default();
        // Pre-create a typed alias with unset api_key.
        config.providers.transcription.openai.insert(
            "default".to_string(),
            crate::schema::OpenAiTranscriptionProviderConfig {
                base: crate::schema::TranscriptionProviderConfig {
                    api_key: None,
                    ..Default::default()
                },
                model: None,
            },
        );

        let results = apply_legacy_env_fallbacks(&mut config);

        // Legacy path should be injected.
        let legacy_path = "transcription.openai.api_key";
        assert!(
            results.iter().any(|(p, _)| p == legacy_path),
            "expected {legacy_path} in results: {results:?}"
        );
        assert_eq!(
            config
                .transcription
                .openai
                .as_ref()
                .unwrap()
                .api_key
                .as_deref(),
            Some("sk-typed-inject"),
        );

        // Typed path should also be injected.
        let typed_path = "providers.transcription.openai.default.api_key";
        assert!(
            results.iter().any(|(p, _)| p == typed_path),
            "expected {typed_path} in results: {results:?}"
        );
        assert_eq!(
            config
                .providers
                .transcription
                .openai
                .get("default")
                .and_then(|c| c.base.api_key.as_deref()),
            Some("sk-typed-inject"),
        );
    }

    #[tokio::test]
    async fn legacy_fallback_does_not_override_explicit_config() {
        let _guard = super::env_test_lock().await;
        let _fixture = LegacyEnvFixture::isolate();
        let _v = EnvVarGuard::set("TRANSCRIPTION_API_KEY", "sk-env-value");

        let mut config = Config::default();
        // Simulate an already-populated config
        config.transcription.openai = Some(crate::schema::OpenAiSttConfig {
            api_key: Some("sk-config-value".to_string()),
            model: "whisper-1".to_string(),
        });

        let results = apply_legacy_env_fallbacks(&mut config);
        assert!(
            results.is_empty(),
            "must not override explicit config: {results:?}"
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
        );
    }

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
        // Must NOT initialize the Option field when no env var is set.
        assert!(
            config.transcription.openai.is_none(),
            "must not create [transcription.openai] when no env var is set"
        );
    }
}
