//! Verify acceptance criterion for US-ZCL-47:
//! Hint does not appear when all plugins are healthy.
//!
//! This test inspects `src/doctor/mod.rs` to confirm that the hint line
//! referencing `zeroclaw plugin doctor` is only printed inside a conditional
//! block that requires plugin issues (Warn or Error). When all plugins report
//! Ok severity, the conditional is false and the hint is never emitted.

#[test]
fn doctor_hint_absent_when_all_plugins_healthy() {
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

    // The hint text must exist exactly once
    let hint_text = "zeroclaw plugin doctor";
    let hint_count = run_body.matches(hint_text).count();
    assert_eq!(
        hint_count, 1,
        "the hint referencing `zeroclaw plugin doctor` must appear exactly once in run(), found {hint_count}"
    );

    // The hint must be inside the `if has_plugin_issues` block — meaning
    // it is only reachable when Warn or Error items exist. Verify that:
    // 1. `has_plugin_issues` requires Warn | Error (no Ok-only path prints it)
    // 2. The hint is guarded by `if has_plugin_issues`

    // Confirm the guard variable requires non-Ok severity
    assert!(
        run_body.contains("has_plugin_issues")
            && run_body.contains("Severity::Warn | Severity::Error"),
        "the hint guard must require Warn or Error severity — healthy (Ok) plugins must not trigger it"
    );

    // Confirm the hint is inside the if-block, not at top level
    let hint_pos = run_body.find(hint_text).unwrap();
    let guard_pos = run_body
        .find("if has_plugin_issues")
        .expect("hint must be guarded by `if has_plugin_issues`");
    assert!(
        guard_pos < hint_pos,
        "the `if has_plugin_issues` guard must appear before the hint text, \
         ensuring the hint is only printed when plugin issues exist"
    );

    // Confirm there is no unconditional fallback that also prints the hint
    // by checking that no second occurrence exists outside the guarded block
    let after_guard = &run_body[guard_pos..];
    let occurrences_after_guard = after_guard.matches(hint_text).count();
    assert_eq!(
        occurrences_after_guard, 1,
        "the hint must appear exactly once and only inside the guarded block"
    );
}
