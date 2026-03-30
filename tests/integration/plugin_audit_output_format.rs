//! Integration test: audit output formatting for varied plugin profiles.
//!
//! Covers three scenarios from US-ZCL-12-8:
//! 1. Plugin with network + filesystem + multiple tools (full capabilities)
//! 2. Plugin with no capabilities (minimal)
//! 3. Plugin with wildcard hosts

use zeroclaw::plugins::{format_audit_summary, PluginManifest};

// ---------------------------------------------------------------------------
// Scenario 1: plugin with network, filesystem, and multiple tools
// ---------------------------------------------------------------------------

fn full_plugin_toml() -> &'static str {
    r#"
[plugin]
name = "full-plugin"
version = "3.0.0"
description = "Plugin exercising all audit sections"
author = "QA Team"
wasm_path = "full.wasm"
capabilities = ["tool", "channel"]
permissions = ["http_client", "file_read", "file_write"]

[plugin.network]
allowed_hosts = ["api.internal.dev", "metrics.internal.dev"]

[plugin.filesystem]
data = "/mnt/data"
logs = "/var/log/full-plugin"

[[tools]]
name = "query"
description = "Query records"
export = "query"
risk_level = "low"
parameters_schema = { type = "object" }

[[tools]]
name = "mutate"
description = "Mutate records"
export = "mutate"
risk_level = "medium"
parameters_schema = { type = "object" }

[[tools]]
name = "destroy"
description = "Destroy records"
export = "destroy"
risk_level = "high"
parameters_schema = { type = "object" }
"#
}

#[test]
fn full_plugin_header_section() {
    let manifest = PluginManifest::parse(full_plugin_toml()).expect("parse");
    let output = format_audit_summary(&manifest);

    assert!(output.starts_with("Plugin: full-plugin v3.0.0"));
    assert!(output.contains("  Description: Plugin exercising all audit sections"));
    assert!(output.contains("  Author: QA Team"));
}

#[test]
fn full_plugin_network_section() {
    let manifest = PluginManifest::parse(full_plugin_toml()).expect("parse");
    let output = format_audit_summary(&manifest);

    assert!(output.contains("Network access:"));
    assert!(output.contains("  ✓ api.internal.dev"));
    assert!(output.contains("  ✓ metrics.internal.dev"));
    // Should NOT show (none)
    let net_section = output.split("Network access:").nth(1).unwrap();
    let until_blank = net_section.split("\n\n").next().unwrap();
    assert!(!until_blank.contains("(none)"));
}

#[test]
fn full_plugin_filesystem_section() {
    let manifest = PluginManifest::parse(full_plugin_toml()).expect("parse");
    let output = format_audit_summary(&manifest);

    assert!(output.contains("Filesystem access:"));
    assert!(output.contains("✓ data → /mnt/data"));
    assert!(output.contains("✓ logs → /var/log/full-plugin"));
}

#[test]
fn full_plugin_capabilities_section() {
    let manifest = PluginManifest::parse(full_plugin_toml()).expect("parse");
    let output = format_audit_summary(&manifest);

    assert!(output.contains("Host capabilities:"));
    assert!(output.contains("  ✓ tool provider"));
    assert!(output.contains("  ✓ channel provider"));
    assert!(output.contains("  ✓ http client"));
    assert!(output.contains("  ✓ filesystem (read)"));
    assert!(output.contains("  ✓ filesystem (write)"));
}

#[test]
fn full_plugin_risk_levels_section() {
    let manifest = PluginManifest::parse(full_plugin_toml()).expect("parse");
    let output = format_audit_summary(&manifest);

    let risk_section = output.split("Risk levels:").nth(1).expect("risk section");

    // query → low, no approval
    let query_line = risk_section.lines().find(|l| l.contains("query")).unwrap();
    assert!(query_line.contains("→ low"));
    assert!(!query_line.contains("requires approval"));

    // mutate → medium, requires approval
    let mutate_line = risk_section.lines().find(|l| l.contains("mutate")).unwrap();
    assert!(mutate_line.contains("→ medium"));
    assert!(mutate_line.contains("(requires approval in supervised mode)"));

    // destroy → high, requires approval
    let destroy_line = risk_section.lines().find(|l| l.contains("destroy")).unwrap();
    assert!(destroy_line.contains("→ high"));
    assert!(destroy_line.contains("(requires approval in supervised mode)"));
}

