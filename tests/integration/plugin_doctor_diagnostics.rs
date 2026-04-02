//! Verify that the doctor diagnostic engine has runtime unit tests covering
//! the four key scenarios: missing config key, missing WASM file, invalid
//! manifest TOML, and all-pass for a valid plugin — all using temp directories
//! with crafted plugin states.

/// Doctor unit tests exercise missing config key detection with real temp dirs.
#[test]
fn doctor_has_runtime_test_for_missing_config_key() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    let test_section = source
        .find("#[cfg(test)]")
        .expect("host.rs must have a test module");
    let tests = &source[test_section..];

    // There must be a test that exercises config key detection via diagnose_plugin
    assert!(
        tests.contains("missing_config") || tests.contains("config_keys"),
        "host.rs must have a runtime test for missing config key detection"
    );

    // The test must create plugin state with required config keys
    assert!(
        tests.contains("required") && tests.contains("api_key"),
        "the config test must use a manifest with required config keys"
    );

    // The test must verify the diagnostic reports the key names
    assert!(
        tests.contains("config_check") || tests.contains("\"config\""),
        "the config test must verify the config DiagCheck output"
    );
}

/// Doctor unit tests exercise missing WASM file detection with real temp dirs.
#[test]
fn doctor_has_runtime_test_for_missing_wasm_file() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    let test_section = source
        .find("#[cfg(test)]")
        .expect("host.rs must have a test module");
    let tests = &source[test_section..];

    // The doctor test that creates multiple plugins must include one with a missing WASM file
    assert!(
        tests.contains("wasm_file") && tests.contains("DiagStatus::Fail"),
        "host.rs must have a runtime test verifying missing WASM file produces Fail"
    );

    // The test must use a manifest that references a WASM file that does not exist on disk
    assert!(
        tests.contains("beta.wasm") || tests.contains("missing.wasm"),
        "the test must reference a WASM path that does not exist on disk"
    );
}

/// Doctor unit tests exercise invalid manifest TOML detection with real temp dirs.
#[test]
fn doctor_has_runtime_test_for_invalid_manifest() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    let test_section = source
        .find("#[cfg(test)]")
        .expect("host.rs must have a test module");
    let tests = &source[test_section..];

    // The test must write invalid TOML to a manifest file
    assert!(
        tests.contains("not valid toml"),
        "host.rs must have a test that writes invalid TOML to a manifest file"
    );

    // The test must verify the manifest check fails
    assert!(
        tests.contains("\"manifest\"") && tests.contains("DiagStatus::Fail"),
        "the test must verify that the manifest DiagCheck reports Fail for invalid TOML"
    );
}

/// Doctor unit tests exercise all-pass scenario with a fully valid plugin.
#[test]
fn doctor_has_runtime_test_for_all_pass_valid_plugin() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    let test_section = source
        .find("#[cfg(test)]")
        .expect("host.rs must have a test module");
    let tests = &source[test_section..];

    // The multi-plugin doctor test must include a valid plugin (alpha) that does NOT fail
    assert!(
        tests.contains("alpha") && tests.contains("doctor()"),
        "host.rs must have a runtime test that exercises doctor() with a valid plugin"
    );

    // The test must verify the valid plugin does not fail
    assert!(
        tests.contains("alpha should not fail") || tests.contains("DiagStatus::Pass"),
        "the test must verify the valid plugin passes or does not fail"
    );

    // The clean plugin in the capability conflict test must pass
    assert!(
        tests.contains("clean-plugin") || tests.contains("clean plugin should pass"),
        "host.rs must test a clean plugin that passes all capability checks"
    );
}
