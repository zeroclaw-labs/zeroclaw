//! Verify acceptance criterion for US-ZCL-46:
//! Plugin results use category: plugins with ok/warn/error severity.
//!
//! This test inspects `src/doctor/mod.rs` to confirm:
//! 1. `check_plugin_health` uses `"plugins"` as the category for CLI diagnostics.
//! 2. `check_plugin_health` emits Ok, Warn, and Error severity items via DiagItem.
//! 3. `diagnose_plugins` uses `"plugins"` as the category for API JSON entries.
//! 4. `diagnose_plugins` uses `"error"` and `"warn"` severity strings in JSON entries.

#[test]
fn check_plugin_health_category_is_plugins() {
    use std::fs;
    use std::path::Path;

    let doctor_mod = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor/mod.rs");
    let source = fs::read_to_string(&doctor_mod).expect("failed to read src/doctor/mod.rs");

    // Locate the check_plugin_health function (cfg(feature = "plugins-wasm") version)
    let fn_pos = source
        .find("fn check_plugin_health(config: &Config")
        .or_else(|| source.find("fn check_plugin_health("))
        .expect("check_plugin_health function must exist");
    let fn_body = &source[fn_pos..];
    let body_end = fn_body[1..]
        .find("\n#[cfg(")
        .or_else(|| fn_body[1..].find("\nfn "))
        .map(|p| p + 1)
        .unwrap_or(fn_body.len());
    let fn_body = &fn_body[..body_end];

    // Category must be "plugins"
    assert!(
        fn_body.contains(r#"let cat = "plugins""#) || fn_body.contains(r#""plugins""#),
        "check_plugin_health must use \"plugins\" as the category string"
    );
}

#[test]
fn check_plugin_health_emits_all_three_severities() {
    use std::fs;
    use std::path::Path;

    let doctor_mod = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor/mod.rs");
    let source = fs::read_to_string(&doctor_mod).expect("failed to read src/doctor/mod.rs");

    let fn_pos = source
        .find("fn check_plugin_health(config: &Config")
        .or_else(|| source.find("fn check_plugin_health("))
        .expect("check_plugin_health function must exist");
    let fn_body = &source[fn_pos..];
    let body_end = fn_body[1..]
        .find("\n#[cfg(")
        .or_else(|| fn_body[1..].find("\nfn "))
        .map(|p| p + 1)
        .unwrap_or(fn_body.len());
    let fn_body = &fn_body[..body_end];

    // Must use DiagItem::ok for healthy/summary items
    assert!(
        fn_body.contains("DiagItem::ok("),
        "check_plugin_health must emit Ok severity items via DiagItem::ok"
    );

    // Must use DiagItem::warn for warning-level issues
    assert!(
        fn_body.contains("DiagItem::warn("),
        "check_plugin_health must emit Warn severity items via DiagItem::warn"
    );

    // Must use DiagItem::error for failure-level issues
    assert!(
        fn_body.contains("DiagItem::error("),
        "check_plugin_health must emit Error severity items via DiagItem::error"
    );
}

#[test]
fn diagnose_plugins_api_entries_use_plugins_category() {
    use std::fs;
    use std::path::Path;

    let doctor_mod = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor/mod.rs");
    let source = fs::read_to_string(&doctor_mod).expect("failed to read src/doctor/mod.rs");

    // Locate the diagnose_plugins_inner function (the cfg(feature) version)
    let fn_pos = source
        .find("fn diagnose_plugins_inner(config: &Config")
        .or_else(|| source.find("fn diagnose_plugins_inner("))
        .expect("diagnose_plugins_inner function must exist");
    let fn_body = &source[fn_pos..];
    let body_end = fn_body[1..]
        .find("\n#[cfg(")
        .or_else(|| fn_body[1..].find("\nfn "))
        .map(|p| p + 1)
        .unwrap_or(fn_body.len());
    let fn_body = &fn_body[..body_end];

    // All JSON entries must use category: "plugins"
    assert!(
        fn_body.contains(r#""category": "plugins""#),
        "diagnose_plugins_inner must produce JSON entries with category: \"plugins\""
    );

    // Count occurrences of category: "plugins" — should appear for both error and warn branches
    let category_count = fn_body.matches(r#""category": "plugins""#).count();
    assert!(
        category_count >= 2,
        "expected at least 2 JSON entries with category: \"plugins\" (error + warn branches), found {}",
        category_count
    );
}

#[test]
fn diagnose_plugins_api_entries_use_error_and_warn_severity() {
    use std::fs;
    use std::path::Path;

    let doctor_mod = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor/mod.rs");
    let source = fs::read_to_string(&doctor_mod).expect("failed to read src/doctor/mod.rs");

    let fn_pos = source
        .find("fn diagnose_plugins_inner(config: &Config")
        .or_else(|| source.find("fn diagnose_plugins_inner("))
        .expect("diagnose_plugins_inner function must exist");
    let fn_body = &source[fn_pos..];
    let body_end = fn_body[1..]
        .find("\n#[cfg(")
        .or_else(|| fn_body[1..].find("\nfn "))
        .map(|p| p + 1)
        .unwrap_or(fn_body.len());
    let fn_body = &fn_body[..body_end];

    // Must use "error" severity in JSON for failed plugins
    assert!(
        fn_body.contains(r#""severity": "error""#),
        "diagnose_plugins_inner must produce JSON entries with severity: \"error\" for failed plugins"
    );

    // Must use "warn" severity in JSON for warning-level plugins
    assert!(
        fn_body.contains(r#""severity": "warn""#),
        "diagnose_plugins_inner must produce JSON entries with severity: \"warn\" for warning-level plugins"
    );
}

#[test]
fn diagnose_plugins_api_passes_only_on_healthy_plugins() {
    use std::fs;
    use std::path::Path;

    let doctor_mod = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor/mod.rs");
    let source = fs::read_to_string(&doctor_mod).expect("failed to read src/doctor/mod.rs");

    let fn_pos = source
        .find("fn diagnose_plugins_inner(config: &Config")
        .or_else(|| source.find("fn diagnose_plugins_inner("))
        .expect("diagnose_plugins_inner function must exist");
    let fn_body = &source[fn_pos..];
    let body_end = fn_body[1..]
        .find("\n#[cfg(")
        .or_else(|| fn_body[1..].find("\nfn "))
        .map(|p| p + 1)
        .unwrap_or(fn_body.len());
    let fn_body = &fn_body[..body_end];

    // DiagStatus::Pass should not produce an entry (healthy plugins are not listed)
    assert!(
        fn_body.contains("DiagStatus::Pass => {}")
            || fn_body.contains("DiagStatus::Pass => {}")
            || fn_body.contains("Pass => {}"),
        "DiagStatus::Pass should produce no JSON entry — only failures and warnings are reported"
    );
}
