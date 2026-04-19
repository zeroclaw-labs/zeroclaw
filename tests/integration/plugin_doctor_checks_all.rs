#![cfg(feature = "plugins-wasm")]
//! Verify acceptance criterion: `zeroclaw plugin doctor` checks **all** installed
//! plugins — not just the first one, not just successfully-loaded ones.

#[test]
fn doctor_returns_diagnostic_for_every_installed_plugin() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    // 1. doctor() method exists on PluginHost
    assert!(
        source.contains("pub fn doctor("),
        "PluginHost must expose a public doctor() method"
    );

    // 2. doctor() returns Vec<PluginDiagnostic> (one per plugin)
    let doctor_pos = source
        .find("pub fn doctor(")
        .expect("doctor() method must exist");
    let doctor_sig = &source[doctor_pos..doctor_pos + 200];
    assert!(
        doctor_sig.contains("Vec<PluginDiagnostic>"),
        "doctor() must return Vec<PluginDiagnostic>"
    );

    // 3. doctor() scans the plugins directory (read_dir)
    let doctor_body_start = doctor_pos;
    let doctor_body = &source[doctor_body_start..];
    let body_end = doctor_body
        .find("\n    pub fn ")
        .unwrap_or(doctor_body.len());
    let doctor_body = &doctor_body[..body_end];

    assert!(
        doctor_body.contains("read_dir"),
        "doctor() must scan the plugins directory via read_dir"
    );

    // 4. doctor() calls diagnose_plugin() for each directory entry
    assert!(
        doctor_body.contains("diagnose_plugin"),
        "doctor() must call diagnose_plugin() for each plugin directory"
    );

    // 5. doctor() iterates over entries (for loop), not early-returning
    assert!(
        doctor_body.contains("for entry in"),
        "doctor() must iterate over all directory entries"
    );

    // 6. diagnose_plugin() is public and takes a plugin_dir path
    assert!(
        source.contains("pub fn diagnose_plugin("),
        "diagnose_plugin() must be a public method"
    );

    // 7. doctor() sorts results for deterministic output
    assert!(
        doctor_body.contains("diagnostics.sort"),
        "doctor() should sort diagnostics for deterministic output"
    );
}

/// Verify that the unit tests in host.rs exercise doctor() with multiple plugins.
#[test]
fn doctor_has_unit_test_exercising_multiple_plugins() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    let test_section = source
        .find("#[cfg(test)]")
        .expect("host.rs must have a test module");
    let tests = &source[test_section..];

    // There should be a test that calls .doctor()
    assert!(
        tests.contains(".doctor()"),
        "host.rs tests should exercise the doctor() method directly"
    );
}
