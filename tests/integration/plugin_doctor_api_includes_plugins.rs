//! Verify acceptance criterion for US-ZCL-46:
//! POST /api/doctor response includes plugin diagnostic entries.
//!
//! This test inspects `src/gateway/api.rs` and `src/doctor/mod.rs` to confirm:
//! 1. `handle_api_doctor` calls `diagnose_plugins()` and merges the result into
//!    the JSON response under the `"plugins"` key.
//! 2. `diagnose_plugins()` returns structured JSON with loaded/failed/disabled
//!    counts and an entries array.

#[test]
fn handle_api_doctor_calls_diagnose_plugins() {
    use std::fs;
    use std::path::Path;

    let api_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gateway/api.rs");
    let source = fs::read_to_string(&api_rs).expect("failed to read src/gateway/api.rs");

    // Locate the handle_api_doctor function
    let fn_pos = source
        .find("fn handle_api_doctor(")
        .expect("handle_api_doctor function must exist in api.rs");
    let fn_body = &source[fn_pos..];
    let body_end = fn_body[1..]
        .find("\npub async fn ")
        .or_else(|| fn_body[1..].find("\npub fn "))
        .map(|p| p + 1)
        .unwrap_or(fn_body.len());
    let fn_body = &fn_body[..body_end];

    // 1. Must call diagnose_plugins to obtain plugin diagnostics
    assert!(
        fn_body.contains("diagnose_plugins"),
        "handle_api_doctor must call diagnose_plugins() to include plugin diagnostics"
    );

    // 2. Must insert the result under the "plugins" key in the response body
    assert!(
        fn_body.contains(r#"body["plugins"]"#) || fn_body.contains(r#""plugins""#),
        "handle_api_doctor must include a \"plugins\" key in the JSON response"
    );
}

#[test]
fn handle_api_doctor_merges_plugins_additively() {
    use std::fs;
    use std::path::Path;

    let api_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/gateway/api.rs");
    let source = fs::read_to_string(&api_rs).expect("failed to read src/gateway/api.rs");

    let fn_pos = source
        .find("fn handle_api_doctor(")
        .expect("handle_api_doctor function must exist in api.rs");
    let fn_body = &source[fn_pos..];
    let body_end = fn_body[1..]
        .find("\npub async fn ")
        .or_else(|| fn_body[1..].find("\npub fn "))
        .map(|p| p + 1)
        .unwrap_or(fn_body.len());
    let fn_body = &fn_body[..body_end];

    // The base response must still contain "results" and "summary"
    assert!(
        fn_body.contains(r#""results""#),
        "handle_api_doctor response must include \"results\" array"
    );
    assert!(
        fn_body.contains(r#""summary""#),
        "handle_api_doctor response must include \"summary\" object"
    );

    // Plugins are merged conditionally (additive only — no breaking change)
    assert!(
        fn_body.contains("if let Some(plugins)"),
        "plugin diagnostics must be merged conditionally via Option, \
         keeping the response backwards-compatible when the feature is disabled"
    );
}

#[test]
fn diagnose_plugins_returns_structured_json() {
    use std::fs;
    use std::path::Path;

    let doctor_mod = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor/mod.rs");
    let source = fs::read_to_string(&doctor_mod).expect("failed to read src/doctor/mod.rs");

    // diagnose_plugins must be public so the API layer can call it
    assert!(
        source.contains("pub fn diagnose_plugins("),
        "diagnose_plugins must be a public function callable from the gateway API"
    );

    // Must return Option<Value> — None when plugins feature disabled
    let fn_pos = source
        .find("pub fn diagnose_plugins(")
        .expect("diagnose_plugins must exist");
    let fn_sig_end = source[fn_pos..].find('{').unwrap();
    let fn_sig = &source[fn_pos..fn_pos + fn_sig_end];
    assert!(
        fn_sig.contains("Option<serde_json::Value>") || fn_sig.contains("Option<Value>"),
        "diagnose_plugins must return Option<serde_json::Value>"
    );

    // The inner implementation must produce structured JSON with required fields
    assert!(
        source.contains(r#""loaded""#) && source.contains(r#""failed""#) && source.contains(r#""disabled""#),
        "diagnose_plugins JSON must include loaded, failed, and disabled counts"
    );
    assert!(
        source.contains(r#""entries""#),
        "diagnose_plugins JSON must include an entries array"
    );
}
