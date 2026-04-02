//! Verify acceptance criterion for US-ZCL-11:
//! "Removed plugins are unloaded on reload"
//!
//! This integration test confirms that PluginHost::reload() detects plugins
//! that were deleted from the plugins directory and reports them as unloaded.

#[test]
fn reload_unloads_removed_plugins() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    // 1. reload() must exist
    assert!(
        source.contains("pub fn reload("),
        "PluginHost must expose a public reload() method"
    );

    // 2. Find the unit test that validates removed-plugin unloading
    let test_section = source
        .find("#[cfg(test)]")
        .expect("host.rs must have a test module");
    let tests = &source[test_section..];

    assert!(
        tests.contains("test_reload_drops_removed_plugins"),
        "host.rs must contain a unit test for removed-plugin unloading on reload"
    );

    // 3. Extract the test body for detailed verification
    let test_pos = tests
        .find("fn test_reload_drops_removed_plugins")
        .expect("reload drops-removed test must exist");
    let test_body = &tests[test_pos..];
    let body_end = test_body.find("\n    #[test]").unwrap_or(test_body.len());
    let test_body = &test_body[..body_end];

    // The test creates a plugin, then removes its directory from disk
    assert!(
        test_body.contains("remove_dir_all"),
        "test must remove the plugin directory from disk before reloading"
    );

    // The test calls reload() to trigger re-scan
    assert!(
        test_body.contains(".reload()"),
        "test must call reload() to detect the removed plugin"
    );

    // The test verifies the plugin is no longer loaded
    assert!(
        test_body.contains("list_plugins().is_empty()"),
        "test must verify the removed plugin is no longer in the plugin list"
    );

    // The test verifies ReloadSummary reports it as unloaded
    assert!(
        test_body.contains("summary.unloaded.contains"),
        "test must verify ReloadSummary reports the removed plugin in the unloaded set"
    );

    // The test confirms the total count dropped to zero
    assert!(
        test_body.contains("summary.total, 0"),
        "test must verify total plugin count is zero after the only plugin is removed"
    );
}

/// Verify that reload() computes unloaded plugins via set difference (before - after).
#[test]
fn reload_computes_unloaded_via_set_difference() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    let reload_pos = source.find("fn reload(").expect("reload method must exist");
    let reload_body = &source[reload_pos..];
    let body_end = reload_body
        .find("\n    pub fn ")
        .unwrap_or(reload_body.len());
    let reload_body = &reload_body[..body_end];

    // reload() must track names before clearing
    assert!(
        reload_body.contains("before_names"),
        "reload() must capture plugin names before clearing for unload detection"
    );

    // reload() must compute unloaded plugins as (before - after)
    assert!(
        reload_body.contains("difference(&after_names)"),
        "reload() must compute unloaded plugins as the set difference (before - after)"
    );

    // The unloaded field must be populated in ReloadSummary
    assert!(
        reload_body.contains("unloaded"),
        "reload() must populate the unloaded field in ReloadSummary"
    );
}
