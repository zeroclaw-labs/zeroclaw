use std::process::Command;

fn zeroclaw_bin() -> String {
    std::env::var("CARGO_BIN_EXE_zeroclaw").unwrap_or_else(|_| "target/debug/zeroclaw".to_string())
}

fn write_tool_plugin(plugin_dir: &std::path::Path, name: &str) {
    std::fs::create_dir_all(plugin_dir).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        format!(
            "name = \"{name}\"\nversion = \"0.1.0\"\ndescription = \"CLI plugin\"\nwasm_path = \"plugin.wasm\"\ncapabilities = [\"tool\"]\npermissions = []\n"
        ),
    )
    .unwrap();
    std::fs::write(plugin_dir.join("plugin.wasm"), b"not-real-wasm").unwrap();
}

#[test]
#[cfg(feature = "plugins-wasm")]
fn plugin_install_and_list_use_configured_plugins_dir() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config_dir = tmp.path().join("config");
    let configured_plugins_dir = tmp.path().join("configured-plugins");
    let source_plugin = tmp.path().join("source-plugin");
    write_tool_plugin(&source_plugin, "cli-installed");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        format!(
            "[plugins]\nenabled = true\nplugins_dir = \"{}\"\n",
            configured_plugins_dir.display()
        ),
    )
    .unwrap();

    let install = Command::new(zeroclaw_bin())
        .arg("--config-dir")
        .arg(&config_dir)
        .arg("plugin")
        .arg("install")
        .arg(&source_plugin)
        .output()
        .expect("run plugin install");
    assert!(
        install.status.success(),
        "install failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr)
    );
    assert!(
        configured_plugins_dir
            .join("cli-installed")
            .join("manifest.toml")
            .is_file(),
        "plugin install should write to configured plugins.plugins_dir"
    );

    let list = Command::new(zeroclaw_bin())
        .arg("--config-dir")
        .arg(&config_dir)
        .arg("plugin")
        .arg("list")
        .output()
        .expect("run plugin list");
    assert!(
        list.status.success(),
        "list failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&list.stdout),
        String::from_utf8_lossy(&list.stderr)
    );
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert!(
        stdout.contains("cli-installed"),
        "plugin list should read from configured plugins.plugins_dir, got:\n{stdout}"
    );
}
