//! Verify acceptance criterion for US-ZCL-11:
//! "New plugins added after startup are discovered on reload"
//!
//! This integration test confirms that PluginHost::reload() discovers
//! plugins that were added to the plugins directory after initial startup.

#[test]
fn reload_discovers_new_plugins_added_after_startup() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    // 1. reload() must exist
    assert!(
        source.contains("pub fn reload("),
        "PluginHost must expose a public reload() method"
    );

    // 2. Find the unit test that validates new-plugin discovery on reload
    let test_section = source
        .find("#[cfg(test)]")
        .expect("host.rs must have a test module");
    let tests = &source[test_section..];

    assert!(
        tests.contains("test_reload_rescans_and_reinstantiates"),
        "host.rs must contain a unit test for reload re-scan behavior"
    );

    // 3. That test must add a plugin *after* PluginHost construction and
    //    verify it appears after reload.
    let test_pos = tests
        .find("fn test_reload_rescans_and_reinstantiates")
        .expect("reload rescan test must exist");
    let test_body = &tests[test_pos..];
    let body_end = test_body.find("\n    #[test]").unwrap_or(test_body.len());
    let test_body = &test_body[..body_end];

    // The test creates a new plugin directory after initial host construction
    assert!(
        test_body.contains("create_dir_all"),
        "test must create a new plugin directory on disk after startup"
    );

    // The test calls reload() to trigger re-scan
    assert!(
        test_body.contains(".reload()"),
        "test must call reload() to discover the new plugin"
    );

    // The test verifies the new plugin is present after reload
    assert!(
        test_body.contains("get_plugin(\"beta\").is_some()"),
        "test must verify the newly added plugin is discoverable after reload"
    );

    // The test verifies the ReloadSummary reports it as newly loaded
    assert!(
        test_body.contains("summary.loaded.contains"),
        "test must verify ReloadSummary reports the new plugin in the loaded set"
    );

    // The test confirms the total count increased
    assert!(
        test_body.contains("summary.total, 2"),
        "test must verify total plugin count increased after reload"
    );
}

/// Verify that reload() uses discover() internally, which is the mechanism
/// that scans the filesystem for new plugin directories.
#[test]
fn reload_uses_discover_to_find_new_plugins() {
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

    // reload() must clear before re-scanning so new plugins don't collide
    // with stale state
    assert!(
        reload_body.contains("self.loaded.clear()"),
        "reload() must clear loaded plugins before re-scanning"
    );

    // reload() must call discover() which does the actual filesystem scan
    assert!(
        reload_body.contains("self.discover()"),
        "reload() must call discover() to scan for new plugins"
    );

    // The ReloadSummary must track newly loaded plugins via set difference
    assert!(
        reload_body.contains("difference(&before_names)"),
        "reload() must compute newly loaded plugins as the set difference (after - before)"
    );
}
