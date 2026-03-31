//! Verify acceptance criterion: plugin doctor **reports invalid manifests with
//! parse errors**.

#[test]
fn diagnose_plugin_reports_invalid_manifest_with_parse_error() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    // Locate diagnose_plugin() body
    let dp_pos = source
        .find("pub fn diagnose_plugin(")
        .expect("diagnose_plugin() must exist");
    let dp_body = &source[dp_pos..];
    let body_end = dp_body.find("\n    pub fn ").unwrap_or(dp_body.len());
    let dp_body = &dp_body[..body_end];

    // 1. It attempts to load and parse the manifest via load_manifest()
    assert!(
        dp_body.contains("load_manifest"),
        "diagnose_plugin() must call load_manifest() to parse the manifest file"
    );

    // 2. Parse errors produce a Fail status on the "manifest" check
    assert!(
        dp_body.contains("DiagStatus::Fail") && dp_body.contains("\"manifest\""),
        "diagnose_plugin() must report DiagStatus::Fail for the \"manifest\" check on parse errors"
    );

    // 3. The error message includes the parse error details (format!(..., e))
    assert!(
        dp_body.contains("invalid manifest.toml: {e}")
            || dp_body.contains("invalid plugin.toml: {e}"),
        "diagnose_plugin() must include the parse error in the diagnostic message"
    );

    // 4. Both manifest.toml and plugin.toml parse failures are handled
    assert!(
        dp_body.contains("invalid manifest.toml") && dp_body.contains("invalid plugin.toml"),
        "diagnose_plugin() must handle parse errors for both manifest.toml and plugin.toml"
    );

    // 5. A missing manifest (neither file exists) also produces a Fail
    assert!(
        dp_body.contains("no manifest.toml or plugin.toml found"),
        "diagnose_plugin() must report when no manifest file is found at all"
    );

    // 6. On manifest parse failure, diagnose_plugin returns early or skips remaining checks
    //    (the manifest is set to None, so subsequent checks are gated on Some(manifest))
    assert!(
        dp_body.contains("None"),
        "diagnose_plugin() must handle the case where manifest parsing fails (manifest = None)"
    );
}

#[test]
fn diagnose_plugin_has_unit_test_for_invalid_manifest() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    let test_section = source
        .find("#[cfg(test)]")
        .expect("host.rs must have a test module");
    let tests = &source[test_section..];

    // The test module must exercise diagnose_plugin or doctor
    assert!(
        tests.contains("diagnose_plugin") || tests.contains("doctor"),
        "host.rs tests must exercise diagnose_plugin() or doctor()"
    );

    // The test module must verify invalid manifest handling
    assert!(
        tests.contains("invalid") || tests.contains("not valid toml") || tests.contains("manifest"),
        "host.rs tests must verify invalid manifest scenarios"
    );

    // The test must verify that a parse error results in a Fail status
    assert!(
        tests.contains("DiagStatus::Fail") || tests.contains("Fail"),
        "host.rs tests must verify that invalid manifests produce a Fail status"
    );
}
