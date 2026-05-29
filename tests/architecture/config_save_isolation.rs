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
const ISOLATION_MARKERS: &[&str] = &[
    "config_path",
    "ZEROCLAW_CONFIG_DIR",
    "set_var(\"HOME\"",
];

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
        let display = path.display().to_string();
        let is_integration_test = display.contains("/tests/");
        let region = if is_integration_test {
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
