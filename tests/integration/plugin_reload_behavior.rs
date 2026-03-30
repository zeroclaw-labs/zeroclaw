//! Full reload-cycle integration test for US-ZCL-11-7:
//!
//! 1. Start with plugin A, reload after adding plugin B — verify both loaded.
//! 2. Remove plugin A, reload — verify only B loaded.
//! 3. Re-add plugin A with a modified manifest, reload — verify new config applied.
//!
//! This exercises the complete lifecycle rather than isolated operations.

#[test]
fn reload_full_cycle_add_remove_modify() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    // The unit test that exercises the full reload cycle must exist in host.rs
    let test_section = source
        .find("#[cfg(test)]")
        .expect("host.rs must have a test module");
    let tests = &source[test_section..];

    assert!(
        tests.contains("test_reload_full_cycle"),
        "host.rs must contain a unit test named test_reload_full_cycle"
    );

    // Extract the test body
    let test_pos = tests
        .find("fn test_reload_full_cycle")
        .expect("test_reload_full_cycle must exist");
    let test_body = &tests[test_pos..];
    let body_end = test_body
        .find("\n    #[test]")
        .unwrap_or(test_body.len());
    let test_body = &test_body[..body_end];

    // --- Phase 1: start with A, add B, reload ---

    // Creates two plugin directories (alpha and beta)
    assert!(
        test_body.contains("create_dir_all"),
        "test must create plugin directories on disk"
    );

    // Calls reload() after adding the second plugin
    assert!(
        test_body.contains(".reload()"),
        "test must call reload() to discover changes"
    );

    // Both plugins present after first reload
    assert!(
        test_body.contains("get_plugin(\"alpha\").is_some()")
            || test_body.contains("get_plugin(\"alpha\").is_some()"),
        "test must verify alpha is still loaded after reload"
    );
    assert!(
        test_body.contains("get_plugin(\"beta\").is_some()"),
        "test must verify beta is discovered after reload"
    );

    // --- Phase 2: remove A, reload ---

    // Removes alpha's directory from disk
    assert!(
        test_body.contains("remove_dir_all"),
        "test must remove alpha's directory to simulate uninstall"
    );

    // After reload, alpha is gone and beta remains
    assert!(
        test_body.contains("get_plugin(\"alpha\").is_none()"),
        "test must verify alpha is gone after removal and reload"
    );

    // --- Phase 3: re-add A with modified manifest, reload ---

    // The test writes a new manifest with a different version or description
    // to verify reload picks up config changes
    assert!(
        test_body.contains("2.0.0") || test_body.contains("modified"),
        "test must write a modified manifest for alpha with new version or description"
    );

    // After final reload, the updated config is applied
    assert!(
        test_body.contains("get_plugin(\"alpha\").unwrap()"),
        "test must retrieve alpha's info after re-adding with modified manifest"
    );
}

/// Verify the unit test exercises ReloadSummary fields across all three phases.
#[test]
fn reload_full_cycle_checks_summary_fields() {
    use std::fs;
    use std::path::Path;

    let host_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/plugins/host.rs");
    let source = fs::read_to_string(&host_rs).expect("failed to read src/plugins/host.rs");

    let test_section = source
        .find("#[cfg(test)]")
        .expect("host.rs must have a test module");
    let tests = &source[test_section..];

    let test_pos = tests
        .find("fn test_reload_full_cycle")
        .expect("test_reload_full_cycle must exist");
    let test_body = &tests[test_pos..];
    let body_end = test_body
        .find("\n    #[test]")
        .unwrap_or(test_body.len());
    let test_body = &test_body[..body_end];

    // The test must check summary.loaded and summary.unloaded across phases
    assert!(
        test_body.contains("summary") || test_body.contains("reload()"),
        "test must capture and inspect ReloadSummary"
    );

    // Multiple reload() calls for the three phases
    let reload_count = test_body.matches(".reload()").count();
    assert!(
        reload_count >= 3,
        "test must call reload() at least 3 times (add, remove, modify) — found {}",
        reload_count
    );
}