#[test]
fn full_plugin_all_sections_present_in_order() {
    let manifest = PluginManifest::parse(full_plugin_toml()).expect("parse");
    let output = format_audit_summary(&manifest);

    let headings = [
        "Plugin: full-plugin v3.0.0",
        "Network access:",
        "Filesystem access:",
        "Host capabilities:",
        "Risk levels:",
    ];

    let mut last_pos = 0;
    for heading in &headings {
        let pos = output[last_pos..]
            .find(heading)
            .unwrap_or_else(|| panic!("missing or out-of-order section '{heading}'"));
        last_pos += pos + heading.len();
    }
}

// ---------------------------------------------------------------------------
// Scenario 2: plugin with no capabilities (minimal)
// ---------------------------------------------------------------------------

fn minimal_plugin_toml() -> &'static str {
    r#"
[plugin]
name = "minimal-plugin"
version = "0.0.1"
wasm_path = "minimal.wasm"
capabilities = []
permissions = []

[[tools]]
name = "noop"
description = "Does nothing"
export = "noop"
risk_level = "low"
parameters_schema = { type = "object" }
"#
}

#[test]
fn minimal_plugin_shows_none_for_network() {
    let manifest = PluginManifest::parse(minimal_plugin_toml()).expect("parse");
    let output = format_audit_summary(&manifest);

    let net_section = output.split("Network access:").nth(1).unwrap();
    let until_blank = net_section.split("\n\n").next().unwrap();
    assert!(until_blank.contains("(none)"), "network should show (none)");
}

#[test]
fn minimal_plugin_shows_none_for_filesystem() {
    let manifest = PluginManifest::parse(minimal_plugin_toml()).expect("parse");
    let output = format_audit_summary(&manifest);

    let fs_section = output.split("Filesystem access:").nth(1).unwrap();
    let until_blank = fs_section.split("\n\n").next().unwrap();
    assert!(until_blank.contains("(none)"), "filesystem should show (none)");
}

#[test]
fn minimal_plugin_shows_none_for_capabilities() {
    let manifest = PluginManifest::parse(minimal_plugin_toml()).expect("parse");
    let output = format_audit_summary(&manifest);

    let cap_section = output.split("Host capabilities:").nth(1).unwrap();
    let until_blank = cap_section.split("\n\n").next().unwrap();
    assert!(until_blank.contains("(none)"), "capabilities should show (none)");
}

#[test]
fn minimal_plugin_has_no_description_or_author() {
    let manifest = PluginManifest::parse(minimal_plugin_toml()).expect("parse");
    let output = format_audit_summary(&manifest);

    assert!(!output.contains("Description:"), "minimal plugin has no description");
    assert!(!output.contains("Author:"), "minimal plugin has no author");
}

#[test]
fn minimal_plugin_no_trailing_newline() {
    let manifest = PluginManifest::parse(minimal_plugin_toml()).expect("parse");
    let output = format_audit_summary(&manifest);
    assert!(!output.ends_with('\n'), "output should not end with newline");
}

// ---------------------------------------------------------------------------
// Scenario 3: plugin with wildcard hosts
// ---------------------------------------------------------------------------

fn wildcard_plugin_toml() -> &'static str {
    r#"
[plugin]
name = "wildcard-net-plugin"
version = "1.2.0"
wasm_path = "wildcard.wasm"
capabilities = ["tool"]

[plugin.network]
allowed_hosts = ["*.example.com", "*.internal.io", "exact-host.dev"]

[[tools]]
name = "fetch_all"
description = "Fetch from many hosts"
export = "fetch_all"
risk_level = "high"
parameters_schema = { type = "object" }
"#
}

#[test]
fn wildcard_hosts_displayed_with_check_marks() {
    let manifest = PluginManifest::parse(wildcard_plugin_toml()).expect("parse");
    let output = format_audit_summary(&manifest);

    assert!(output.contains("  ✓ *.example.com"), "wildcard *.example.com missing");
    assert!(output.contains("  ✓ *.internal.io"), "wildcard *.internal.io missing");
    assert!(output.contains("  ✓ exact-host.dev"), "exact host missing");
}

#[test]
fn wildcard_hosts_in_network_section() {
    let manifest = PluginManifest::parse(wildcard_plugin_toml()).expect("parse");
    let output = format_audit_summary(&manifest);

    let net_section = output.split("Network access:").nth(1).unwrap();
    let until_blank = net_section.split("\n\n").next().unwrap();

    // All three hosts should be in the network section specifically
    assert!(until_blank.contains("*.example.com"));
    assert!(until_blank.contains("*.internal.io"));
    assert!(until_blank.contains("exact-host.dev"));
    assert!(!until_blank.contains("(none)"));
}
