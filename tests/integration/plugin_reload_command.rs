//! Verify that `zeroclaw plugin reload` command exists and is feature-gated
//! behind `plugins-wasm`.

use std::fs;
use std::path::Path;

/// The reload variant must exist in PluginCommands and the entire enum
/// must be gated behind `#[cfg(feature = "plugins-wasm")]`.
#[test]
fn plugin_reload_command_exists_and_is_feature_gated() {
    let main_rs = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs");
    let source = fs::read_to_string(&main_rs).expect("failed to read src/main.rs");

    // 1. The Reload variant exists in PluginCommands
    assert!(
        source.contains("Reload,") || source.contains("Reload {"),
        "PluginCommands should contain a Reload variant"
    );

    // 2. PluginCommands enum is feature-gated behind plugins-wasm
    let enum_pos = source
        .find("enum PluginCommands")
        .expect("PluginCommands enum should exist in main.rs");

    // Look backwards from the enum definition for the cfg attribute
    let before_enum = &source[..enum_pos];
    let last_cfg = before_enum.rfind("#[cfg(feature = \"plugins-wasm\")]");
    assert!(
        last_cfg.is_some(),
        "PluginCommands enum should be gated by #[cfg(feature = \"plugins-wasm\")]"
    );

    // Ensure the cfg attribute is close to the enum (within a few lines, no other items between)
    let between = &source[last_cfg.unwrap()..enum_pos];
    let line_count = between.lines().count();
    assert!(
        line_count <= 3,
        "Feature gate should be immediately before PluginCommands (found {} lines between)",
        line_count
    );
}
