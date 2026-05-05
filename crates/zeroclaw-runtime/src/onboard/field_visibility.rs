//! Per-section field visibility + default-injection helpers.
//!
//! Used by both the CLI wizard (`onboard::offer_advanced_settings`) and the
//! gateway HTTP endpoints (`/api/onboard/sections/.../items/...` for default
//! injection, `/api/config/list` for filtering). One source of truth so the
//! CLI and dashboard can't disagree about which fields apply to a given
//! provider / memory backend / etc.
//!
//! Follow-up (#5960 deferral): a `#[configurable(family = "...")]` schema
//! attribute would let the `Configurable` derive emit these filters
//! automatically. Until that lands, the per-family lists below are the
//! pragmatic stand-in.
//!
//! Issue #6175.

use anyhow::Result;
use zeroclaw_config::schema::Config;

/// Exclude list for `[providers.models.<name>]` field walks.
///
/// Returns leaf field names (kebab-case, the suffix after the
/// `providers.models.<name>.` prefix in `prop_fields()`). A field absent
/// from this list is shown for every provider.
pub fn provider_family_excludes(provider: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    if !matches!(provider, "openai" | "openai_codex") {
        out.push("wire-api");
        out.push("requires-openai-auth");
    }
    out
}

/// Exclude list for the top-level `[memory]` walk based on the active backend.
///
/// `MemoryConfig` carries fields and nested subsections for every backend
/// (sqlite-only knobs, `[memory.qdrant]`, `[memory.postgres]`); only the
/// active backend's surface is relevant. Each entry is a path SUFFIX after
/// the `memory.` prefix in `prop_fields()`. Sub-table fields are matched
/// by leading segment (`qdrant.`, `postgres.`).
pub fn memory_backend_excludes(backend: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    if backend != "sqlite" {
        out.push("sqlite-open-timeout-secs");
        out.push("conversation-retention-days");
    }
    if backend != "qdrant" {
        out.push("qdrant.");
    }
    if backend != "postgres" {
        out.push("postgres.");
    }
    out
}

/// Compute the set of full property paths to hide when a client requests
/// `prefix`. Returns an empty vec for prefixes that don't have visibility
/// rules (most of the schema).
///
/// This is the single entry point the gateway's `/api/config/list` handler
/// calls — it inspects the requested prefix, looks at the live config to
/// resolve any state-dependent rules (e.g. `memory.backend`), and returns
/// the absolute paths to drop from the response.
pub fn excluded_paths(cfg: &Config, prefix: &str) -> Vec<String> {
    if let Some(provider_key) = providers_models_key(prefix) {
        // Use the full prefix as-is when it already includes the alias
        // (providers.models.<type>.<alias>); otherwise fall back to the
        // type-only prefix and let the caller pass a bare type path.
        let alias_prefix = if prefix
            .strip_prefix("providers.models.")
            .is_some_and(|rest| rest.contains('.'))
        {
            prefix.to_string()
        } else {
            format!("providers.models.{provider_key}")
        };
        return provider_family_excludes(provider_key)
            .into_iter()
            .map(|leaf| format!("{alias_prefix}.{leaf}"))
            .collect();
    }

    if prefix == "memory" || prefix.is_empty() {
        let backend = if cfg.memory.backend.is_empty() {
            "sqlite"
        } else {
            cfg.memory.backend.as_str()
        };
        return memory_backend_excludes(backend)
            .into_iter()
            .map(|leaf| {
                if leaf.ends_with('.') {
                    // Sub-table prefix — represent as a path-prefix marker
                    // that callers match with `starts_with`.
                    format!("memory.{leaf}")
                } else {
                    format!("memory.{leaf}")
                }
            })
            .collect();
    }

    Vec::new()
}

/// Test whether `path` is one of the excluded entries returned from
/// `excluded_paths`. Handles both exact matches and sub-table prefix
/// markers (`"memory.qdrant."` matches every `memory.qdrant.*`).
pub fn is_excluded(path: &str, excludes: &[String]) -> bool {
    excludes
        .iter()
        .any(|e| path == e || (e.ends_with('.') && path.starts_with(e)))
}

/// Extract the provider type key from a `providers.models.<type>` or
/// `providers.models.<type>.<alias>` prefix. Returns `None` when the
/// prefix doesn't match a per-model walk.
pub fn providers_models_key(prefix: &str) -> Option<&str> {
    let rest = prefix.strip_prefix("providers.models.")?;
    if rest.is_empty() {
        return None;
    }
    // V3: "providers.models.<type>.<alias>" — return just the type segment.
    // V2-compat: "providers.models.<type>" — return the bare segment.
    Some(rest.split_once('.').map_or(rest, |(type_key, _)| type_key))
}

