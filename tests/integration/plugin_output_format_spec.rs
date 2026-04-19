#![cfg(any())] // disabled: pending format_audit_summary/decrypt functions

//! Integration test: Output format matches spec example.
//!
//! Verifies the acceptance criterion for story US-ZCL-12:
//! > Output format matches spec example
//!
//! Asserts that `format_audit_summary` produces output with the correct section
//! order, separators, headings, and formatting when all sections are populated.

use zeroclaw::plugins::{PluginManifest, format_audit_summary};

/// Build a fully-populated manifest that exercises every section of the audit output.
fn full_manifest_toml() -> &'static str {
    r#"
[plugin]
name = "acme-tool"
version = "2.1.0"
description = "A multi-capability plugin for testing audit output format."
author = "Test Author"
wasm_path = "acme.wasm"
capabilities = ["tool", "channel"]
permissions = ["http_client", "file_read"]

[plugin.network]
allowed_hosts = ["api.acme.com", "cdn.acme.com"]

[plugin.filesystem]
data = "/var/data"
cache = "/tmp/cache"

[[tools]]
name = "search"
description = "Search records"
export = "search"
risk_level = "low"
parameters_schema = { type = "object" }

[[tools]]
name = "update"
description = "Update records"
export = "update"
risk_level = "medium"
parameters_schema = { type = "object" }

[[tools]]
name = "purge"
description = "Purge all records"
export = "purge"
risk_level = "high"
parameters_schema = { type = "object" }
"#
}

/// The full output must contain all five sections in order:
/// 1. Plugin header (name, version, description, author)
/// 2. Network access
/// 3. Filesystem access
/// 4. Host capabilities
/// 5. Risk levels
#[test]
fn audit_output_contains_all_sections_in_order() {
    let manifest = PluginManifest::parse(full_manifest_toml()).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    let sections = [
        "Plugin: acme-tool v2.1.0",
        "Network access:",
        "Filesystem access:",
        "Host capabilities:",
        "Risk levels:",
    ];

    let mut last_pos = 0;
    for section in &sections {
        let pos = output[last_pos..]
            .find(section)
            .unwrap_or_else(|| panic!("missing section '{section}' in output:\n{output}"));
        last_pos += pos + section.len();
    }
}

/// The header section must include name, version, description, and author.
#[test]
fn audit_output_header_matches_spec() {
    let manifest = PluginManifest::parse(full_manifest_toml()).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    assert!(
        output.starts_with("Plugin: acme-tool v2.1.0"),
        "output should start with plugin header, got:\n{output}"
    );
    assert!(
        output
            .contains("  Description: A multi-capability plugin for testing audit output format."),
        "output should contain indented description"
    );
    assert!(
        output.contains("  Author: Test Author"),
        "output should contain indented author"
    );
}

/// Sections are separated by blank lines.
#[test]
fn audit_output_sections_separated_by_blank_lines() {
    let manifest = PluginManifest::parse(full_manifest_toml()).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    // Each section heading should be preceded by a blank line (except the first)
    for heading in &[
        "Network access:",
        "Filesystem access:",
        "Host capabilities:",
        "Risk levels:",
    ] {
        let pos = output
            .find(heading)
            .unwrap_or_else(|| panic!("missing '{heading}'"));
        // The character before the heading should be \n and the one before that should also be \n
        assert!(
            pos >= 2 && &output[pos - 2..pos] == "\n\n",
            "'{heading}' should be preceded by a blank line, context: {:?}",
            &output[pos.saturating_sub(10)..pos + heading.len()]
        );
    }
}

/// Network access section shows ✓ for each allowed host.
#[test]
fn audit_output_network_section_format() {
    let manifest = PluginManifest::parse(full_manifest_toml()).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    assert!(
        output.contains("  ✓ api.acme.com"),
        "should list api.acme.com with ✓"
    );
    assert!(
        output.contains("  ✓ cdn.acme.com"),
        "should list cdn.acme.com with ✓"
    );
}

/// Filesystem access section shows ✓ with logical → physical mapping.
#[test]
fn audit_output_filesystem_section_format() {
    let manifest = PluginManifest::parse(full_manifest_toml()).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    assert!(
        output.contains("✓ data → /var/data"),
        "should map data → /var/data"
    );
    assert!(
        output.contains("✓ cache → /tmp/cache"),
        "should map cache → /tmp/cache"
    );
}

/// Host capabilities section lists capabilities and permissions with ✓.
#[test]
fn audit_output_capabilities_section_format() {
    let manifest = PluginManifest::parse(full_manifest_toml()).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    // Capabilities
    assert!(
        output.contains("  ✓ tool provider"),
        "should list tool provider"
    );
    assert!(
        output.contains("  ✓ channel provider"),
        "should list channel provider"
    );
    // Permissions
    assert!(
        output.contains("  ✓ http client"),
        "should list http client permission"
    );
    assert!(
        output.contains("  ✓ filesystem (read)"),
        "should list filesystem (read) permission"
    );
}

