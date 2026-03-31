//! Verify that an example plugin using the SDK compiles and works end-to-end.
//!
//! Acceptance criterion for US-ZCL-27:
//! > Example plugin using SDK compiles and works end-to-end

use std::path::Path;

const SDK_DIR: &str = "crates/zeroclaw-plugin-sdk";
const EXAMPLE_DIR: &str = "tests/plugins/sdk-example-plugin";

// ---------------------------------------------------------------------------
// Example plugin crate exists
// ---------------------------------------------------------------------------

#[test]
fn sdk_example_plugin_directory_exists() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(EXAMPLE_DIR);
    assert!(
        base.is_dir(),
        "SDK example plugin directory does not exist at {}",
        base.display()
    );
}

#[test]
fn sdk_example_plugin_has_cargo_toml() {
    let cargo_toml = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(EXAMPLE_DIR)
        .join("Cargo.toml");
    assert!(
        cargo_toml.is_file(),
        "SDK example plugin is missing Cargo.toml"
    );
}

#[test]
fn sdk_example_plugin_has_source() {
    let lib_rs = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(EXAMPLE_DIR)
        .join("src/lib.rs");
    assert!(lib_rs.is_file(), "SDK example plugin is missing src/lib.rs");
}

// ---------------------------------------------------------------------------
// Cargo.toml depends on the SDK crate
// ---------------------------------------------------------------------------

fn example_cargo_toml() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(EXAMPLE_DIR)
        .join("Cargo.toml");
    std::fs::read_to_string(&path).expect("failed to read sdk-example-plugin/Cargo.toml")
}

#[test]
fn sdk_example_depends_on_plugin_sdk() {
    let toml = example_cargo_toml();
    assert!(
        toml.contains("zeroclaw-plugin-sdk"),
        "SDK example plugin Cargo.toml must depend on zeroclaw-plugin-sdk"
    );
}

#[test]
fn sdk_example_depends_on_extism_pdk() {
    let toml = example_cargo_toml();
    assert!(
        toml.contains("extism-pdk"),
        "SDK example plugin Cargo.toml must depend on extism-pdk for WASM entry points"
    );
}

#[test]
fn sdk_example_is_cdylib() {
    let toml = example_cargo_toml();
    assert!(
        toml.contains("cdylib"),
        "SDK example plugin must be a cdylib crate (required for WASM plugins)"
    );
}

// ---------------------------------------------------------------------------
// Source code uses the SDK modules
// ---------------------------------------------------------------------------

fn example_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(EXAMPLE_DIR)
        .join("src/lib.rs");
    std::fs::read_to_string(&path).expect("failed to read sdk-example-plugin/src/lib.rs")
}

#[test]
fn sdk_example_imports_sdk() {
    let src = example_source();
    assert!(
        src.contains("zeroclaw_plugin_sdk") || src.contains("zeroclaw_plugin_sdk::"),
        "SDK example plugin must import zeroclaw_plugin_sdk"
    );
}

#[test]
fn sdk_example_has_plugin_fn() {
    let src = example_source();
    assert!(
        src.contains("#[plugin_fn]") || src.contains("plugin_fn"),
        "SDK example plugin must define at least one #[plugin_fn] entry point"
    );
}

#[test]
fn sdk_example_uses_at_least_two_sdk_modules() {
    let src = example_source();
    let mut modules_used = 0;
    if src.contains("memory") {
        modules_used += 1;
    }
    if src.contains("tool_call") || src.contains("tools::") {
        modules_used += 1;
    }
    if src.contains("send_message") || src.contains("get_channels") || src.contains("messaging::") {
        modules_used += 1;
    }
    if src.contains("session")
        || src.contains("user_identity")
        || src.contains("agent_config")
        || src.contains("context::")
    {
        modules_used += 1;
    }
    assert!(
        modules_used >= 2,
        "SDK example plugin should demonstrate at least 2 SDK modules, found {}",
        modules_used
    );
}

// ---------------------------------------------------------------------------
// The example is registered in the test-plugins workspace
// ---------------------------------------------------------------------------

#[test]
fn sdk_example_in_test_plugins_workspace() {
    let ws_toml = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/plugins/Cargo.toml");
    let toml = std::fs::read_to_string(&ws_toml).expect("failed to read tests/plugins/Cargo.toml");
    assert!(
        toml.contains("sdk-example-plugin"),
        "sdk-example-plugin must be a member of the tests/plugins workspace"
    );
}

// ---------------------------------------------------------------------------
// Compiled WASM artifact exists (proves end-to-end compilation)
// ---------------------------------------------------------------------------

#[test]
fn sdk_example_wasm_artifact_exists() {
    let artifact = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/artifacts/sdk_example_plugin.wasm");
    assert!(
        artifact.is_file(),
        "Pre-compiled sdk_example_plugin.wasm artifact is missing — \
         the example plugin must compile to WASM end-to-end. \
         Build with: cargo build --manifest-path tests/plugins/Cargo.toml \
         --target wasm32-wasip1 --release"
    );
}

#[test]
fn sdk_example_wasm_artifact_is_nontrivial() {
    let artifact = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/artifacts/sdk_example_plugin.wasm");
    if artifact.is_file() {
        let metadata = std::fs::metadata(&artifact).expect("failed to stat wasm artifact");
        assert!(
            metadata.len() > 1024,
            "sdk_example_plugin.wasm is suspiciously small ({} bytes) — \
             a real WASM plugin should be at least a few KB",
            metadata.len()
        );
    }
}

// ---------------------------------------------------------------------------
// The SDK crate itself exists (prerequisite sanity check)
// ---------------------------------------------------------------------------

#[test]
fn sdk_crate_exists_for_example() {
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join(SDK_DIR);
    assert!(
        sdk.is_dir(),
        "zeroclaw-plugin-sdk crate must exist at {} for the example to depend on it",
        sdk.display()
    );
}