/// Walk a freshly-constructed defaults struct (from
/// `zeroclaw_providers::default_provider_config`, etc.) and copy its
/// populated values into `cfg` under `target_prefix`, but only for paths
/// the user hasn't already set.
///
/// Schema-driven: enumerates fields via the source's `prop_fields()`
/// macro output. Adding a new field to the source struct propagates
/// automatically — no per-field plumbing here.
///
/// `source_prefix` is the source struct's `configurable_prefix()` (e.g.
/// `providers.models` for `ModelProviderConfig`). Each leaf gets rebased
/// onto `target_prefix` before writing.
fn apply_typed_defaults<S>(cfg: &mut Config, defaults: &S, source_prefix: &str, target_prefix: &str)
where
    S: PropFieldSource,
{
    let source_dot = format!("{source_prefix}.");
    let target_dot = format!("{target_prefix}.");
    for field in defaults.prop_fields() {
        // Only direct leaves of the defaults struct — nested sub-tables
        // need their own walks (they have their own configurable_prefix).
        let Some(leaf) = field.name.strip_prefix(&source_dot) else {
            continue;
        };
        if leaf.contains('.') {
            continue;
        }
        // Sparse defaults: an unset field on the typed default struct means
        // the source has no opinion; leave the target alone.
        if field.display_value == "<unset>" {
            continue;
        }
        let target_path = format!("{target_dot}{leaf}");
        let current = cfg.get_prop(&target_path).unwrap_or_default();
        if !current.is_empty() && current != "<unset>" {
            // User has already set this field — don't clobber.
            continue;
        }
        let _ = cfg.set_prop(&target_path, &field.display_value);
    }
}

/// Trait-object-friendly view onto any `Configurable`-derived struct's
/// `prop_fields()`. Lets `apply_typed_defaults` accept any source struct
/// without naming concrete types.
trait PropFieldSource {
    fn prop_fields(&self) -> Vec<zeroclaw_config::traits::PropFieldInfo>;
}

impl PropFieldSource for zeroclaw_config::schema::ModelProviderConfig {
    fn prop_fields(&self) -> Vec<zeroclaw_config::traits::PropFieldInfo> {
        zeroclaw_config::schema::ModelProviderConfig::prop_fields(self)
    }
}

