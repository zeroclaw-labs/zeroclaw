//! Tests to verify .dockerignore excludes sensitive paths from Docker build context.
//!
//! These tests validate that:
//! 1. The .dockerignore file exists
//! 2. All security-critical paths are excluded
//! 3. All build-essential paths are NOT excluded
//! 4. Pattern syntax is valid

use std::fs;
use std::path::Path;

/// Paths that MUST be excluded from Docker build context (security/performance)
const MUST_EXCLUDE: &[&str] = &[
    ".git",
    ".githooks",
    "target",
    "docs",
    "examples",
    "tests",
    "*.md",
    "*.png",
    "*.db",
    "*.db-journal",
    ".DS_Store",
    ".github",
    "deny.toml",
    "LICENSE",
    ".env",
    ".tmp_*",
];

/// Paths that MUST NOT be excluded (required for build)
const MUST_INCLUDE: &[&str] = &["Cargo.toml", "Cargo.lock", "src/"];

/// Parse .dockerignore and return all non-comment, non-empty lines
fn parse_dockerignore(content: &str) -> Vec<String> {
    content
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.to_string())
        .collect()
}

/// Check if a pattern would match a given path
fn pattern_matches(pattern: &str, path: &str) -> bool {
    // Handle negation patterns
    if pattern.starts_with('!') {
        return false; // Negation re-includes, so it doesn't "exclude"
    }

    // Handle glob patterns
    if pattern.starts_with("*.") {
        let ext = &pattern[1..]; // e.g., ".md"
        return path.ends_with(ext);
    }

    // Handle directory patterns (with or without trailing slash)
    let pattern_normalized = pattern.trim_end_matches('/');
    let path_normalized = path.trim_end_matches('/');

    // Exact match
    if path_normalized == pattern_normalized {
        return true;
    }

    // Pattern is a prefix (directory match)
    if path_normalized.starts_with(&format!("{}/", pattern_normalized)) {
        return true;
    }

    // Wildcard prefix patterns like ".tmp_*"
    if pattern.contains('*') && !pattern.starts_with("*.") {
        let prefix = pattern.split('*').next().unwrap_or("");
        if !prefix.is_empty() && path.starts_with(prefix) {
            return true;
        }
    }

    false
}

/// Check if any pattern in the list would exclude the given path
fn is_excluded(patterns: &[String], path: &str) -> bool {
    let mut excluded = false;
    for pattern in patterns {
        if let Some(negated) = pattern.strip_prefix('!') {
            // Negation pattern - re-include
            if pattern_matches(negated, path) {
                excluded = false;
            }
        } else if pattern_matches(pattern, path) {
            excluded = true;
        }
    }
    excluded
}

#[test]
fn dockerignore_file_exists() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".dockerignore");
    assert!(
        path.exists(),
        ".dockerignore file must exist at project root"
    );
}

#[test]
fn dockerignore_excludes_security_critical_paths() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".dockerignore");
    let content = fs::read_to_string(&path).expect("Failed to read .dockerignore");
    let patterns = parse_dockerignore(&content);

    for must_exclude in MUST_EXCLUDE {
        // For glob patterns, test with a sample file
        let test_path = if must_exclude.starts_with("*.") {
            format!("sample{}", &must_exclude[1..])
        } else {
            must_exclude.to_string()
        };

        assert!(
            is_excluded(&patterns, &test_path),
            "Path '{}' (tested as '{}') MUST be excluded by .dockerignore but is not. \
             This is a security/performance issue.",
            must_exclude,
            test_path
        );
    }
}

#[test]
fn dockerignore_does_not_exclude_build_essentials() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".dockerignore");
    let content = fs::read_to_string(&path).expect("Failed to read .dockerignore");
    let patterns = parse_dockerignore(&content);

    for must_include in MUST_INCLUDE {
        assert!(
            !is_excluded(&patterns, must_include),
            "Path '{}' MUST NOT be excluded by .dockerignore (required for build)",
            must_include
        );
    }
}

#[test]
fn dockerignore_excludes_git_directory() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".dockerignore");
    let content = fs::read_to_string(&path).expect("Failed to read .dockerignore");
    let patterns = parse_dockerignore(&content);

    // .git directory and its contents must be excluded
    assert!(is_excluded(&patterns, ".git"), ".git must be excluded");
    assert!(
        is_excluded(&patterns, ".git/config"),
        ".git/config must be excluded"
    );
    assert!(
        is_excluded(&patterns, ".git/objects/pack/pack-abc123.pack"),
        ".git subdirectories must be excluded"
    );
}

