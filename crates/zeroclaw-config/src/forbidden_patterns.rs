//! Workspace-relative forbidden path pattern matching.
//!
//! Extends the existing `forbidden_paths` mechanism with glob and
//! workspace-relative pattern support. Absolute paths (`/...`, `~...`)
//! continue to use the existing prefix-match loop in `SecurityPolicy`.
//! This module handles patterns that operate on the **workspace-relative**
//! path, evaluated *before* the workspace short-circuit in the three
//! path-check methods.
//!
//! # Pattern semantics
//!
//! | Pattern form | Matching rule |
//! |---|---|
//! | `*`, `?`, `[` chars present | `glob::Pattern` match on normalized relative path (`require_literal_separator: true`) |
//! | Contains `/`, no glob chars, no trailing `/` | Exact match on normalized relative path |
//! | Contains `/`, no glob chars, trailing `/` | Directory prefix — matches the directory and all descendants |
//! | No `/`, no glob chars | Basename match against any single path component |
//!
//! Bare `*`, parent-traversing patterns (`../`), and invalid glob syntax
//! are rejected silently during construction.

use std::path::Path;

use glob::MatchOptions;

// Glob matching: `*`/`?` must not match path separators; dotfiles
// are matched normally (needed for `.env*` deny patterns).
static GLOB_MATCH_OPTIONS: MatchOptions = MatchOptions {
    require_literal_separator: true,
    require_literal_leading_dot: false,
    case_sensitive: true,
};

/// Categorized set of forbidden path patterns evaluated against
/// workspace-relative paths. Built once at policy construction from
/// the `forbidden_paths` config entries.
#[derive(Debug, Clone, Default)]
pub struct ForbiddenPatternSet {
    /// Glob patterns matched against the normalized workspace-relative path.
    globs: Vec<glob::Pattern>,
    /// Exact relative path matches (contain `/`, no glob chars, no trailing `/`).
    /// Example: `.cargo/config.toml` matches only that exact relative path.
    relative_exact_paths: Vec<String>,
    /// Directory prefix patterns (contain `/`, no glob chars, trailing `/` stored
    /// without the slash). Example: `secrets` from `secrets/` matches the
    /// directory and everything underneath.
    relative_dir_prefixes: Vec<String>,
    /// Basename patterns (no `/`, no glob chars). Matched against any single
    /// path component at any depth. Example: `credentials.json` matches
    /// `credentials.json`, `dir/credentials.json`, etc.
    basename_patterns: Vec<String>,
}

impl ForbiddenPatternSet {
    /// Build from config entries. Only workspace-relative entries are
    /// categorized here; absolute entries (`/...`, `~...`) are handled
    /// by the existing `forbidden_paths` prefix loop in `SecurityPolicy`.
    pub fn from_config_entries(config_patterns: &[String]) -> Self {
        let mut result = Self::default();
        for pattern in config_patterns {
            result.add_pattern(pattern);
        }
        result
    }

    fn add_pattern(&mut self, pattern: &str) {
        let trimmed = pattern.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return;
        }
        // Absolute paths and ~ paths are handled by existing forbidden_paths loop.
        if trimmed.starts_with('/') || trimmed.starts_with('~') {
            return;
        }
        // Reject bare `*` — would block all files in workspace.
        if trimmed == "*" {
            return;
        }
        // Reject parent-traversing patterns (e.g. `../etc/passwd`).
        // Only reject when `..` is followed by a separator, so harmless
        // names like `foo..bar` are allowed.
        if trimmed.contains("../") || trimmed.contains("..\\") {
            return;
        }

        let has_glob_chars =
            trimmed.contains('*') || trimmed.contains('?') || trimmed.contains('[');
        let has_separator = trimmed.contains('/');

        if has_glob_chars {
            match glob::Pattern::new(trimmed) {
                Ok(pat) => self.globs.push(pat),
                Err(_) => {
                    // Invalid glob — silently skip.
                }
            }
        } else if has_separator && trimmed.ends_with('/') {
            // Directory prefix — match the directory and all descendants.
            self.relative_dir_prefixes
                .push(trimmed.trim_end_matches('/').to_string());
        } else if has_separator {
            // Exact relative path match.
            self.relative_exact_paths.push(trimmed.to_string());
        } else {
            // Bare filename — match any path component.
            self.basename_patterns.push(trimmed.to_string());
        }
    }

    /// Check if a workspace-relative path should be forbidden.
    ///
    /// `relative` is the path after stripping the workspace root prefix.
    /// An empty path (the workspace root itself) is never forbidden.
    pub fn is_forbidden(&self, relative: &Path) -> bool {
        // Normalize to forward slashes for consistent matching.
        let relative_str = normalize_separators(relative);

        // Basename patterns — match the final path component.
        if let Some(file_name) = relative.file_name().and_then(|v| v.to_str()) {
            for basename in &self.basename_patterns {
                if file_name == basename.as_str() {
                    return true;
                }
            }
        }

        // Exact relative path patterns.
        for exact in &self.relative_exact_paths {
            if relative_str == *exact {
                return true;
            }
        }

        // Directory prefix patterns (trailing `/`).
        for prefix in &self.relative_dir_prefixes {
            if relative_str == *prefix || relative_str.starts_with(&format!("{prefix}/")) {
                return true;
            }
        }

        // Glob patterns.
        for glob in &self.globs {
            if glob.matches_with(&relative_str, GLOB_MATCH_OPTIONS) {
                return true;
            }
        }

        false
    }

    /// Returns `true` if there are no workspace-relative patterns configured.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.globs.is_empty()
            && self.relative_exact_paths.is_empty()
            && self.relative_dir_prefixes.is_empty()
            && self.basename_patterns.is_empty()
    }
}

