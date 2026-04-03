//! Verify acceptance criterion for US-ZCL-45:
//! `zeroclaw doctor` output includes a **Plugin Health** section.
//!
//! This test inspects `src/doctor/mod.rs` to confirm:
//! 1. `check_plugin_health` is called from the main `diagnose()` function.
//! 2. The category `"plugins"` maps to the display name `"Plugin Health"`.
//! 3. The `run()` function renders section headers using `category_display_name`,
//!    so `[Plugin Health]` appears in doctor output.

#[test]
fn doctor_diagnose_calls_check_plugin_health() {
    use std::fs;
    use std::path::Path;

    let doctor_mod = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor/mod.rs");
    let source = fs::read_to_string(&doctor_mod).expect("failed to read src/doctor/mod.rs");

    // 1. diagnose() must call check_plugin_health
    let diagnose_pos = source
        .find("pub fn diagnose(")
        .expect("doctor module must have a public diagnose() function");
    let diagnose_body = &source[diagnose_pos..];
    // Grab until the next top-level `pub fn` or end of file
    let body_end = diagnose_body[1..]
        .find("\npub fn ")
        .map(|p| p + 1)
        .unwrap_or(diagnose_body.len());
    let diagnose_body = &diagnose_body[..body_end];

    assert!(
        diagnose_body.contains("check_plugin_health"),
        "diagnose() must call check_plugin_health so plugins appear in doctor output"
    );
}

#[test]
fn doctor_category_maps_plugins_to_plugin_health() {
    use std::fs;
    use std::path::Path;

    let doctor_mod = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor/mod.rs");
    let source = fs::read_to_string(&doctor_mod).expect("failed to read src/doctor/mod.rs");

    // 2. category_display_name maps "plugins" → "Plugin Health"
    assert!(
        source.contains(r#""plugins" => "Plugin Health""#),
        "category_display_name must map \"plugins\" to \"Plugin Health\""
    );
}

#[test]
fn doctor_run_renders_section_headers_via_category_display_name() {
    use std::fs;
    use std::path::Path;

    let doctor_mod = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor/mod.rs");
    let source = fs::read_to_string(&doctor_mod).expect("failed to read src/doctor/mod.rs");

    // 3. run() uses category_display_name to render section headers
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

    assert!(
        run_body.contains("category_display_name"),
        "run() must use category_display_name to render human-readable section headers"
    );

    // The rendered format should be [SectionName]
    assert!(
        run_body.contains("[{display}]") || run_body.contains("[{}]"),
        "run() must render section headers in [SectionName] format"
    );
}

#[test]
fn check_plugin_health_uses_plugins_category() {
    use std::fs;
    use std::path::Path;

    let doctor_mod = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/doctor/mod.rs");
    let source = fs::read_to_string(&doctor_mod).expect("failed to read src/doctor/mod.rs");

    // 4. check_plugin_health uses "plugins" as its category string
    let fn_pos = source
        .find("fn check_plugin_health(")
        .expect("check_plugin_health function must exist");
    let fn_body = &source[fn_pos..];
    let body_end = fn_body[1..]
        .find("\nfn ")
        .map(|p| p + 1)
        .unwrap_or(fn_body.len());
    let fn_body = &fn_body[..body_end];

    assert!(
        fn_body.contains(r#"let cat = "plugins""#) || fn_body.contains(r#""plugins""#),
        "check_plugin_health must use \"plugins\" as the category, \
         which maps to \"Plugin Health\" in the rendered output"
    );
}
