#![cfg(feature = "plugins-wasm")]
//! Verify acceptance criterion for US-ZCL-46:
//! Individual failure entries include plugin name and reason.
//!
//! This test inspects `src/doctor/mod.rs` to confirm:
//! 1. Each failure/warn entry includes a `plugin_name` field.
//! 2. Each entry includes a `message` field combining plugin name and reasons.
//! 3. Failure reasons are extracted from individual `DiagCheck` messages.

#[test]
fn failure_entry_includes_plugin_name_field() {
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

    // Each individual entry must include a "plugin_name" key
    assert!(
        fn_body.contains(r#""plugin_name""#),
        "failure entries must include a \"plugin_name\" field identifying the plugin"
    );

    // The plugin_name value must come from the diagnostic struct, not be hard-coded
    assert!(
        fn_body.contains("diag.plugin_name"),
        "plugin_name field must be sourced from diag.plugin_name (the actual plugin identifier)"
    );
}

#[test]
fn failure_entry_message_combines_name_and_reasons() {
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

    // The message field must combine the plugin name with the failure reasons
    // Expected format: "{plugin_name}: {reason1}; {reason2}"
    assert!(
        fn_body.contains(r#""message""#),
        "failure entries must include a \"message\" field"
    );

    // The message must be formatted with plugin name prefix and joined reasons
    assert!(
        fn_body.contains("diag.plugin_name, reasons.join"),
        "message must be formatted as \"{{plugin_name}}: {{reasons joined by separator}}\""
    );
}

#[test]
fn failure_reasons_extracted_from_diag_checks() {
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

    // Reasons must be extracted from individual check messages
    assert!(
        fn_body.contains("c.message"),
        "failure reasons must be extracted from DiagCheck message fields"
    );

    // Must filter checks by status to only include failed/warned checks
    assert!(
        fn_body.contains("c.status == DiagStatus::Fail")
            || fn_body.contains("c.status == DiagStatus::Warn"),
        "reasons must be filtered by check status (Fail or Warn)"
    );
}

#[test]
fn failure_and_warn_entries_both_handled() {
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

    // Both Fail and Warn branches must produce entries with plugin_name and message
    assert!(
        fn_body.contains("DiagStatus::Fail") && fn_body.contains("DiagStatus::Warn"),
        "diagnose_plugins_inner must handle both Fail and Warn diagnostic statuses"
    );

    // Fail entries use "error" severity, Warn entries use "warn" severity
    assert!(
        fn_body.contains(r#""severity": "error""#),
        "Fail entries must use severity: \"error\""
    );
    assert!(
        fn_body.contains(r#""severity": "warn""#),
        "Warn entries must use severity: \"warn\""
    );

    // Pass entries must be skipped (no entry generated)
    assert!(
        fn_body.contains("DiagStatus::Pass => {}") || fn_body.contains("DiagStatus::Pass => {"),
        "Pass entries must be explicitly skipped (no output generated)"
    );
}
