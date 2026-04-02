//! Integration test: Displays filesystem access (allowed paths) with check/cross marks.
//!
//! Verifies the acceptance criterion for story US-ZCL-12:
//! > Displays filesystem access (allowed paths) with check/cross marks
//!
//! Exercises `format_audit_summary` to confirm that allowed paths appear under a
//! "Filesystem access:" heading with a ✓ prefix and an arrow mapping, and that an
//! empty path list renders "(none)" instead.

use zeroclaw::plugins::{format_audit_summary, PluginManifest};

/// When a manifest declares allowed paths, the audit output should list each path
/// with a ✓ check mark under the "Filesystem access:" heading.
#[test]
fn audit_displays_allowed_paths_with_check_marks() {
    let toml_str = r#"
[plugin]
name = "fs-plugin"
version = "1.0.0"
wasm_path = "fs.wasm"
capabilities = ["tool"]

[plugin.filesystem]
data = "/var/data"
cache = "/tmp/cache"

[[tools]]
name = "read_data"
description = "Reads data files"
export = "read_data"
risk_level = "medium"
parameters_schema = { type = "object" }
"#;

    let manifest = PluginManifest::parse(toml_str).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    // The "Filesystem access:" section must exist
    assert!(
        output.contains("Filesystem access:"),
        "output should contain 'Filesystem access:' heading"
    );

    // Each allowed path should appear with a ✓ prefix and arrow mapping
    assert!(
        output.contains("\u{2713} data \u{2192} /var/data"),
        "output should show \u{2713} for data path, got:\n{output}"
    );
    assert!(
        output.contains("\u{2713} cache \u{2192} /tmp/cache"),
        "output should show \u{2713} for cache path, got:\n{output}"
    );
}

/// When a manifest declares a single allowed path, the check mark should appear.
#[test]
fn audit_displays_single_path_with_check_mark() {
    let toml_str = r#"
[plugin]
name = "single-path-plugin"
version = "0.1.0"
wasm_path = "single.wasm"
capabilities = ["tool"]

[plugin.filesystem]
logs = "/var/log/app"

[[tools]]
name = "tail_log"
description = "Tail log files"
export = "tail_log"
risk_level = "low"
parameters_schema = { type = "object" }
"#;

    let manifest = PluginManifest::parse(toml_str).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    assert!(
        output.contains("\u{2713} logs \u{2192} /var/log/app"),
        "output should show \u{2713} for the single allowed path, got:\n{output}"
    );
}

/// When no filesystem section is declared (no allowed paths), the output should
/// indicate no filesystem access with "(none)".
#[test]
fn audit_displays_none_when_no_paths_allowed() {
    let toml_str = r#"
[plugin]
name = "no-fs-plugin"
version = "0.1.0"
wasm_path = "nofs.wasm"
capabilities = ["tool"]

[[tools]]
name = "compute"
description = "Pure computation"
export = "compute"
risk_level = "low"
parameters_schema = { type = "object" }
"#;

    let manifest = PluginManifest::parse(toml_str).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    // Extract the Filesystem access section
    let fs_section = output
        .split("Filesystem access:")
        .nth(1)
        .expect("should have Filesystem access section");
    let section_end = fs_section.find("\n\n").unwrap_or(fs_section.len());
    let section = &fs_section[..section_end];

    assert!(
        section.contains("(none)"),
        "filesystem section should show (none) when no paths allowed, got:\n{section}"
    );
    assert!(
        !section.contains('\u{2713}'),
        "filesystem section should not contain \u{2713} when no paths allowed"
    );
}

/// Multiple paths with varied mount points should all display with check marks.
#[test]
fn audit_displays_multiple_paths_with_check_marks() {
    let toml_str = r#"
[plugin]
name = "multi-path-plugin"
version = "2.0.0"
wasm_path = "multi.wasm"
capabilities = ["tool"]

[plugin.filesystem]
data = "/var/data"
logs = "/var/log/plugin"
tmp = "/tmp/scratch"

[[tools]]
name = "process"
description = "Process files"
export = "process"
risk_level = "high"
parameters_schema = { type = "object" }
"#;

    let manifest = PluginManifest::parse(toml_str).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    // All three paths should have ✓ marks
    assert!(
        output.contains("\u{2713} data \u{2192} /var/data"),
        "output should show \u{2713} for data, got:\n{output}"
    );
    assert!(
        output.contains("\u{2713} logs \u{2192} /var/log/plugin"),
        "output should show \u{2713} for logs, got:\n{output}"
    );
    assert!(
        output.contains("\u{2713} tmp \u{2192} /tmp/scratch"),
        "output should show \u{2713} for tmp, got:\n{output}"
    );
}
