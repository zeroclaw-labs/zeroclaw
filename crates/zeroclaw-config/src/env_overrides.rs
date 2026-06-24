//! V0.8.0 env-var override mechanism.
//!
//! Grammar: `ZEROCLAW_<dotted_path_with_double_underscores>=<value>`.
//! Each `__` (double underscore) is a path separator (`.` in the TOML); each
//! single `_` is either a snake-case joiner inside a field name (which the
//! walker converts to kebab `-` for `set_prop`) or a literal char inside an
//! alias key.
//!
//! Schema-derived: [`map_key_sections`] gives HashMap positions (one alias
//! token consumed; alias chars are `[a-z0-9_]`); [`prop_fields`] gives every
//! other leaf path. No string-literal pattern matching, no hardcoded family
//! names.
//!
//! Bootstrap exception: `ZEROCLAW_WORKSPACE` and `ZEROCLAW_CONFIG_DIR` keep
//! their UPPERCASE form. The case rule (lowercase tail = config-tree,
//! uppercase tail = bootstrap) does the disambiguation work without an
//! exemption list.
//!
//! Persistence boundary: each overridden path's pre-override raw value is
//! snapshotted (post-`decrypt_secrets`, so secrets are plaintext) and used
//! by [`mask_env_overrides_for_save`] to restore disk-or-default values
//! before `encrypt_secrets()` runs. Env-injected values never reach disk.
//!
//! [`map_key_sections`]: crate::schema::Config::map_key_sections
//! [`prop_fields`]: crate::schema::Config::prop_fields

use crate::schema::Config;
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

const PREFIX: &str = "ZEROCLAW_";
const SEP: &str = "__";

/// Paths that the schema exposes via `prop_fields()` but that operators must
/// not override at runtime. Currently just `schema-version` (kebab form, as
/// emitted by `prop_fields()`) — the migration engine sets it from the
/// on-disk file's value, and an env override would either skip needed
/// migrations or trigger a no-op rerun. O(1) HashSet lookup so adding more
/// reserved paths stays cheap.
static NON_OVERRIDABLE_PATHS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| HashSet::from(["schema-version"]));

/// Outcome of [`apply_env_overrides`]: the set of overridden paths plus the
/// per-path snapshot of pre-override raw values. The snapshot drives
/// [`mask_env_overrides_for_save`] so secret fields recover their original
/// plaintext (which `encrypt_secrets()` then re-encrypts), and non-secret
/// fields recover their disk-or-default value.
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
            // Inner segments are `__`-separated; each segment is a snake-case
            // field name that maps to kebab in the prop-path.
            let inner_path = inner
                .split(SEP)
                .map(|seg| seg.replace('_', "-"))
                .collect::<Vec<_>>()
                .join(".");
            format!("{}.{}.{}", section.path, alias, inner_path)
        };
        return Ok(path);
    }

    // Non-map path: prop_fields() entries are dotted with kebab fields.
    // Convert to env-form (`.` → `__`, `-` → `_`) and compare.
    config
        .prop_fields()
        .into_iter()
        .find(|f| f.name.replace('.', SEP).replace('-', "_") == tail)
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

/// Read the raw string value at a dotted (kebab-cased) prop path from a
/// serializable Config struct, bypassing the `is_secret` masking that
/// `Config::get_prop` applies. Returns `None` when the path doesn't resolve
/// (e.g. the alias entry hasn't been created yet on disk).
///
/// Walks the TOML serialization with `kebab → snake` field-name conversion
/// (HashMap aliases keep `_` natively per the validator). Used by
/// [`apply_env_overrides`] so the pre-override snapshot of a secret field
/// captures the real plaintext rather than the display mask.
fn raw_value_for_path(source: &Config, path: &str) -> Option<String> {
    let table = toml::Value::try_from(source).ok()?;
    let mut current: &toml::Value = &table;
    for segment in path.split('.') {
        let key = segment.replace('-', "_");
        current = current.as_table()?.get(&key)?;
    }
    Some(match current {
        toml::Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

/// Restore env-overridden paths in a save-bound clone to their pre-override
/// snapshots, so env-injected values never reach `encrypt_secrets()` or the
/// on-disk TOML.
///
/// Snapshots come from [`apply_env_overrides`] which captures the
/// post-`decrypt_secrets` plaintext for secret fields. After this restore,
/// `encrypt_secrets()` re-encrypts the recovered plaintext to fresh
/// ciphertext that decrypts to the same value — preserving the operator's
/// real on-disk credential across env-override + save cycles.
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

/// Process-wide lock for env-mutating tests. Both `env_overrides::tests`
/// and `schema::tests` race on `ZEROCLAW_*` env vars and must serialize on
/// the same mutex; defining it once here and re-exporting `pub(crate)`
/// keeps a single coordinator. `#[cfg(test)]` so it never lands in
/// production builds.
#[cfg(test)]
pub(crate) async fn env_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Config;

    /// RAII-ish helper: removes the named ZEROCLAW_* var on drop so failed
    /// asserts don't leak state into sibling tests.
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
                .contains("providers.models.anthropic.default.api-key"),
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
                .contains("providers.models.openrouter.prod_v2.api-key"),
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

        assert!(applied.paths.contains("gateway.request-timeout-secs"));
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
                .contains("providers.models.anthropic.default.api-key"),
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
        // identified in PR #6523 review.
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
