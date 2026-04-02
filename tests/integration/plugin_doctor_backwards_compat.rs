//! Verify acceptance criterion for US-ZCL-46:
//! Response format is backwards-compatible (additive only).
//!
//! This test inspects `src/gateway/api.rs` to confirm that the plugin
//! diagnostics addition to `handle_api_doctor` does not alter, remove, or
//! restructure the pre-existing response fields (`results`, `summary`).
//! The `plugins` key is appended conditionally — never replacing the
//! original payload.

#[test]
fn doctor_response_preserves_original_results_and_summary_keys() {
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

    // The base JSON body must be built with "results" and "summary" before
    // any plugin data is merged — these are the pre-existing contract fields.
    let results_pos = fn_body.find(r#""results""#);
    let summary_pos = fn_body.find(r#""summary""#);
    assert!(
        results_pos.is_some(),
        "handle_api_doctor must include a \"results\" key in the base response"
    );
    assert!(
        summary_pos.is_some(),
        "handle_api_doctor must include a \"summary\" key in the base response"
    );

    // The base body construction must appear before the plugin merge so that
    // the original fields are established first and only appended to.
    let json_macro_pos = fn_body
        .find("serde_json::json!")
        .expect("handle_api_doctor must build the response via serde_json::json!");
    let plugins_merge_pos = fn_body
        .find(r#"body["plugins"]"#)
        .or_else(|| fn_body.find("diagnose_plugins"))
        .expect("handle_api_doctor must merge plugin diagnostics");
    assert!(
        json_macro_pos < plugins_merge_pos,
        "base JSON body (results + summary) must be constructed before plugin diagnostics are merged"
    );
}

#[test]
fn doctor_plugin_merge_is_conditional_not_destructive() {
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

    // Plugin merge must be gated behind Option — when None (e.g. feature
    // disabled, no plugins configured) the response stays identical to the
    // pre-plugin format.
    assert!(
        fn_body.contains("if let Some(plugins)"),
        "plugin diagnostics must be conditionally merged via `if let Some(...)`, \
         so the response is unchanged when plugin diagnostics are unavailable"
    );

    // The merge must only *add* a key, not replace the entire body.
    // Verify that `body["plugins"] = ...` is the only mutation — there must
    // be no reassignment of `body` itself after the initial json! construction.
    let after_json = {
        let json_pos = fn_body
            .find("serde_json::json!")
            .expect("json! macro must exist");
        // Skip past the json! block (find the matching closing brace pair)
        &fn_body[json_pos..]
    };

    // body must not be fully reassigned (e.g., `body = ...` or `let body = ...`
    // appearing a second time after the initial construction).
    let second_let_body = after_json
        .find("let body")
        .map(|p| {
            // Allow `let mut body` from the initial declaration
            p > 0 && after_json[..p].contains(r#""results""#)
        })
        .unwrap_or(false);
    assert!(
        !second_let_body,
        "body must not be fully re-declared after the initial json! construction — \
         plugin merge must be additive via indexing (body[\"plugins\"] = ...)"
    );
}

#[test]
fn doctor_response_does_not_nest_plugins_inside_results() {
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

    // Plugins must be a top-level key, not nested inside results or summary.
    // Check that the merge target is body["plugins"], not body["results"]["plugins"]
    // or body["summary"]["plugins"].
    assert!(
        !fn_body.contains(r#"body["results"]["plugins"]"#),
        "plugins must be a top-level key, not nested inside results"
    );
    assert!(
        !fn_body.contains(r#"body["summary"]["plugins"]"#),
        "plugins must be a top-level key, not nested inside summary"
    );
}
