//! Regression guard for ChannelMessage field naming consistency.
//!
//! This test prevents accidental reintroduction of the removed `reply_to` field
//! in Rust source code where `reply_target` must be used.

use std::fs;
use std::path::{Path, PathBuf};

const SCAN_PATHS: &[&str] = &["src", "examples"];
const FORBIDDEN_PATTERNS: &[&str] = &[".reply_to", "reply_to:"];

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("Failed to read directory {}: {err}", dir.display()));

    for entry in entries {
        let entry =
            entry.unwrap_or_else(|err| panic!("Failed to read entry in {}: {err}", dir.display()));
        let path = entry.path();

        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn source_does_not_use_legacy_reply_to_field() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut rust_files = Vec::new();

    for relative in SCAN_PATHS {
        collect_rs_files(&root.join(relative), &mut rust_files);
    }

    rust_files.sort();

    let mut violations = Vec::new();

    for file_path in rust_files {
        let content = fs::read_to_string(&file_path).unwrap_or_else(|err| {
            panic!("Failed to read source file {}: {err}", file_path.display())
        });

        for (line_idx, line) in content.lines().enumerate() {
            for pattern in FORBIDDEN_PATTERNS {
                if line.contains(pattern) {
                    let rel = file_path
                        .strip_prefix(root)
                        .unwrap_or(&file_path)
                        .display()
                        .to_string();
                    violations.push(format!(
                        "{rel}:{} contains forbidden pattern `{pattern}`: {}",
                        line_idx + 1,
                        line.trim()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "Found legacy `reply_to` field usage:\n{}",
        violations.join("\n")
    );
}
