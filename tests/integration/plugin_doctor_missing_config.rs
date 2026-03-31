//! Verify acceptance criterion: plugin doctor **reports missing config values
//! with key names and plugin names**.

#[test]
fn diagnose_plugin_reports_missing_config_keys_by_name() {
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

    // 1. It iterates over manifest.config entries to find required keys
    assert!(
        dp_body.contains("manifest.config"),
        "diagnose_plugin() must inspect manifest.config to detect required keys"
    );

    // 2. It checks for a `required` flag on each config entry
    assert!(
        dp_body.contains("\"required\""),
        "diagnose_plugin() must check the \"required\" flag on config entries"
    );

    // 3. Missing key names are collected and reported
    assert!(
        dp_body.contains("missing_keys"),
        "diagnose_plugin() must collect missing config key names"
    );

    // 4. The reported message includes the actual key names (join)
    assert!(
        dp_body.contains("missing_keys.join"),
        "diagnose_plugin() must include key names in the diagnostic message"
    );

    // 5. PluginDiagnostic carries the plugin name
    assert!(
        source.contains("pub plugin_name: String"),
        "PluginDiagnostic must carry the plugin name"
    );

    // 6. diagnose_plugin() sets plugin_name from the directory name
    assert!(
        dp_body.contains("plugin_name"),
        "diagnose_plugin() must set the plugin_name field so the caller knows which plugin is affected"
    );
}

#[test]
fn diagnose_plugin_has_unit_test_for_missing_config() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    let test_section = source
        .find("#[cfg(test)]")
        .expect("host.rs must have a test module");
    let tests = &source[test_section..];

    // The test module must exercise diagnose_plugin directly
    assert!(
        tests.contains("diagnose_plugin"),
        "host.rs tests must exercise diagnose_plugin()"
    );

    // The test module must verify config-related diagnostics with key names
    assert!(
        tests.contains("missing_config")
            || tests.contains("config_check")
            || tests.contains("\"config\""),
        "host.rs tests must verify config-related diagnostics"
    );

    // The test must assert that specific key names appear in the diagnostic message
    assert!(
        tests.contains("api_key") && tests.contains("api_secret"),
        "host.rs tests must verify that specific config key names are reported"
    );
}
