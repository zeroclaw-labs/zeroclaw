// crates/osagent-manifest/tests/manifest_diff.rs — TDD test suite for the
// MANIFEST.toml emission + `osagent manifest --diff` behavior.

use osagent_manifest::{Manifest, ManifestDiffError, manifest_diff};

fn engineer_manifest() -> Manifest {
    Manifest {
        schema_version: 1,
        binary_name: "engineer".into(),
        binary_version: "0.1.0".into(),
        fork_provenance: "a2988f0dfffa0c14fa56e218c7bb9f28da494da4".into(),
        declared: osagent_manifest::Section {
            channels: vec!["telegram".into(), "slack".into()],
            providers: vec!["anthropic".into(), "gemini".into()],
            tools: vec!["bridge".into(), "mcp".into()],
        },
        detected: osagent_manifest::Section {
            channels: vec!["telegram".into(), "slack".into()],
            providers: vec!["anthropic".into(), "gemini".into()],
            tools: vec!["bridge".into(), "mcp".into()],
        },
    }
}

#[test]
fn manifest_diff_ok_when_config_subset_of_declared() {
    let manifest = engineer_manifest();
    let config = r#"
[channels.telegram]
bot_token = "..."

[providers.anthropic]
api_key = "..."

[[tools]]
name = "bridge"
"#;
    assert!(manifest_diff(&manifest, config).is_ok());
}

#[test]
fn manifest_diff_rejects_config_referencing_missing_channel() {
    let manifest = engineer_manifest();
    let config = r#"
[channels.discord]
bot_token = "..."
"#;
    let err = manifest_diff(&manifest, config).expect_err("should reject discord");
    match err {
        ManifestDiffError::MissingChannel(name) => assert_eq!(name, "discord"),
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn manifest_diff_rejects_config_referencing_missing_tool() {
    let manifest = engineer_manifest();
    let config = r#"
[[tools]]
name = "browser"
"#;
    let err = manifest_diff(&manifest, config).expect_err("should reject browser tool");
    match err {
        ManifestDiffError::MissingTool(name) => assert_eq!(name, "browser"),
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn manifest_diff_rejects_config_referencing_missing_provider() {
    let manifest = engineer_manifest();
    let config = r#"
default_provider = "openai"
"#;
    let err = manifest_diff(&manifest, config).expect_err("should reject openai");
    match err {
        ManifestDiffError::MissingProvider(name) => assert_eq!(name, "openai"),
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn manifest_diff_detects_declared_detected_divergence() {
    // Catch the "feature declared but code orphaned" + "code linked but not declared" cases.
    let mut manifest = engineer_manifest();
    manifest.detected.channels.pop();  // declared has slack+telegram; detected has only telegram
    let err = manifest.self_consistency_check().expect_err("must detect divergence");
    match err {
        ManifestDiffError::DeclaredDetectedMismatch { kind, declared_only, detected_only } => {
            assert_eq!(kind, "channels");
            assert!(declared_only.contains(&"slack".to_string()));
            assert!(detected_only.is_empty());
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn wizard_manifest_with_no_mcp_rejects_config_with_mcp_section() {
    // Load-bearing for the wizard binary's structural exclusion.
    let mut manifest = engineer_manifest();
    manifest.binary_name = "wizard".into();
    manifest.declared.tools.retain(|t| t != "mcp");
    manifest.detected.tools.retain(|t| t != "mcp");
    let config = r#"
[mcp]
enabled = true

[[mcp.servers]]
name = "openspace"
"#;
    let err = manifest_diff(&manifest, config).expect_err("wizard must reject mcp config");
    match err {
        ManifestDiffError::MissingTool(name) => assert_eq!(name, "mcp"),
        other => panic!("unexpected error variant: {other:?}"),
    }
}