/// Normalize path separators to `/` for consistent cross-platform matching.
fn normalize_separators(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn policy_entries(patterns: &[&str]) -> ForbiddenPatternSet {
        let strings: Vec<String> = patterns.iter().map(|s| s.to_string()).collect();
        ForbiddenPatternSet::from_config_entries(&strings)
    }

    // ── Basename patterns ────────────────────────────────────────────────

    #[test]
    fn basename_matches_at_any_depth() {
        let p = policy_entries(&["credentials.json"]);
        assert!(p.is_forbidden(Path::new("credentials.json")));
        assert!(p.is_forbidden(Path::new("dir/credentials.json")));
        assert!(p.is_forbidden(Path::new("a/b/c/credentials.json")));
    }

    #[test]
    fn basename_does_not_match_different_name() {
        let p = policy_entries(&["credentials.json"]);
        assert!(!p.is_forbidden(Path::new("other.json")));
        assert!(!p.is_forbidden(Path::new("credentials.toml")));
    }

    #[test]
    fn basename_matches_dotfiles() {
        let p = policy_entries(&[".env"]);
        assert!(p.is_forbidden(Path::new(".env")));
        assert!(p.is_forbidden(Path::new("dir/.env")));
    }

    // ── Exact relative path patterns ─────────────────────────────────────

    #[test]
    fn exact_path_matches_only_exact() {
        let p = policy_entries(&[".cargo/config.toml"]);
        assert!(p.is_forbidden(Path::new(".cargo/config.toml")));
        assert!(!p.is_forbidden(Path::new(".cargo/config.toml.bak")));
        assert!(!p.is_forbidden(Path::new("other/config.toml")));
    }

    #[test]
    fn exact_path_no_false_positive_on_prefix() {
        let p = policy_entries(&["src/main.rs"]);
        assert!(p.is_forbidden(Path::new("src/main.rs")));
        assert!(!p.is_forbidden(Path::new("src/main.rs.bak")));
    }

    // ── Directory prefix patterns ────────────────────────────────────────

    #[test]
    fn dir_prefix_matches_directory_and_descendants() {
        let p = policy_entries(&["secrets/"]);
        assert!(p.is_forbidden(Path::new("secrets")));
        assert!(p.is_forbidden(Path::new("secrets/file.txt")));
        assert!(p.is_forbidden(Path::new("secrets/sub/dir/file.txt")));
    }

    #[test]
    fn dir_prefix_does_not_match_unrelated() {
        let p = policy_entries(&["secrets/"]);
        assert!(!p.is_forbidden(Path::new("mysecrets/file.txt")));
        assert!(!p.is_forbidden(Path::new("notsecrets")));
    }

    #[test]
    fn dir_prefix_matches_root_level() {
        let p = policy_entries(&["build/"]);
        assert!(p.is_forbidden(Path::new("build")));
        assert!(p.is_forbidden(Path::new("build/output")));
    }

    // ── Glob patterns ────────────────────────────────────────────────────

    #[test]
    fn glob_star_does_not_cross_separator() {
        let p = policy_entries(&["*.toml"]);
        assert!(p.is_forbidden(Path::new("Cargo.toml")));
        assert!(p.is_forbidden(Path::new("rust-toolchain.toml")));
        assert!(!p.is_forbidden(Path::new("src/config.toml")));
        assert!(!p.is_forbidden(Path::new("dir/file.txt")));
    }

    #[test]
    fn glob_double_star_crosses_directories() {
        let p = policy_entries(&["**/*.log"]);
        assert!(p.is_forbidden(Path::new("build/debug.log")));
        assert!(p.is_forbidden(Path::new("src/main.log")));
        assert!(p.is_forbidden(Path::new("a/b/c.log")));
    }

    #[test]
    fn glob_question_mark() {
        let p = policy_entries(&["file?.txt"]);
        assert!(p.is_forbidden(Path::new("file1.txt")));
        assert!(p.is_forbidden(Path::new("fileA.txt")));
        assert!(!p.is_forbidden(Path::new("file12.txt")));
        assert!(!p.is_forbidden(Path::new("file.txt")));
    }

    #[test]
    fn glob_bracket_class() {
        let p = policy_entries(&["file[0-9].txt"]);
        assert!(p.is_forbidden(Path::new("file0.txt")));
        assert!(p.is_forbidden(Path::new("file9.txt")));
        assert!(!p.is_forbidden(Path::new("filex.txt")));
    }

    #[test]
    fn glob_with_path_separator() {
        let p = policy_entries(&["src/*.rs"]);
        assert!(p.is_forbidden(Path::new("src/main.rs")));
        assert!(p.is_forbidden(Path::new("src/lib.rs")));
        assert!(!p.is_forbidden(Path::new("src/deep/main.rs")));
        assert!(!p.is_forbidden(Path::new("lib/main.rs")));
    }

    #[test]
    fn glob_matches_dotfiles() {
        let p = policy_entries(&[".*"]);
        assert!(p.is_forbidden(Path::new(".env")));
        assert!(p.is_forbidden(Path::new(".gitignore")));
        assert!(!p.is_forbidden(Path::new("env")));
    }

    #[test]
    fn glob_env_star() {
        let p = policy_entries(&[".env*"]);
        assert!(p.is_forbidden(Path::new(".env")));
        assert!(p.is_forbidden(Path::new(".env.local")));
        assert!(p.is_forbidden(Path::new(".env.prod")));
        assert!(!p.is_forbidden(Path::new("env")));
    }

    // ── Rejected patterns ────────────────────────────────────────────────

    #[test]
    fn bare_star_rejected() {
        let p = policy_entries(&["*"]);
        assert!(p.is_empty());
    }

    #[test]
    fn parent_traversal_rejected() {
        let p = policy_entries(&["../etc/passwd"]);
        assert!(p.is_empty());
    }

    #[test]
    fn dots_in_name_not_rejected() {
        // `foo..bar` is a harmless filename, not parent traversal.
        let p = policy_entries(&["foo..bar"]);
        assert!(!p.is_empty());
        assert!(p.is_forbidden(Path::new("foo..bar")));
        assert!(p.is_forbidden(Path::new("dir/foo..bar")));
    }

    #[test]
    fn invalid_glob_rejected() {
        let p = policy_entries(&["[invalid"]);
        assert!(p.is_empty());
    }

    // ── Empty path (workspace root) ──────────────────────────────────────

    #[test]
    fn empty_relative_path_never_forbidden() {
        let p = policy_entries(&["*.toml", "secrets/", ".env"]);
        assert!(!p.is_forbidden(Path::new("")));
    }

    // ── Windows separator normalization ──────────────────────────────────

    #[test]
    fn windows_separators_normalized() {
        // Pattern uses `/` but we test that `normalize_separators` converts
        // `\` in the input path to `/` before matching.
        let p = policy_entries(&[".cargo/config.toml"]);
        // On non-Windows, construct a PathBuf with `\` (literal char).
        // `normalize_separators` will convert it to `/` for matching.
        let backslash_path = std::path::PathBuf::from(".cargo\\config.toml");
        assert!(
            p.is_forbidden(&backslash_path),
            "backslash path must be normalized to forward slash before matching"
        );
    }

    // ── Edge cases ───────────────────────────────────────────────────────

    #[test]
    fn basename_with_equals() {
        // Filenames with `=` are valid (e.g. env files, option assignments).
        let p = policy_entries(&["key=value.env"]);
        assert!(p.is_forbidden(Path::new("key=value.env")));
        assert!(p.is_forbidden(Path::new("dir/key=value.env")));
        assert!(!p.is_forbidden(Path::new("key_value.env")));
    }

    // ── Absolute entries are skipped ─────────────────────────────────────

    #[test]
    fn absolute_entries_skipped() {
        let p = policy_entries(&["/etc", "~/.ssh"]);
        assert!(p.is_empty());
    }

    // ── Comments and empty lines ─────────────────────────────────────────

    #[test]
    fn comments_and_blanks_ignored() {
        let p = policy_entries(&["# this is a comment", "", "  ", "*.log"]);
        assert!(!p.is_empty());
        assert!(p.is_forbidden(Path::new("debug.log")));
    }

    // ── Multiple patterns ────────────────────────────────────────────────

    #[test]
    fn multiple_patterns_compose() {
        let p = policy_entries(&["*.toml", "secrets/", ".env", ".cargo/config.toml"]);
        assert!(p.is_forbidden(Path::new("Cargo.toml")));
        assert!(p.is_forbidden(Path::new("secrets/creds.json")));
        assert!(p.is_forbidden(Path::new(".env")));
        assert!(p.is_forbidden(Path::new(".cargo/config.toml")));
        assert!(!p.is_forbidden(Path::new("src/main.rs")));
    }

    // ── Platform-native path separators ──────────────────────────────────

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn unix_paths_match_directly() {
        let p = policy_entries(&["src/*.rs"]);
        assert!(p.is_forbidden(Path::new("src/main.rs")));
    }
}