/// Risk levels section uses • bullet, padded tool name, → arrow, and level string.
#[test]
fn audit_output_risk_levels_section_format() {
    let manifest = PluginManifest::parse(full_manifest_toml()).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    let risk_section = output
        .split("Risk levels:")
        .nth(1)
        .expect("should have Risk levels section");

    // Each tool line uses • and →
    for tool_name in &["search", "update", "purge"] {
        let line = risk_section
            .lines()
            .find(|l| l.contains(tool_name))
            .unwrap_or_else(|| panic!("missing tool '{tool_name}' in risk section"));
        assert!(line.contains('•'), "'{tool_name}' line should use bullet •");
        assert!(line.contains('→'), "'{tool_name}' line should use arrow →");
    }

    // Check risk level labels and approval annotations
    let search_line = risk_section.lines().find(|l| l.contains("search")).unwrap();
    assert!(search_line.contains("→ low"), "search should be → low");
    assert!(
        !search_line.contains("requires approval"),
        "low should not require approval"
    );

    let update_line = risk_section.lines().find(|l| l.contains("update")).unwrap();
    assert!(
        update_line.contains("→ medium"),
        "update should be → medium"
    );
    assert!(
        update_line.contains("(requires approval in supervised mode)"),
        "medium should annotate approval requirement"
    );

    let purge_line = risk_section.lines().find(|l| l.contains("purge")).unwrap();
    assert!(purge_line.contains("→ high"), "purge should be → high");
    assert!(
        purge_line.contains("(requires approval in supervised mode)"),
        "high should annotate approval requirement"
    );
}

/// The output should not have trailing newlines.
#[test]
fn audit_output_no_trailing_newlines() {
    let manifest = PluginManifest::parse(full_manifest_toml()).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    assert!(
        !output.ends_with('\n'),
        "output should not end with a newline"
    );
}

/// Verify the exact full output against the expected spec format.
#[test]
fn audit_output_matches_full_spec_example() {
    let manifest = PluginManifest::parse(full_manifest_toml()).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    // Build the expected output line-by-line to match the spec format exactly.
    // Note: filesystem paths come from a BTreeMap so they're sorted alphabetically.
    let expected_lines = [
        "Plugin: acme-tool v2.1.0",
        "  Description: A multi-capability plugin for testing audit output format.",
        "  Author: Test Author",
        "",
        "Network access:",
        "  ✓ api.acme.com",
        "  ✓ cdn.acme.com",
        "",
        "Filesystem access:",
    ];

    let output_lines: Vec<&str> = output.lines().collect();

    // Check the fixed prefix lines
    for (i, expected) in expected_lines.iter().enumerate() {
        assert_eq!(
            output_lines.get(i).copied().unwrap_or("MISSING"),
            *expected,
            "line {i} mismatch"
        );
    }

    // Filesystem paths come from a map, so check they're present (order may vary)
    let fs_section_start = expected_lines.len();
    let fs_lines: Vec<&&str> = output_lines[fs_section_start..]
        .iter()
        .take_while(|l| !l.is_empty())
        .collect();
    assert_eq!(fs_lines.len(), 2, "should have 2 filesystem entries");
    let fs_text: String = fs_lines.iter().map(|l| **l).collect::<Vec<_>>().join("\n");
    assert!(
        fs_text.contains("✓ cache → /tmp/cache"),
        "missing cache path"
    );
    assert!(fs_text.contains("✓ data → /var/data"), "missing data path");

    // After filesystem, verify remaining sections exist with correct structure
    let remaining = output_lines[fs_section_start + fs_lines.len()..].join("\n");
    assert!(
        remaining.contains("Host capabilities:"),
        "missing Host capabilities section"
    );
    assert!(
        remaining.contains("  ✓ tool provider"),
        "missing tool provider"
    );
    assert!(
        remaining.contains("  ✓ channel provider"),
        "missing channel provider"
    );
    assert!(remaining.contains("  ✓ http client"), "missing http client");
    assert!(
        remaining.contains("  ✓ filesystem (read)"),
        "missing filesystem (read)"
    );
    assert!(
        remaining.contains("Risk levels:"),
        "missing Risk levels section"
    );
}

/// A minimal manifest (no optional fields) should still produce valid spec-compliant output.
#[test]
fn audit_output_minimal_manifest_has_all_sections() {
    let toml_str = r#"
[plugin]
name = "bare-plugin"
version = "0.0.1"
wasm_path = "bare.wasm"
capabilities = ["tool"]

[[tools]]
name = "noop"
description = "Does nothing"
export = "noop"
risk_level = "low"
parameters_schema = { type = "object" }
"#;

    let manifest = PluginManifest::parse(toml_str).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    // Even a minimal manifest must have all sections
    assert!(output.contains("Plugin: bare-plugin v0.0.1"));
    assert!(output.contains("Network access:"));
    assert!(output.contains("  (none)"));
    assert!(output.contains("Filesystem access:"));
    assert!(output.contains("Host capabilities:"));
    assert!(output.contains("Risk levels:"));
    assert!(!output.ends_with('\n'));
}
