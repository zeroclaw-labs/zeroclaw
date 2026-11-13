//! Windows regression through the actual plugin-list CLI consumer.
#![cfg(windows)]

use std::process::Command;
use zeroclaw::plugins::registry::{
    PluginRegistryEntry, PluginRegistryIndex, write_cached_registry_index,
};

#[test]
fn windows_plugin_list_reports_unavailable_recipe_without_hiding_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::write(
        dir.path().join("config.toml"),
        "schema_version = 3\nlocale = \"en\"\n",
    )
    .unwrap();
    let origin = "https://registry.invalid/index?team=100%20one&channel=stable";
    let index = PluginRegistryIndex {
        registry_url: Some(origin.into()),
        plugins: vec![PluginRegistryEntry {
            name: "calendar".into(),
            version: "1%PATH% & echo ZC_REGISTRY_HARMLESS_MARKER_12216".into(),
            description: Some("calendar metadata remains available".into()),
            author: None,
            capabilities: vec![],
            url: "https://download.invalid/p.zip".into(),
            sha256: None,
        }],
    };
    write_cached_registry_index(&data, origin, &index).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_zeroclaw"))
        .env("ZEROCLAW_CONFIG_DIR", dir.path())
        .env_remove("ZEROCLAW_PLUGIN_REGISTRY_URL")
        .env("LANG", "en")
        .env("LC_ALL", "C")
        .args(["plugin", "list"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{output:?}");
    assert!(
        text.contains("calendar metadata remains available"),
        "{text}"
    );
    assert!(
        text.contains("copy-ready install command is unavailable"),
        "{text}"
    );
    assert!(!text.contains("zeroclaw plugin install"), "{text}");
    assert!(!text.contains("# PowerShell only"), "{text}");
    // The catalog version is still displayed as metadata, not an install hint.
    let hint = text
        .lines()
        .find(|line| line.contains("copy-ready"))
        .unwrap();
    assert!(!hint.contains("ZC_REGISTRY_HARMLESS_MARKER_12216"));
    // Pasting only actual command recipes means there is nothing to execute.
    // Probe each required native shell with fixed benign text, not metadata.
    for shell in ["cmd.exe", "powershell.exe"] {
        let mut command = Command::new(shell);
        if shell == "cmd.exe" {
            command.args(["/D", "/V:ON", "/S", "/C", "echo ZC_REGISTRY_NO_RECIPE"]);
        } else {
            command.args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "echo ZC_REGISTRY_NO_RECIPE",
            ]);
        }
        let result = command
            .current_dir(dir.path())
            .output()
            .expect("native shell required");
        assert!(result.status.success(), "{result:?}");
        let stdout = String::from_utf8_lossy(&result.stdout);
        assert!(stdout.contains("ZC_REGISTRY_NO_RECIPE"));
        assert!(!stdout.contains("ZC_REGISTRY_HARMLESS_MARKER_12216"));
    }
}
