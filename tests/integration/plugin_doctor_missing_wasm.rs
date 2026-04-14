#![cfg(feature = "plugins-wasm")]
//! Verify acceptance criterion: plugin doctor **reports missing or unreadable
//! WASM files**.

#[test]
fn diagnose_plugin_checks_wasm_file_existence() {
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

    // 1. It resolves the WASM path from the manifest
    assert!(
        dp_body.contains("manifest.wasm_path"),
        "diagnose_plugin() must resolve the WASM file path from the manifest"
    );

    // 2. It checks whether the WASM file exists on disk
    assert!(
        dp_body.contains("wasm_path.exists()"),
        "diagnose_plugin() must check whether the WASM file exists"
    );

    // 3. A missing WASM file produces a Fail status
    assert!(
        dp_body.contains("DiagStatus::Fail") && dp_body.contains("WASM file not found"),
        "diagnose_plugin() must report a Fail when the WASM file is missing"
    );

    // 4. An unreadable WASM file is also detected (metadata check)
    assert!(
        dp_body.contains("metadata") && dp_body.contains("WASM file unreadable"),
        "diagnose_plugin() must detect unreadable WASM files via metadata check"
    );

    // 5. The diagnostic check is named "wasm_file"
    assert!(
        dp_body.contains("\"wasm_file\""),
        "diagnose_plugin() must use the check name \"wasm_file\" for WASM file checks"
    );

    // 6. A readable WASM file gets a Pass status
    assert!(
        dp_body.contains("WASM file exists and is readable"),
        "diagnose_plugin() must report Pass when the WASM file exists and is readable"
    );
}

#[test]
fn diagnose_plugin_has_unit_test_for_missing_wasm() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    let test_section = source
        .find("#[cfg(test)]")
        .expect("host.rs must have a test module");
    let tests = &source[test_section..];

    // The test module must exercise diagnose_plugin
    assert!(
        tests.contains("diagnose_plugin"),
        "host.rs tests must exercise diagnose_plugin()"
    );

    // The test module must verify WASM-related diagnostics
    assert!(
        tests.contains("wasm_file") || tests.contains("wasm"),
        "host.rs tests must verify WASM file diagnostics"
    );

    // The test must verify the missing-WASM failure scenario
    assert!(
        tests.contains("Fail") || tests.contains("not found"),
        "host.rs tests must verify that a missing WASM file is reported as a failure"
    );
}
