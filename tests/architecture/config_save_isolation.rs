//! Architecture gate: tests that persist `Config` must isolate the target
//! path. `Config::default()` targets the real ~/.zeroclaw, so an
//! unisolated save clobbers the developer's live config.

use std::fs;
use std::path::Path;

/// Calls that write config to disk (directly, or by flagging a field
/// for the next `save`).
const PERSIST_CALLS: &[&str] = &[
    ".save()",
    ".save().await",
    ".save_dirty()",
    ".save_dirty().await",
    "set_prop_persistent",
    "set_secret_persistent",
];

/// Evidence that a file isolates its config writes.
const ISOLATION_MARKERS: &[&str] = &["config_path", "ZEROCLAW_CONFIG_DIR", "set_var(\"HOME\""];

/// True if `path` sits under a `tests` directory component of this crate
/// (e.g. `tests/foo.rs`, `crates/x/tests/y.rs`). Classification is done by
/// path component, not by the rendered separator: `Path::display()` uses
/// backslashes on Windows, so a `contains("/tests/")` check on the rendered
/// string silently never matches there. Panics on paths outside the crate
/// root — every caller walks from `CARGO_MANIFEST_DIR`, and a check on the
/// absolute path could misclassify a checkout under a `tests` directory.
fn is_integration_test(path: &Path) -> bool {
    let rel = path
        .strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .expect("path must be under CARGO_MANIFEST_DIR");
    rel.components().any(|c| c.as_os_str() == "tests")
}

#[test]
fn tests_that_persist_config_isolate_the_path() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    let mut violations: Vec<String> = Vec::new();
    scan_dir(&workspace_root.join("crates"), &mut violations);
    scan_dir(&workspace_root.join("apps"), &mut violations);
    scan_dir(&workspace_root.join("tests"), &mut violations);
    assert!(
        violations.is_empty(),
        "Config-persisting test code without path isolation detected. \
         `Config::default()` targets the real ~/.zeroclaw; a test that \
         saves it clobbers the developer's live config. Set `config_path` \
         to a TempDir (or override HOME / ZEROCLAW_CONFIG_DIR to a tempdir) \
         before persisting. To override, add `// SOT: <reason>` on the line.\n\n\
         Violations:\n{}",
        violations.join("\n")
    );
}

fn scan_dir(dir: &Path, violations: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, violations);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let Ok(src) = fs::read_to_string(&path) else {
            continue;
        };
        let region = if is_integration_test(&path) {
            Some((0usize, src.as_str()))
        } else {
            src.find("#[cfg(test)]").map(|start| (start, &src[start..]))
        };
        let Some((region_start, region_src)) = region else {
            continue;
        };
        if ISOLATION_MARKERS.iter().any(|m| region_src.contains(m)) {
            continue;
        }
        let display = path.display().to_string();
        let base_line = src[..region_start].lines().count();
        for (offset, line) in region_src.lines().enumerate() {
            if line.contains("// SOT:") {
                continue;
            }
            if PERSIST_CALLS.iter().any(|c| line.contains(c)) {
                violations.push(format!(
                    "  {}:{}: {}",
                    display,
                    base_line + offset,
                    line.trim()
                ));
            }
        }
    }
}

/// Regression coverage for `is_integration_test`: classification must be
/// separator-independent, so every path here is built from single
/// components via `join` (never from a string with embedded `/` or `\`)
/// to make sure the check can't accidentally pass by matching on a
/// hardcoded separator.
#[test]
fn is_integration_test_matches_tests_component_regardless_of_separator() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    let component_built = manifest_dir.join("tests").join("component").join("foo.rs");
    assert!(is_integration_test(&component_built));

    let crate_tests = manifest_dir
        .join("crates")
        .join("zeroclaw-config")
        .join("tests")
        .join("x.rs");
    assert!(is_integration_test(&crate_tests));

    let crate_src = manifest_dir
        .join("crates")
        .join("zeroclaw-config")
        .join("src")
        .join("schema.rs");
    assert!(!is_integration_test(&crate_src));

    // `tests.rs` is a file name, not a `tests` directory component — must
    // not match on substring.
    let tests_named_file = manifest_dir.join("src").join("tests.rs");
    assert!(!is_integration_test(&tests_named_file));
}
