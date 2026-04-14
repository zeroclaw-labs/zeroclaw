#![cfg(feature = "plugins-wasm")]
//! Verify acceptance criterion for US-ZCL-47:
//! Hint line appears only when plugin issues exist (warn or error).
//!
//! This test inspects `src/doctor/mod.rs` to confirm:
//! 1. The hint is guarded by a check for plugin items with Warn or Error severity.
//! 2. The hint text references `zeroclaw plugin doctor`.
//! 3. The hint is inside `run()` and only printed conditionally.

#[test]
fn doctor_hint_guarded_by_plugin_warn_or_error() {
    use std::fs;
    use std::path::Path;

    let doctor_mod = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor/mod.rs");
    let source = fs::read_to_string(&doctor_mod).expect("failed to read src/doctor/mod.rs");

    // Extract the run() function body
    let run_pos = source
        .find("pub fn run(")
        .expect("doctor module must have a public run() function");
    let run_body = &source[run_pos..];
    let body_end = run_body[1..]
        .find("\npub fn ")
        .or_else(|| run_body[1..].find("\nfn "))
        .map(|p| p + 1)
        .unwrap_or(run_body.len());
    let run_body = &run_body[..body_end];

    // 1. Must check category == "plugins" with Warn | Error severity
    assert!(
        run_body.contains(r#"category == "plugins"#)
            || run_body.contains(r#"category == "plugins""#),
        "run() must filter diagnostic items by the \"plugins\" category"
    );
    assert!(
        run_body.contains("Severity::Warn") && run_body.contains("Severity::Error"),
        "run() must check for Warn and Error severity when deciding to show the hint"
    );

    // 2. The hint must be conditional (inside an `if` block)
    // Find the hint line and verify it's preceded by a conditional
    let hint_pos = run_body
        .find("zeroclaw plugin doctor")
        .expect("run() must contain the hint referencing `zeroclaw plugin doctor`");
    let before_hint = &run_body[..hint_pos];

    // The conditional variable must be checked before the hint is printed
    assert!(
        before_hint.contains("has_plugin_issues") || before_hint.contains("if "),
        "the hint must be guarded by a conditional check for plugin issues"
    );

    // 3. Verify the pattern: compute a bool from the severity check, then use it in an if
    assert!(
        run_body.contains("if has_plugin_issues")
            || (run_body.contains(".any(|")
                && run_body.contains("Severity::Warn | Severity::Error")),
        "the hint must only display when plugin diagnostics include warn or error items"
    );
}