/// Pre-populate the per-provider entry under `prefix` with the named
/// provider's trait-derived defaults. Idempotent: existing user-set
/// values aren't touched.
///
/// Source of truth is `zeroclaw_providers::default_provider_config(name)`
/// — a typed `ModelProviderConfig`. We walk its `prop_fields()` and
/// rebase each leaf onto `prefix` so e.g. the source's
/// `providers.models.base-url` becomes the target's
/// `providers.models.<key>.base-url`.
pub fn apply_provider_trait_defaults(cfg: &mut Config, provider: &str, prefix: &str) -> Result<()> {
    let defaults = zeroclaw_providers::default_provider_config(provider);
    let source_prefix = zeroclaw_config::schema::ModelProviderConfig::configurable_prefix();
    apply_typed_defaults(cfg, &defaults, source_prefix, prefix);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_specific_excludes_for_non_openai() {
        let excludes = provider_family_excludes("anthropic");
        assert!(excludes.contains(&"wire-api"));
        assert!(excludes.contains(&"requires-openai-auth"));
    }

    #[test]
    fn openai_specific_kept_for_openai_family() {
        for p in &["openai", "openai_codex"] {
            let excludes = provider_family_excludes(p);
            assert!(
                !excludes.contains(&"wire-api"),
                "wire-api should show for {p}"
            );
            assert!(
                !excludes.contains(&"requires-openai-auth"),
                "requires-openai-auth should show for {p}"
            );
        }
    }

    #[test]
    fn memory_excludes_hide_inactive_backends() {
        // sqlite active → hide qdrant + postgres subsections, keep sqlite
        // open-timeout
        let ex = memory_backend_excludes("sqlite");
        assert!(ex.contains(&"qdrant."));
        assert!(ex.contains(&"postgres."));
        assert!(!ex.contains(&"sqlite-open-timeout-secs"));
        assert!(!ex.contains(&"conversation-retention-days"));

        // qdrant active → hide sqlite-only knobs + postgres
        let ex = memory_backend_excludes("qdrant");
        assert!(!ex.contains(&"qdrant."));
        assert!(ex.contains(&"postgres."));
        assert!(ex.contains(&"sqlite-open-timeout-secs"));
        assert!(ex.contains(&"conversation-retention-days"));
    }

    #[test]
    fn excluded_paths_for_provider_prefix() {
        let cfg = Config::default();
        // Caller passes the full alias-level prefix: providers.models.<type>.<alias>.
        // The alias is whatever the user named it — "my-ollama-alias" here to make
        // clear that the string is an alias, not a type or hardcoded keyword.
        let paths = excluded_paths(&cfg, "providers.models.ollama.my-ollama-alias");
        assert!(
            paths
                .iter()
                .any(|p| p == "providers.models.ollama.my-ollama-alias.wire-api"),
            "expected wire-api excluded, got: {paths:?}"
        );
    }

    #[test]
    fn excluded_paths_for_memory_uses_active_backend() {
        let mut cfg = Config::default();
        cfg.memory.backend = "sqlite".into();
        let paths = excluded_paths(&cfg, "memory");
        assert!(paths.iter().any(|p| p == "memory.qdrant."));
        assert!(paths.iter().any(|p| p == "memory.postgres."));
    }

    #[test]
    fn is_excluded_handles_sub_table_marker() {
        let excludes = vec!["memory.qdrant.".to_string(), "memory.foo".to_string()];
        // Sub-table prefix matches anything under it.
        assert!(is_excluded("memory.qdrant.url", &excludes));
        assert!(is_excluded("memory.qdrant.api-key", &excludes));
        // Exact matches still work.
        assert!(is_excluded("memory.foo", &excludes));
        // Unrelated paths don't match.
        assert!(!is_excluded("memory.postgres.url", &excludes));
        assert!(!is_excluded("memory.foobar", &excludes));
    }

    #[test]
    fn apply_provider_trait_defaults_populates_ollama_base_url() {
        // The user-facing complaint: picking Ollama in the dashboard left
        // base_url empty. After apply, it should match the provider's
        // default_base_url() (http://localhost:11434).
        let mut cfg = Config::default();
        // Simulate the two-step selection: outer type bucket + named alias.
        cfg.create_map_key("providers.models", "ollama")
            .expect("create outer bucket");
        cfg.create_map_key("providers.models.ollama", "my-ollama-alias")
            .expect("create alias");

        apply_provider_trait_defaults(
            &mut cfg,
            "ollama",
            "providers.models.ollama.my-ollama-alias",
        )
        .expect("apply defaults");

        let uri = cfg
            .get_prop("providers.models.ollama.my-ollama-alias.uri")
            .expect("uri");
        assert!(
            uri.contains("11434"),
            "expected ollama default uri with port 11434, got: {uri}"
        );

        // Temperature should also be populated (Ollama overrides to 0.0).
        let temp = cfg
            .get_prop("providers.models.ollama.my-ollama-alias.temperature")
            .expect("temperature");
        assert!(
            !temp.is_empty() && temp != "<unset>",
            "temperature should be set"
        );
    }

    #[test]
    fn apply_provider_trait_defaults_skips_user_overrides() {
        // If the user already set a value, apply shouldn't clobber it on a
        // re-select / second call.
        let mut cfg = Config::default();
        cfg.create_map_key("providers.models", "ollama")
            .expect("create outer bucket");
        cfg.create_map_key("providers.models.ollama", "my-ollama-alias")
            .expect("create alias");
        cfg.set_prop(
            "providers.models.ollama.my-ollama-alias.uri",
            "http://example:9999",
        )
        .expect("set uri");

        apply_provider_trait_defaults(
            &mut cfg,
            "ollama",
            "providers.models.ollama.my-ollama-alias",
        )
        .expect("apply defaults");

        let uri = cfg
            .get_prop("providers.models.ollama.my-ollama-alias.uri")
            .expect("uri");
        assert_eq!(
            uri, "http://example:9999",
            "user-set uri must survive a defaults pass"
        );
    }

    #[test]
    fn providers_models_key_extracts_simple_segment() {
        // V3: type-only prefix
        assert_eq!(
            providers_models_key("providers.models.ollama"),
            Some("ollama")
        );
        assert_eq!(
            providers_models_key("providers.models.azure_openai"),
            Some("azure_openai")
        );
        // V3: type.alias prefix — extract just the type
        assert_eq!(
            providers_models_key("providers.models.ollama.default"),
            Some("ollama")
        );
        assert_eq!(
            providers_models_key("providers.models.ollama.my-alias"),
            Some("ollama")
        );
        // A nested path (provider's field) — still returns the type
        assert_eq!(
            providers_models_key("providers.models.ollama.default.api-key"),
            Some("ollama")
        );
        assert_eq!(providers_models_key("providers.models"), None);
        assert_eq!(providers_models_key("providers"), None);
        assert_eq!(providers_models_key(""), None);
    }
}
