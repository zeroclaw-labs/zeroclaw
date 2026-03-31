//! Verify acceptance criterion for US-ZCL-47:
//! Hint text references the exact command: `zeroclaw plugin doctor`.
//!
//! This test inspects `src/doctor/mod.rs` to confirm the hint string
//! contains the precise CLI invocation a user would type.

#[test]
fn doctor_hint_references_exact_command() {
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

    // The hint must contain the exact command `zeroclaw plugin doctor`
    let exact_command = "zeroclaw plugin doctor";
    assert!(
        run_body.contains(exact_command),
        "run() must contain the exact command `{exact_command}` in the hint text"
    );

    // Verify the hint is a complete, user-actionable instruction
    // (not just a partial match — the backtick-quoted command must be present)
    let backtick_command = format!("`{exact_command}`");
    assert!(
        run_body.contains(&backtick_command),
        "the hint must wrap the command in backticks for clarity: {backtick_command}"
    );

    // Verify the hint uses "Run" as the verb — it should be an actionable directive
    let run_prefix = format!("Run {backtick_command}");
    assert!(
        run_body.contains(&run_prefix),
        "the hint must be an actionable directive starting with \"Run {backtick_command}\""
    );
}
