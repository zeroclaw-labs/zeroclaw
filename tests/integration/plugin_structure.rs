//! Verify that each test plugin crate contains the required files:
//! Cargo.toml, src/lib.rs, and plugin.toml.

use std::path::Path;

const PLUGIN_DIR: &str = "tests/plugins";
const REQUIRED_PLUGINS: &[&str] = &["echo-plugin", "multi-tool-plugin", "bad-actor-plugin"];

#[test]
fn each_plugin_has_cargo_toml_and_lib_rs_and_plugin_toml() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(PLUGIN_DIR);
    assert!(
        base.is_dir(),
        "tests/plugins/ directory does not exist at {}",
        base.display()
    );

    for plugin in REQUIRED_PLUGINS {
        let plugin_dir = base.join(plugin);
        assert!(
            plugin_dir.is_dir(),
            "{plugin}/ directory does not exist under tests/plugins/"
        );

        let cargo_toml = plugin_dir.join("Cargo.toml");
        assert!(cargo_toml.is_file(), "{plugin}/Cargo.toml is missing");

        let lib_rs = plugin_dir.join("src/lib.rs");
        assert!(lib_rs.is_file(), "{plugin}/src/lib.rs is missing");

        let plugin_toml = plugin_dir.join("plugin.toml");
        assert!(plugin_toml.is_file(), "{plugin}/plugin.toml is missing");
    }
}
