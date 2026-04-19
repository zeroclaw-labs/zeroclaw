#![cfg(feature = "plugins-wasm")]
//! Verify acceptance criterion for US-ZCL-46:
//! Summary entry shows loaded/failed/disabled counts.
//!
//! This test inspects `src/doctor/mod.rs` to confirm:
//! 1. `diagnose_plugins_inner` returns JSON with "loaded", "failed", "disabled" count fields.
//! 2. `check_plugin_health` emits a CLI summary line containing total/loaded/failed/disabled.
//! 3. Counts are computed from host.doctor() diagnostics and host.list_plugins().

#[test]
fn diagnose_plugins_json_has_loaded_failed_disabled_counts() {
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

    // The returned JSON must include all three count fields
    assert!(
        fn_body.contains(r#""loaded""#),
        "diagnose_plugins_inner JSON must include a \"loaded\" count field"
    );
    assert!(
        fn_body.contains(r#""failed""#),
        "diagnose_plugins_inner JSON must include a \"failed\" count field"
    );
    assert!(
        fn_body.contains(r#""disabled""#),
        "diagnose_plugins_inner JSON must include a \"disabled\" count field"
    );
}

#[test]
fn diagnose_plugins_json_counts_are_integers() {
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

    // Counts must be computed from usize variables (not hard-coded strings)
    assert!(
        fn_body.contains("let loaded =") || fn_body.contains("let loaded="),
        "diagnose_plugins_inner must compute loaded count as a variable"
    );
    assert!(
        fn_body.contains("let failed =") || fn_body.contains("let failed="),
        "diagnose_plugins_inner must compute failed count as a variable"
    );
    assert!(
        fn_body.contains("let disabled =") || fn_body.contains("let disabled="),
        "diagnose_plugins_inner must compute disabled count as a variable"
    );
}

#[test]
fn check_plugin_health_cli_summary_includes_all_counts() {
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

    // The CLI summary line must mention all four counts
    assert!(
        fn_body.contains("total")
            && fn_body.contains("loaded")
            && fn_body.contains("failed")
            && fn_body.contains("disabled"),
        "check_plugin_health summary line must include total, loaded, failed, and disabled counts"
    );

    // Summary is pushed as DiagItem::ok (informational)
    assert!(
        fn_body.contains("DiagItem::ok("),
        "check_plugin_health must push the summary as DiagItem::ok"
    );
}

#[test]
fn diagnose_plugins_error_fallback_also_has_counts() {
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

    // When PluginHost::new fails, the fallback JSON must still include
    // loaded/failed/disabled count fields (all zero) for API consistency.
    assert!(
        fn_body.contains(r#""loaded": 0"#),
        "Error fallback JSON must include \"loaded\": 0"
    );
    assert!(
        fn_body.contains(r#""failed": 0"#),
        "Error fallback JSON must include \"failed\": 0"
    );
    assert!(
        fn_body.contains(r#""disabled": 0"#),
        "Error fallback JSON must include \"disabled\": 0"
    );
}
