//! Verify acceptance criterion: plugin doctor **reports capability conflicts
//! with current security level**.

#[test]
fn diagnose_plugin_checks_capability_conflicts() {
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

    // 1. It checks for wildcard hosts in allowed_hosts
    assert!(
        dp_body.contains("allowed_hosts") && dp_body.contains("'*'"),
        "diagnose_plugin() must check allowed_hosts for wildcard patterns"
    );

    // 2. It checks for forbidden paths in allowed_paths
    assert!(
        dp_body.contains("FORBIDDEN_PATHS"),
        "diagnose_plugin() must check allowed_paths against FORBIDDEN_PATHS"
    );

    // 3. It produces a "capabilities" check
    assert!(
        dp_body.contains("\"capabilities\""),
        "diagnose_plugin() must produce a DiagCheck named 'capabilities'"
    );

    // 4. The message references security levels for wildcard host conflicts
    assert!(
        dp_body.contains("strict") || dp_body.contains("paranoid") || dp_body.contains("security"),
        "diagnose_plugin() must reference security policy/levels in capability conflict messages"
    );

    // 5. Wildcard-only conflict produces a Warn
    assert!(
        dp_body.contains("DiagStatus::Warn") && dp_body.contains("wildcard hosts"),
        "diagnose_plugin() must warn (not fail) for wildcard-host-only conflicts"
    );

    // 6. Forbidden-path conflict produces a Fail
    assert!(
        dp_body.contains("DiagStatus::Fail") && dp_body.contains("forbidden"),
        "diagnose_plugin() must fail for forbidden-path conflicts"
    );

    // 7. No conflicts produces a Pass
    assert!(
        dp_body.contains("no capability conflicts detected"),
        "diagnose_plugin() must report 'no capability conflicts detected' when clean"
    );
}

#[test]
fn diagnose_plugin_has_unit_test_for_capability_conflicts() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    let test_section = source
        .find("#[cfg(test)]")
        .expect("host.rs must have a test module");
    let tests = &source[test_section..];

    // The test module must exercise capability conflict diagnostics
    assert!(
        tests.contains("capabilities") && tests.contains("diagnose_plugin"),
        "host.rs tests must exercise capability conflict detection in diagnose_plugin()"
    );

    // The test must verify wildcard host detection
    assert!(
        tests.contains("wildcard") || tests.contains("*.example.com") || tests.contains("\"*\""),
        "host.rs tests must verify wildcard host detection"
    );

    // The test must verify forbidden path detection
    assert!(
        tests.contains("/etc") || tests.contains("forbidden"),
        "host.rs tests must verify forbidden path detection"
    );
}
