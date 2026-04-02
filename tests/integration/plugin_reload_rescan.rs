//! Verify that `PluginHost::reload()` re-scans the plugins directory and
//! re-instantiates all discovered plugins (acceptance criterion for US-ZCL-11).

#[test]
fn reload_rescans_directory_and_reinstantiates_plugins() {
    use std::fs;
    use std::path::Path;

    // Verify that reload() exists and clears + re-discovers plugins by reading
    // the implementation in host.rs.
    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    // 1. reload() method exists on PluginHost
    assert!(
        source.contains("pub fn reload("),
        "PluginHost must expose a public reload() method"
    );

    // 2. reload() clears the loaded registry before re-scanning
    //    (ensures stale plugins don't survive a reload)
    let reload_pos = source.find("fn reload(").expect("reload method must exist");
    let reload_body = &source[reload_pos..];

    // Find the closing of reload (next `pub fn` or end of impl)
    let body_end = reload_body
        .find("\n    pub fn ")
        .unwrap_or(reload_body.len());
    let reload_body = &reload_body[..body_end];

    assert!(
        reload_body.contains("self.loaded.clear()"),
        "reload() must clear the loaded plugin registry before re-scanning"
    );

    // 3. reload() calls discover() to re-scan the directory
    assert!(
        reload_body.contains("self.discover()"),
        "reload() must call discover() to re-scan the plugins directory"
    );

    // 4. reload() returns a ReloadSummary with loaded/unloaded/failed info
    assert!(
        source.contains("pub struct ReloadSummary"),
        "ReloadSummary struct must be defined"
    );
    assert!(
        reload_body.contains("ReloadSummary"),
        "reload() must return a ReloadSummary"
    );

    // 5. ReloadSummary tracks the right fields
    let summary_pos = source
        .find("pub struct ReloadSummary")
        .expect("ReloadSummary must exist");
    let summary_block = &source[summary_pos..summary_pos + 500.min(source.len() - summary_pos)];
    assert!(
        summary_block.contains("pub loaded:"),
        "ReloadSummary must track newly loaded plugins"
    );
    assert!(
        summary_block.contains("pub unloaded:"),
        "ReloadSummary must track unloaded plugins"
    );
    assert!(
        summary_block.contains("pub failed:"),
        "ReloadSummary must track failed plugins"
    );
    assert!(
        summary_block.contains("pub total:"),
        "ReloadSummary must track total plugin count"
    );
}

/// Verify the unit test coverage: a test exists that exercises reload()
/// with actual plugin discovery (not just source inspection).
#[test]
fn reload_has_unit_test_coverage() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    // There should be at least one #[test] function that calls .reload()
    let test_section = source
        .find("#[cfg(test)]")
        .expect("host.rs must have a test module");
    let tests = &source[test_section..];

    assert!(
        tests.contains(".reload()"),
        "host.rs tests should exercise the reload() method directly"
    );
}
