//! V0.8.0 env-var override mechanism.

use crate::schema::Config;
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

const PREFIX: &str = "ZEROCLAW_";
const SEP: &str = "__";

/// Retired config surfaces recognized by exact env-form prefix BEFORE the
/// unknown-path rejection. Each hit is IGNORED (never applied) and reported
/// as a structured deprecation warning so existing deployments setting
/// these env vars keep booting instead of failing config load after the
/// backing schema was deleted. This is deliberately a narrow prefix
/// carve-out, not a relaxation of the walker: every other walker-eligible
/// (conforming lowercase-tail) unknown env path still hard-errors; tails
/// that fail the lowercase prefilter were never overrides to begin with.
///
/// The prefix set is derived from the single retired-surface table
/// (`RETIRED_CONFIG_SURFACES`): dotted path → env form with a trailing
/// `__`, so the env shim and the file-section shim cannot drift apart.
/// Sunset: these tombstones are compatibility shims; they will be removed
/// in a later announced window, after which the paths hard-error like any
/// other unknown path.
static RETIRED_ENV_TOMBSTONE_PREFIXES: LazyLock<Vec<(String, &'static str)>> =
    LazyLock::new(|| {
        crate::validation_warnings::RETIRED_CONFIG_SURFACES
            .iter()
            .map(|(path, code)| (format!("{}{SEP}", path.replace('.', SEP)), *code))
            .collect()
    });

/// One retired env path pattern. `segments` come from the dotted retired
/// path split on the env separator; a `*` segment (from the file lane's
/// map-key wildcard) matches exactly one env segment. Every pattern is
/// prefix-matching: children under a retired section AND children under a
/// retired field (e.g. `delegation_policy__mode`) are ignored with the
/// same warning instead of erroring as unknown paths.
struct RetiredEnvPattern {
    segments: Vec<&'static str>,
    code: &'static str,
    prefix: bool,
}

/// Field-lane patterns derived from `RETIRED_CONFIG_FIELDS`, so an env
/// override naming a removed field is ignored with the same structured
/// warning as the file section/field shims instead of hard-erroring as an
/// unknown path. Same SSOT rationale as the section prefixes above.
static RETIRED_ENV_FIELD_PATTERNS: LazyLock<Vec<RetiredEnvPattern>> = LazyLock::new(|| {
    crate::validation_warnings::RETIRED_CONFIG_FIELDS
        .iter()
        .map(|(path, code)| RetiredEnvPattern {
            segments: path.split('.').collect(),
            code,
            prefix: true,
        })
        .collect()
});

/// Match one retired env pattern against the `__`-split env tail.
fn retired_env_pattern_matches(pattern: &RetiredEnvPattern, tail_segments: &[&str]) -> bool {
    let length_ok = if pattern.prefix {
        tail_segments.len() >= pattern.segments.len()
    } else {
        tail_segments.len() == pattern.segments.len()
    };
    length_ok
        && pattern
            .segments
            .iter()
            .zip(tail_segments)
            .all(|(expected, actual)| *expected == "*" || expected.eq_ignore_ascii_case(actual))
}

/// Test-visible view of the derived env-prefix set, so the SSOT guard can
/// assert the walker carries a prefix for every retired surface.
#[cfg(test)]
pub(crate) fn retired_env_tombstone_prefixes() -> &'static [(String, &'static str)] {
    &RETIRED_ENV_TOMBSTONE_PREFIXES
}

static NON_OVERRIDABLE_PATHS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| HashSet::from(["schema_version"]));

