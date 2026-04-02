//! Verify that the zeroclaw-plugin-sdk crate exists with Cargo.toml and typed modules.
//!
//! Acceptance criterion for US-ZCL-27:
//! > zeroclaw-plugin-sdk crate created with Cargo.toml and typed modules

use std::path::Path;

const SDK_DIR: &str = "crates/zeroclaw-plugin-sdk";

#[test]
fn sdk_crate_has_cargo_toml() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(SDK_DIR);
    assert!(
        base.is_dir(),
        "zeroclaw-plugin-sdk crate directory does not exist at {}",
        base.display()
    );

    let cargo_toml = base.join("Cargo.toml");
    assert!(
        cargo_toml.is_file(),
        "zeroclaw-plugin-sdk/Cargo.toml is missing"
    );
}

#[test]
fn sdk_crate_has_lib_rs() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(SDK_DIR);
    let lib_rs = base.join("src/lib.rs");
    assert!(
        lib_rs.is_file(),
        "zeroclaw-plugin-sdk/src/lib.rs is missing"
    );
}

#[test]
fn sdk_crate_has_memory_module() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(SDK_DIR);
    let memory = base.join("src/memory.rs");
    assert!(
        memory.is_file(),
        "zeroclaw-plugin-sdk/src/memory.rs module is missing"
    );
}

#[test]
fn sdk_crate_has_tools_module() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(SDK_DIR);
    let tools = base.join("src/tools.rs");
    assert!(
        tools.is_file(),
        "zeroclaw-plugin-sdk/src/tools.rs module is missing"
    );
}

#[test]
fn sdk_crate_has_messaging_module() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(SDK_DIR);
    let messaging = base.join("src/messaging.rs");
    assert!(
        messaging.is_file(),
        "zeroclaw-plugin-sdk/src/messaging.rs module is missing"
    );
}

#[test]
fn sdk_crate_has_context_module() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(SDK_DIR);
    let context = base.join("src/context.rs");
    assert!(
        context.is_file(),
        "zeroclaw-plugin-sdk/src/context.rs module is missing"
    );
}
