//! Per-section field visibility helpers.

use crate::schema::Config;

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

/// Test whether `path` equals `prefix` or sits beneath it at a `.` segment
/// boundary. A bare `starts_with` is wrong here: prefix `agents.aaa` must
/// not match `agents.aaalore.workspace`.
pub fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    match path.strip_prefix(prefix) {
        Some(rest) => {
            prefix.is_empty() || rest.is_empty() || rest.starts_with('.') || prefix.ends_with('.')
        }
        None => false,
    }
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

    #[test]
    fn postgres_backend_hides_sqlite_and_qdrant_subsections() {
        // postgres active → hide sqlite-only knobs and qdrant subsection,
        // keep postgres subsection visible
        let ex = memory_backend_excludes("postgres");
        assert!(ex.contains(&"sqlite-open-timeout-secs"));
        assert!(ex.contains(&"conversation-retention-days"));
        assert!(ex.contains(&"qdrant."));
        assert!(!ex.contains(&"postgres."));
    }

    #[test]
    fn path_matches_prefix_requires_segment_boundary() {
        // Exact match and children.
        assert!(path_matches_prefix("agents.aaa", "agents.aaa"));
        assert!(path_matches_prefix("agents.aaa.workspace", "agents.aaa"));
        assert!(path_matches_prefix("agents.aaa.memory.limit", "agents.aaa"));
        assert!(!path_matches_prefix(
            "agents.aaalore.workspace",
            "agents.aaa"
        ));
        assert!(!path_matches_prefix(
            "agents.aaatools.identity",
            "agents.aaa"
        ));
        assert!(!path_matches_prefix("agents.aaalore", "agents.aaa"));
        // Dot-terminated prefixes keep their sub-table semantics.
        assert!(path_matches_prefix("agents.aaa.workspace", "agents.aaa."));
        assert!(!path_matches_prefix("agents.aab.workspace", "agents.aaa."));
        // Top-level sections.
        assert!(path_matches_prefix("memory.backend", "memory"));
        assert!(!path_matches_prefix("memory.backend", "mem"));
        assert!(!path_matches_prefix("unrelated", "agents.aaa"));
        // Empty prefix matches everything (no-filter semantics, parity
        // with the bare starts_with behavior it replaced).
        assert!(path_matches_prefix("anything.at.all", ""));
        assert!(path_matches_prefix("", ""));
    }
}