#[test]
fn dockerignore_excludes_target_directory() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".dockerignore");
    let content = fs::read_to_string(&path).expect("Failed to read .dockerignore");
    let patterns = parse_dockerignore(&content);

    assert!(is_excluded(&patterns, "target"), "target must be excluded");
    assert!(
        is_excluded(&patterns, "target/debug/zeroclaw"),
        "target/debug must be excluded"
    );
    assert!(
        is_excluded(&patterns, "target/release/zeroclaw"),
        "target/release must be excluded"
    );
}

#[test]
fn dockerignore_excludes_database_files() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".dockerignore");
    let content = fs::read_to_string(&path).expect("Failed to read .dockerignore");
    let patterns = parse_dockerignore(&content);

    assert!(
        is_excluded(&patterns, "brain.db"),
        "*.db files must be excluded"
    );
    assert!(
        is_excluded(&patterns, "memory.db"),
        "*.db files must be excluded"
    );
    assert!(
        is_excluded(&patterns, "brain.db-journal"),
        "*.db-journal files must be excluded"
    );
}

#[test]
fn dockerignore_excludes_markdown_files() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".dockerignore");
    let content = fs::read_to_string(&path).expect("Failed to read .dockerignore");
    let patterns = parse_dockerignore(&content);

    assert!(
        is_excluded(&patterns, "README.md"),
        "*.md files must be excluded"
    );
    assert!(
        is_excluded(&patterns, "CHANGELOG.md"),
        "*.md files must be excluded"
    );
    assert!(
        is_excluded(&patterns, "CONTRIBUTING.md"),
        "*.md files must be excluded"
    );
}

#[test]
fn dockerignore_excludes_image_files() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".dockerignore");
    let content = fs::read_to_string(&path).expect("Failed to read .dockerignore");
    let patterns = parse_dockerignore(&content);

    assert!(
        is_excluded(&patterns, "zeroclaw.png"),
        "*.png files must be excluded"
    );
    assert!(
        is_excluded(&patterns, "logo.png"),
        "*.png files must be excluded"
    );
}

#[test]
fn dockerignore_excludes_env_files() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".dockerignore");
    let content = fs::read_to_string(&path).expect("Failed to read .dockerignore");
    let patterns = parse_dockerignore(&content);

    assert!(
        is_excluded(&patterns, ".env"),
        ".env must be excluded (contains secrets)"
    );
}

#[test]
fn dockerignore_excludes_ci_configs() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".dockerignore");
    let content = fs::read_to_string(&path).expect("Failed to read .dockerignore");
    let patterns = parse_dockerignore(&content);

    assert!(
        is_excluded(&patterns, ".github"),
        ".github must be excluded"
    );
    assert!(
        is_excluded(&patterns, ".github/workflows/ci.yml"),
        ".github/workflows must be excluded"
    );
}

#[test]
fn dockerignore_has_valid_syntax() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(".dockerignore");
    let content = fs::read_to_string(&path).expect("Failed to read .dockerignore");

    for (line_num, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        // Skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Check for invalid patterns
        assert!(
            !trimmed.contains("**") || trimmed.matches("**").count() <= 2,
            "Line {}: Too many ** in pattern '{}'",
            line_num + 1,
            trimmed
        );

        // Check for trailing spaces (can cause issues)
        assert!(
            line.trim_end() == line.trim_start().trim_end(),
            "Line {}: Pattern '{}' has leading whitespace which may cause issues",
            line_num + 1,
            line
        );
    }
}

#[test]
fn dockerignore_pattern_matching_edge_cases() {
    // Test the pattern matching logic itself
    let patterns = vec![
        ".git".to_string(),
        ".githooks".to_string(),
        "target".to_string(),
        "*.md".to_string(),
        "*.db".to_string(),
        ".tmp_*".to_string(),
        ".env".to_string(),
    ];

    // Should match
    assert!(is_excluded(&patterns, ".git"));
    assert!(is_excluded(&patterns, ".git/config"));
    assert!(is_excluded(&patterns, ".githooks"));
    assert!(is_excluded(&patterns, "target"));
    assert!(is_excluded(&patterns, "target/debug/build"));
    assert!(is_excluded(&patterns, "README.md"));
    assert!(is_excluded(&patterns, "brain.db"));
    assert!(is_excluded(&patterns, ".tmp_todo_probe"));
    assert!(is_excluded(&patterns, ".env"));

    // Should NOT match
    assert!(!is_excluded(&patterns, "src"));
    assert!(!is_excluded(&patterns, "src/main.rs"));
    assert!(!is_excluded(&patterns, "Cargo.toml"));
    assert!(!is_excluded(&patterns, "Cargo.lock"));
}