#[derive(Debug, Default, Clone)]
pub struct AppliedOverrides {
    pub paths: HashSet<String>,
    pub snapshots: HashMap<String, String>,
    /// Structured deprecation warnings for retired-surface env hits that
    /// were ignored instead of applied. Callers attach these to
    /// `Config::retired_surface_warnings` so `collect_warnings()` replays
    /// them through the stable-code warning machinery.
    pub tombstone_warnings: Vec<crate::validation_warnings::ValidationWarning>,
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
    let mut tombstone_warnings: Vec<crate::validation_warnings::ValidationWarning> = Vec::new();
    for (env_name, value, tail) in entries {
        // Retired-surface tombstone: exact-prefix carve-out consulted
        // BEFORE the unknown-path rejection (so no map-key section can
        // intercept first). The hit is ignored and warned; it must never
        // be applied (the schema no longer has a destination) and must
        // never mask an otherwise-unknown path. The apply-time log is
        // deliberate dual emission with the collect_warnings replay: it
        // covers loaders that never call validate()/collect_warnings
        // (the channels standalone reload) and fires even when a later
        // entry hard-errors and the structured vec is discarded with the
        // Err — deprecation visibility should not depend on the rest of
        // the environment being clean.
        let tail_segments: Vec<&str> = tail.split(SEP).collect();
        if tail_segments.starts_with(&["providers", "models"])
            && let Some(family) = tail_segments.get(2)
        {
            crate::providers::reject_retired_model_provider(family)?;
        }
        let retired_hit = RETIRED_ENV_TOMBSTONE_PREFIXES
            .iter()
            .find(|(prefix, _)| tail.starts_with(prefix.as_str()))
            .map(|(prefix, code)| {
                let dotted = prefix.trim_end_matches(SEP).replace(SEP, ".");
                (dotted, *code)
            })
            .or_else(|| {
                RETIRED_ENV_FIELD_PATTERNS
                    .iter()
                    .find(|pattern| retired_env_pattern_matches(pattern, &tail_segments))
                    .map(|pattern| {
                        let dotted: Vec<&str> = tail_segments
                            .iter()
                            .zip(&pattern.segments)
                            .map(
                                |(actual, expected)| {
                                    if *expected == "*" { *actual } else { expected }
                                },
                            )
                            .collect();
                        (dotted.join("."), pattern.code)
                    })
            });
        if let Some((dotted, code)) = retired_hit {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"env_var": env_name, "path": dotted})),
                "env override for retired config surface ignored"
            );
            tombstone_warnings.push(crate::validation_warnings::ValidationWarning::new(
                code,
                format!(
                    "{env_name} targets the retired `[{dotted}]` config key and is \
                     ignored. The key was removed from the schema and has no runtime \
                     consumer; this compatibility shim will be removed in a later \
                     announced window. Remove the env var."
                ),
                dotted.clone(),
            ));
            continue;
        }
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
    Ok(AppliedOverrides {
        paths,
        snapshots,
        tombstone_warnings,
    })
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
    let toml::Value::Table(full_table) = toml::Value::try_from(source).ok()? else {
        return None;
    };
    let current = crate::schema::lookup_prop_value_in_table(&full_table, path)?;
    Some(match current {
        toml::Value::String(s) => s.clone(),
        toml::Value::Array(_) => serde_json::to_string(current).ok()?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Config;

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

    #[tokio::test]
    async fn walker_rejects_retired_model_provider_credentials_without_applying_them() {
        let _guard = super::env_test_lock().await;
        for name in [
            "ZEROCLAW_providers__models__bedrock__private_alias__api_key",
            "ZEROCLAW_providers__models__azure__private_alias__api_key",
            "ZEROCLAW_providers__models__azure_openai__private_alias__api_key",
            "ZEROCLAW_providers__models__telnyx__private_alias__api_key",
            "ZEROCLAW_providers__models__gemini_cli__private_alias__binary_path",
            "ZEROCLAW_providers__models__kilocli__private_alias__binary_path",
        ] {
            let _value = EnvVarGuard::set(name, "private-test-value");
            let mut config = Config::default();
            let error = apply_env_overrides(&mut config).unwrap_err().to_string();
            assert!(error.contains("is retired"), "{name}: {error}");
            assert!(!error.contains("private-test-value"));
            assert!(!error.contains("private_alias"));
            assert!(config.providers.models.iter_entries().next().is_none());
        }
        let _value = EnvVarGuard::set(
            "ZEROCLAW_providers__models__gemini__gemini_cli__oauth_project",
            "retained-project",
        );
        let mut config = Config::default();
        apply_env_overrides(&mut config).unwrap();
        assert_eq!(
            config.providers.models.gemini["gemini_cli"]
                .oauth_project
                .as_deref(),
            Some("retained-project")
        );
    }

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
    async fn walker_ignores_retired_pairing_dashboard_prefix_with_warning() {
        // The discrimination that prevents global loosening: the retired
        // section's env prefix is ignored + warned, while every OTHER
        // unknown gateway path still hard-errors (next test). Before the
        // tombstone, this var hard-errored (path unknown after schema
        // removal); deployments setting it must keep booting with a
        // structured deprecation warning instead.
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set("ZEROCLAW_gateway__pairing_dashboard__anything", "1");

        let mut config = Config::default();
        let applied =
            apply_env_overrides(&mut config).expect("retired prefix must be ignored, not fatal");
        assert!(
            applied.paths.is_empty(),
            "tombstoned var must not be applied: {:?}",
            applied.paths
        );
        assert_eq!(
            applied.tombstone_warnings.len(),
            1,
            "exactly one deprecation warning per hit: {:?}",
            applied.tombstone_warnings
        );
        let warning = &applied.tombstone_warnings[0];
        assert_eq!(warning.code, "gateway_pairing_dashboard_removed");
        assert_eq!(warning.path, "gateway.pairing_dashboard");
        assert!(
            warning
                .message
                .contains("ZEROCLAW_gateway__pairing_dashboard__anything"),
            "warning must name the env var: {}",
            warning.message
        );
        assert!(
            warning.message.contains("later announced window"),
            "warning must name the sunset intent: {}",
            warning.message
        );

        // The structured warning flows through the validation_warnings
        // machinery once attached, mirroring the otp_action_gating
        // precedent.
        config.retired_surface_warnings = applied.tombstone_warnings;
        assert!(
            config
                .collect_warnings()
                .iter()
                .any(|w| w.code == "gateway_pairing_dashboard_removed"
                    && w.path == "gateway.pairing_dashboard"),
            "collect_warnings must replay the tombstone warning"
        );
    }

    #[tokio::test]
    async fn walker_still_hard_errors_on_unknown_gateway_paths() {
        // Guard against the tombstone becoming a relaxation: only the
        // exact retired prefix is carved out; adjacent unknown paths under
        // `gateway` keep hard-erroring.
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set("ZEROCLAW_gateway__anything_else__foo", "1");

        let mut config = Config::default();
        let err = apply_env_overrides(&mut config).expect_err("must hard-error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("ZEROCLAW_gateway__anything_else__foo") && msg.contains("did not resolve"),
            "error must name the env var and the failure: {msg}",
        );
    }

    #[tokio::test]
    async fn walker_tombstone_prefix_is_exact_not_a_name_prefix() {
        // `gateway__pairing_dashboard_something` differs from the retired
        // prefix by a single separator: it is a DIFFERENT (unknown) path
        // and must still hard-error. The tombstone requires the full
        // `__`-terminated prefix, so it cannot swallow longer section
        // names that merely start with the same words.
        let _guard = super::env_test_lock().await;
        let _v = EnvVarGuard::set("ZEROCLAW_gateway__pairing_dashboard_something__x", "1");

        let mut config = Config::default();
        let err = apply_env_overrides(&mut config).expect_err("must hard-error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("did not resolve"),
            "prefix look-alike must hard-error: {msg}",
        );
    }

    #[tokio::test]
    async fn walker_tombstone_covers_bare_prefix_and_former_real_keys() {
        // The prefix match includes the exact bare form (empty remainder)
        // and every former real sub-key of the retired section; each hit
        // warns once and none is applied.
        let _guard = super::env_test_lock().await;
        let _v1 = EnvVarGuard::set("ZEROCLAW_gateway__pairing_dashboard__", "1");
        let _v2 = EnvVarGuard::set("ZEROCLAW_gateway__pairing_dashboard__code_length", "9");
        let _v3 = EnvVarGuard::set("ZEROCLAW_gateway__pairing_dashboard__nested__deep", "7");

        let mut config = Config::default();
        let applied = apply_env_overrides(&mut config).expect("all hits ignored, not fatal");
        assert!(
            applied.paths.is_empty(),
            "no tombstoned var may be applied: {:?}",
            applied.paths
        );
        assert_eq!(
            applied.tombstone_warnings.len(),
            3,
            "one warning per hit: {:?}",
            applied.tombstone_warnings
        );
        assert!(
            applied
                .tombstone_warnings
                .iter()
                .all(|w| w.code == "gateway_pairing_dashboard_removed"),
            "all hits carry the stable code"
        );
    }

    #[tokio::test]
    async fn walker_field_tombstone_ignores_retired_delegate_keys() {
        // Field-lane tombstones (RETIRED_CONFIG_FIELDS): an env override
        // naming a removed delegate config field is ignored with the
        // stable structured warning — symmetric with the file lane — and
        // must not hard-error as an unknown path. The wildcard matches the
        // actual alias segment; the warning path records it verbatim.
        let _guard = super::env_test_lock().await;
        let _v1 = EnvVarGuard::set("ZEROCLAW_agents__lead__delegates", "peer");
        let _v2 = EnvVarGuard::set("ZEROCLAW_agents__lead__delegate_same_risk_profile", "true");
        // A child key under the retired field is ignored by the same
        // prefix semantics as the section lane, not a hard error.
        let _v3 = EnvVarGuard::set(
            "ZEROCLAW_risk_profiles__ops__delegation_policy__mode",
            "allow",
        );

        let mut config = Config::default();
        let applied = apply_env_overrides(&mut config).expect("field hits ignored, not fatal");
        assert!(
            applied.paths.is_empty(),
            "no tombstoned var may be applied: {:?}",
            applied.paths
        );
        assert_eq!(
            applied.tombstone_warnings.len(),
            3,
            "one warning per hit: {:?}",
            applied.tombstone_warnings
        );
        let mut paths: Vec<_> = applied
            .tombstone_warnings
            .iter()
            .map(|w| w.path.as_str())
            .collect();
        paths.sort_unstable();
        assert_eq!(
            paths,
            vec![
                "agents.lead.delegate_same_risk_profile",
                "agents.lead.delegates",
                "risk_profiles.ops.delegation_policy"
            ]
        );
        assert!(
            applied
                .tombstone_warnings
                .iter()
                .all(|w| w.code == "delegate_config_removed"),
            "all hits carry the stable code"
        );
    }

    #[tokio::test]
    async fn walker_tombstones_native_ollama_options_without_hiding_live_aliases() {
        let _guard = super::env_test_lock().await;
        let _ctx = EnvVarGuard::set(
            "ZEROCLAW_providers__models__ollama__local__num_ctx",
            "16384",
        );
        let _predict = EnvVarGuard::set(
            "ZEROCLAW_providers__models__ollama__local__num_predict",
            "4096",
        );
        let _temperature = EnvVarGuard::set(
            "ZEROCLAW_providers__models__ollama__retired_only__temperature_override",
            "0.5",
        );
        let _model = EnvVarGuard::set("ZEROCLAW_providers__models__ollama__local__model", "qwen3");
        // A retired leaf's spelling must not retire an alias with that name.
        let _alias = EnvVarGuard::set(
            "ZEROCLAW_providers__models__ollama__num_ctx__model",
            "llama3.1",
        );

        let mut config = Config::default();
        let applied = apply_env_overrides(&mut config).unwrap();
        assert_eq!(applied.paths.len(), 2);
        assert_eq!(applied.tombstone_warnings.len(), 3);
        assert!(
            applied
                .tombstone_warnings
                .iter()
                .all(|warning| warning.code == "ollama_native_options_removed")
        );
        assert_eq!(
            config.providers.models.ollama["local"]
                .base
                .model
                .as_deref(),
            Some("qwen3")
        );
        assert_eq!(
            config.providers.models.ollama["num_ctx"]
                .base
                .model
                .as_deref(),
            Some("llama3.1")
        );
        assert!(!config.providers.models.ollama.contains_key("retired_only"));
    }

    #[tokio::test]
    async fn walker_tombstones_cost_selector_under_runtime_profile() {
        let _guard = super::env_test_lock().await;
        let _retired = EnvVarGuard::set(
            "ZEROCLAW_runtime_profiles__fast__auto_classify__cost_optimized_hint",
            "legacy-cost-selector",
        );
        let _live = EnvVarGuard::set(
            "ZEROCLAW_runtime_profiles__fast__auto_classify__simple_hint",
            "cost-optimized",
        );
        let mut config = Config::default();
        config.runtime_profiles.insert(
            "fast".into(),
            crate::schema::RuntimeProfileConfig {
                auto_classify: Some(Default::default()),
                ..Default::default()
            },
        );
        let applied = apply_env_overrides(&mut config).unwrap();
        assert_eq!(applied.paths.len(), 1);
        assert_eq!(applied.tombstone_warnings.len(), 1);
        assert_eq!(
            applied.tombstone_warnings[0].code,
            "cost_optimized_routing_removed"
        );
        assert_eq!(
            applied.tombstone_warnings[0].path,
            "runtime_profiles.fast.auto_classify.cost_optimized_hint"
        );
        let hints = config.runtime_profiles["fast"]
            .auto_classify
            .as_ref()
            .unwrap();
        assert_eq!(hints.simple_hint.as_deref(), Some("cost-optimized"));
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
}
