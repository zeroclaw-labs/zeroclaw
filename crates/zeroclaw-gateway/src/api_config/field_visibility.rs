//! Request-time presentation filters for the Gateway config list.

use zeroclaw_config::schema::Config;

fn memory_backend_excludes(backend: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    if backend != "tachi" {
        out.push("conversation-retention-days");
    }
    out
}

pub(super) fn excluded_paths(cfg: &Config, prefix: &str) -> Vec<String> {
    if prefix == "memory" || prefix.is_empty() {
        let backend = if cfg.memory.backend.is_empty() {
            "tachi"
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
/// markers (`"memory.foo."` matches every `memory.foo.*`).
pub(super) fn is_excluded(path: &str, excludes: &[String]) -> bool {
    excludes
        .iter()
        .any(|e| path == e || (e.ends_with('.') && path.starts_with(e)))
}

/// Test whether `path` equals `prefix` or sits beneath it at a `.` segment
/// boundary. A bare `starts_with` is wrong here: prefix `agents.aaa` must
/// not match `agents.aaalore.workspace`.
pub(super) fn path_matches_prefix(path: &str, prefix: &str) -> bool {
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
    fn memory_excludes_hide_non_tachi_retention_knobs() {
        let ex = memory_backend_excludes("tachi");
        assert!(!ex.contains(&"conversation-retention-days"));

        let ex = memory_backend_excludes("markdown");
        assert!(ex.contains(&"conversation-retention-days"));
    }

    #[test]
    fn excluded_paths_for_memory_uses_active_backend() {
        let mut cfg = Config::default();
        cfg.memory.backend = "markdown".into();
        let paths = excluded_paths(&cfg, "memory");
        assert!(
            paths
                .iter()
                .any(|p| p == "memory.conversation-retention-days")
        );
    }

    #[test]
    fn is_excluded_handles_sub_table_marker() {
        let excludes = vec!["memory.sub.".to_string(), "memory.foo".to_string()];
        // Sub-table prefix matches anything under it.
        assert!(is_excluded("memory.sub.url", &excludes));
        assert!(is_excluded("memory.sub.api-key", &excludes));
        // Exact matches still work.
        assert!(is_excluded("memory.foo", &excludes));
        // Unrelated paths don't match.
        assert!(!is_excluded("memory.other.url", &excludes));
        assert!(!is_excluded("memory.foobar", &excludes));
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
