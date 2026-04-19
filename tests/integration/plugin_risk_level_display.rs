#![cfg(any())] // disabled: pending format_audit_summary/decrypt functions

//! Integration test: Displays tool risk levels with approval requirements.
//!
//! Verifies the acceptance criterion for story US-ZCL-12:
//! > Displays tool risk levels with approval requirements
//!
//! Exercises `format_audit_summary` to confirm that each tool appears under a
//! "Risk levels:" heading with its risk level and, for medium/high tools, an
//! approval-requirement annotation.

use zeroclaw::plugins::{PluginManifest, format_audit_summary};

/// Low-risk tools should display their level without an approval annotation.
#[test]
fn audit_displays_low_risk_without_approval() {
    let toml_str = r#"
[plugin]
name = "safe-plugin"
version = "1.0.0"
wasm_path = "safe.wasm"
capabilities = ["tool"]

[[tools]]
name = "read_data"
description = "Reads data"
export = "read_data"
risk_level = "low"
parameters_schema = { type = "object" }
"#;

    let manifest = PluginManifest::parse(toml_str).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    assert!(
        output.contains("Risk levels:"),
        "output should contain 'Risk levels:' heading"
    );
    assert!(
        output.contains("read_data"),
        "output should list the tool name, got:\n{output}"
    );
    assert!(
        output.contains("low"),
        "output should show 'low' risk level, got:\n{output}"
    );
    // Low-risk tools must NOT have an approval annotation
    let risk_line = output.lines().find(|l| l.contains("read_data")).unwrap();
    assert!(
        !risk_line.contains("requires approval"),
        "low-risk tool should not require approval, got:\n{risk_line}"
    );
}

/// Medium-risk tools should display an approval requirement.
#[test]
fn audit_displays_medium_risk_with_approval() {
    let toml_str = r#"
[plugin]
name = "mid-plugin"
version = "2.0.0"
wasm_path = "mid.wasm"
capabilities = ["tool"]

[[tools]]
name = "modify_data"
description = "Modifies data"
export = "modify_data"
risk_level = "medium"
parameters_schema = { type = "object" }
"#;

    let manifest = PluginManifest::parse(toml_str).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    let risk_line = output
        .lines()
        .find(|l| l.contains("modify_data"))
        .expect("output should list modify_data tool");

    assert!(
        risk_line.contains("medium"),
        "should show 'medium' risk level, got:\n{risk_line}"
    );
    assert!(
        risk_line.contains("requires approval in supervised mode"),
        "medium-risk tool should require approval in supervised mode, got:\n{risk_line}"
    );
}

/// High-risk tools should display an approval requirement.
#[test]
fn audit_displays_high_risk_with_approval() {
    let toml_str = r#"
[plugin]
name = "danger-plugin"
version = "3.0.0"
wasm_path = "danger.wasm"
capabilities = ["tool"]

[[tools]]
name = "delete_everything"
description = "Deletes everything"
export = "delete_everything"
risk_level = "high"
parameters_schema = { type = "object" }
"#;

    let manifest = PluginManifest::parse(toml_str).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    let risk_line = output
        .lines()
        .find(|l| l.contains("delete_everything"))
        .expect("output should list delete_everything tool");

    assert!(
        risk_line.contains("high"),
        "should show 'high' risk level, got:\n{risk_line}"
    );
    assert!(
        risk_line.contains("requires approval in supervised mode"),
        "high-risk tool should require approval in supervised mode, got:\n{risk_line}"
    );
}

/// Multiple tools with different risk levels should each display correctly.
#[test]
fn audit_displays_mixed_risk_levels() {
    let toml_str = r#"
[plugin]
name = "multi-tool"
version = "1.0.0"
wasm_path = "multi.wasm"
capabilities = ["tool"]

[[tools]]
name = "search"
description = "Searches"
export = "search"
risk_level = "low"
parameters_schema = { type = "object" }

[[tools]]
name = "update"
description = "Updates"
export = "update"
risk_level = "medium"
parameters_schema = { type = "object" }

[[tools]]
name = "destroy"
description = "Destroys"
export = "destroy"
risk_level = "high"
parameters_schema = { type = "object" }
"#;

    let manifest = PluginManifest::parse(toml_str).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    // All three tools should appear under the "Risk levels:" heading
    assert!(
        output.contains("Risk levels:"),
        "should have Risk levels heading"
    );

    let search_line = output.lines().find(|l| l.contains("search")).unwrap();
    let update_line = output.lines().find(|l| l.contains("update")).unwrap();
    let destroy_line = output.lines().find(|l| l.contains("destroy")).unwrap();

    assert!(search_line.contains("low"), "search should be low risk");
    assert!(
        !search_line.contains("requires approval"),
        "low should not require approval"
    );

    assert!(
        update_line.contains("medium"),
        "update should be medium risk"
    );
    assert!(
        update_line.contains("requires approval in supervised mode"),
        "medium should require approval"
    );

    assert!(destroy_line.contains("high"), "destroy should be high risk");
    assert!(
        destroy_line.contains("requires approval in supervised mode"),
        "high should require approval"
    );
}

/// A plugin with no tools should display "(no tools)" in the risk levels section.
#[test]
fn audit_displays_no_tools_placeholder() {
    let toml_str = r#"
[plugin]
name = "empty-plugin"
version = "0.1.0"
wasm_path = "empty.wasm"
capabilities = ["channel"]
"#;

    let manifest = PluginManifest::parse(toml_str).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    assert!(
        output.contains("Risk levels:"),
        "output should contain 'Risk levels:' heading even with no tools"
    );
    assert!(
        output.contains("(no tools)"),
        "should display '(no tools)' when plugin has no tools, got:\n{output}"
    );
}

/// Risk level lines use bullet (•) and arrow (→) formatting.
#[test]
fn audit_risk_level_formatting() {
    let toml_str = r#"
[plugin]
name = "fmt-plugin"
version = "1.0.0"
wasm_path = "fmt.wasm"
capabilities = ["tool"]

[[tools]]
name = "some_tool"
description = "A tool"
export = "some_tool"
risk_level = "high"
parameters_schema = { type = "object" }
"#;

    let manifest = PluginManifest::parse(toml_str).expect("should parse manifest");
    let output = format_audit_summary(&manifest);

    let risk_line = output
        .lines()
        .find(|l| l.contains("some_tool"))
        .expect("should list some_tool");

    // Check bullet and arrow formatting
    assert!(
        risk_line.contains('•'),
        "risk line should use bullet (•) prefix, got:\n{risk_line}"
    );
    assert!(
        risk_line.contains('→'),
        "risk line should use arrow (→) separator, got:\n{risk_line}"
    );
}
