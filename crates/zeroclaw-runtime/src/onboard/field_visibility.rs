//! Per-section field visibility helpers.
//!
//! Used by both the CLI wizard (`onboard::offer_advanced_settings`) and the
//! gateway HTTP endpoints (`/api/config/list` for filtering). One source of
//! truth so the CLI and dashboard can't disagree about which fields apply.
//!
//! Per-provider-family field exclusion is GONE as of #6273 — the typed-family
//! ModelProviders container only exposes fields that genuinely apply to each
//! family (every typed `*ModelProviderConfig` carries only its own surface),
//! so there's nothing to suppress. Memory-backend exclusion stays because the
//! `[memory]` section is still a single struct carrying every backend's
//! sub-tables (the typed-family pattern hasn't been applied there).

use zeroclaw_config::schema::Config;

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
    if prefix == "memory" || prefix.is_empty() {
        let backend = if cfg.memory.backend.is_empty() {
            "sqlite"
        } else {
            cfg.memory.backend.as_str()
        };
        return memory_backend_excludes(backend)
            .into_iter()
            .map(|leaf| format!("memory.{leaf}"))
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
