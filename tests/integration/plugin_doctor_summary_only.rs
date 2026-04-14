#![cfg(feature = "plugins-wasm")]
//! Verify acceptance criterion for US-ZCL-45:
//!
//! > Does not duplicate full plugin doctor output — summary only
//!
//! The main doctor's `check_plugin_health` function must produce summary-level
//! diagnostics (counts + per-plugin error/warn one-liners) without repeating
//! the individual per-check detail that `PluginHost::doctor()` / `diagnose_plugin()`
//! returns (e.g. "manifest.toml is valid", "WASM file exists and is readable").

/// `check_plugin_health` must NOT emit individual DiagCheck names
/// (manifest, wasm_file, config, allowed_hosts, allowed_paths) as separate
/// DiagItem entries — it should aggregate them into one error/warn per plugin.
#[test]
fn doctor_plugin_health_does_not_emit_per_check_items() {
    use std::fs;
    use std::path::Path;

    let doctor_mod = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor/mod.rs");
    let source = fs::read_to_string(&doctor_mod).expect("failed to read src/doctor/mod.rs");

    // Locate the check_plugin_health function body (the #[cfg(feature = "plugins-wasm")] one)
    let fn_pos = source
        .find("fn check_plugin_health(config: &Config")
        .expect("check_plugin_health(config: &Config, ...) must exist");
    let fn_body = &source[fn_pos..];
    let body_end = fn_body[1..]
        .find("\nfn ")
        .map(|p| p + 1)
        .unwrap_or(fn_body.len());
    let fn_body = &fn_body[..body_end];

    // The function should NOT push items with individual check names as messages.
    // Individual DiagCheck names from diagnose_plugin are: "manifest", "wasm_file",
    // "config", "allowed_hosts", "allowed_paths".
    // If check_plugin_health were duplicating the full output, it would push
    // separate DiagItems for each of these check names.
    //
    // What it SHOULD do: iterate over diagnostics, filter by overall() status,
    // and emit one aggregated line per plugin (name + joined reasons).

    // Verify the function does NOT call diagnose_plugin directly — it uses
    // host.doctor() which returns already-aggregated PluginDiagnostic structs,
    // and then further summarises them.
    assert!(
        !fn_body.contains("diagnose_plugin("),
        "check_plugin_health must not call diagnose_plugin directly; \
         it should use host.doctor() which returns per-plugin summaries"
    );

    // Verify the function aggregates by iterating over diagnostics and checking
    // overall() status, not by pushing individual check-level items.
    assert!(
        fn_body.contains(".overall()"),
        "check_plugin_health must use diag.overall() to determine per-plugin \
         status rather than inspecting individual checks"
    );

    // Verify it produces at most one error/warn DiagItem per plugin by joining
    // the failure reasons, not by pushing one DiagItem per DiagCheck.
    assert!(
        fn_body.contains(".join(") || fn_body.contains("reasons.join"),
        "check_plugin_health must join failure reasons into a single message \
         per plugin rather than pushing separate items per check"
    );

    // Verify summary count line uses aggregated totals, not individual check results.
    assert!(
        fn_body.contains("total") && fn_body.contains("loaded") && fn_body.contains("failed"),
        "check_plugin_health must emit a summary count line with total/loaded/failed"
    );
}

/// The summary must NOT contain verbose check-level messages that appear
/// in the full `diagnose_plugin` output (e.g. "manifest.toml is valid").
#[test]
fn doctor_plugin_health_does_not_contain_verbose_check_messages() {
    use std::fs;
    use std::path::Path;

    let doctor_mod = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor/mod.rs");
    let source = fs::read_to_string(&doctor_mod).expect("failed to read src/doctor/mod.rs");

    let fn_pos = source
        .find("fn check_plugin_health(config: &Config")
        .expect("check_plugin_health must exist");
    let fn_body = &source[fn_pos..];
    let body_end = fn_body[1..]
        .find("\nfn ")
        .map(|p| p + 1)
        .unwrap_or(fn_body.len());
    let fn_body = &fn_body[..body_end];

    // These are verbose messages from diagnose_plugin's individual checks.
    // They must NOT appear as literal strings in check_plugin_health.
    let verbose_messages = [
        "manifest.toml is valid",
        "plugin.toml is valid",
        "WASM file exists and is readable",
        "no required config keys",
    ];

    for msg in &verbose_messages {
        assert!(
            !fn_body.contains(msg),
            "check_plugin_health must not contain verbose check message '{}' — \
             summary only, not full doctor output",
            msg
        );
    }
}

/// For passing plugins, check_plugin_health must NOT emit any per-plugin
/// DiagItem — only the summary count line. This confirms it suppresses
/// detail for healthy plugins rather than listing each passing check.
#[test]
fn doctor_plugin_health_skips_passing_plugins_detail() {
    use std::fs;
    use std::path::Path;

    let doctor_mod = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor/mod.rs");
    let source = fs::read_to_string(&doctor_mod).expect("failed to read src/doctor/mod.rs");

    let fn_pos = source
        .find("fn check_plugin_health(config: &Config")
        .expect("check_plugin_health must exist");
    let fn_body = &source[fn_pos..];
    let body_end = fn_body[1..]
        .find("\nfn ")
        .map(|p| p + 1)
        .unwrap_or(fn_body.len());
    let fn_body = &fn_body[..body_end];

    // The match on overall() must have a Pass arm that does nothing (no push).
    // This confirms passing plugins are silently counted, not individually listed.
    assert!(
        fn_body.contains("DiagStatus::Pass => {}") || fn_body.contains("DiagStatus::Pass => {\n"),
        "check_plugin_health must have a Pass arm that emits nothing — \
         passing plugins should only appear in the summary count, not individually"
    );
}
