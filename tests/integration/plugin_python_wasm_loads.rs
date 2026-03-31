//! Verify that Python plugin .wasm binaries load successfully in PluginHost —
//! acceptance criteria for US-ZCL-34 and US-ZCL-35.

use std::path::Path;
use tempfile::tempdir;
use zeroclaw::plugins::host::PluginHost;

/// The pre-compiled python_echo_plugin.wasm must load in PluginHost without
/// any host-side changes (same manifest format as Rust plugins).
#[test]
fn python_echo_wasm_loads_in_plugin_host() {
    let artifact = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/artifacts/python_echo_plugin.wasm");
    let manifest =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/plugins/python-echo-plugin/plugin.toml");

    assert!(artifact.exists(), "python_echo_plugin.wasm must exist");
    assert!(manifest.exists(), "plugin.toml must exist");

    // Set up a temp workspace with plugins/<name>/ containing the manifest and wasm.
    let workspace = tempdir().unwrap();
    let plugin_dir = workspace.path().join("plugins").join("python-echo-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();

    std::fs::copy(&manifest, plugin_dir.join("plugin.toml")).unwrap();
    std::fs::copy(&artifact, plugin_dir.join("python_echo_plugin.wasm")).unwrap();

    // PluginHost::new discovers plugins under <workspace>/plugins/
    let host = PluginHost::new(workspace.path()).expect("PluginHost::new must succeed");

    // The plugin must be discoverable.
    let info = host
        .get_plugin("python-echo-plugin")
        .expect("python-echo-plugin must be discovered by PluginHost");

    assert_eq!(info.name, "python-echo-plugin");
    assert_eq!(info.version, "0.1.0");
    assert!(info.wasm_path.exists(), "WASM binary path must exist");
    assert!(info.loaded, "plugin loaded flag must be true");

    // load_plugin verifies WASM hash integrity.
    let loaded = host
        .load_plugin("python-echo-plugin")
        .expect("load_plugin must succeed for the Python echo WASM binary");

    assert_eq!(loaded.name, "python-echo-plugin");
    assert!(
        loaded.wasm_sha256.is_some(),
        "WASM SHA-256 hash must be recorded after load"
    );

    // Verify the tool from the manifest is present.
    assert_eq!(loaded.tools.len(), 1);
    assert_eq!(loaded.tools[0].name, "tool_echo");
}

/// PluginHost must report the correct metadata from the Python plugin manifest.
#[test]
fn python_echo_plugin_metadata_matches_manifest() {
    let artifact = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/artifacts/python_echo_plugin.wasm");
    let manifest =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/plugins/python-echo-plugin/plugin.toml");

    let workspace = tempdir().unwrap();
    let plugin_dir = workspace.path().join("plugins").join("python-echo-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();

    std::fs::copy(&manifest, plugin_dir.join("plugin.toml")).unwrap();
    std::fs::copy(&artifact, plugin_dir.join("python_echo_plugin.wasm")).unwrap();

    let host = PluginHost::new(workspace.path()).expect("PluginHost::new must succeed");
    let info = host
        .get_plugin("python-echo-plugin")
        .expect("plugin must be discovered");

    assert_eq!(
        info.description.as_deref(),
        Some("Echoes JSON input back as output — Python SDK version of echo-plugin for round-trip integration tests.")
    );
    assert!(!info.capabilities.is_empty(), "capabilities must be parsed");
    assert_eq!(info.tools.len(), 1, "manifest declares one tool");
    assert_eq!(info.tools[0].export, "tool_echo");
}

// ---------------------------------------------------------------------------
// python-sdk-example-plugin PluginHost loading (US-ZCL-35)
// ---------------------------------------------------------------------------

/// The pre-compiled python_sdk_example_plugin.wasm must load in PluginHost.
#[test]
fn python_sdk_example_wasm_loads_in_plugin_host() {
    let artifact = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/artifacts/python_sdk_example_plugin.wasm");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/python-sdk-example-plugin/plugin.toml");

    assert!(
        artifact.exists(),
        "python_sdk_example_plugin.wasm must exist"
    );
    assert!(manifest.exists(), "plugin.toml must exist");

    let workspace = tempdir().unwrap();
    let plugin_dir = workspace
        .path()
        .join("plugins")
        .join("python-sdk-example-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();

    std::fs::copy(&manifest, plugin_dir.join("plugin.toml")).unwrap();
    std::fs::copy(&artifact, plugin_dir.join("python_sdk_example_plugin.wasm")).unwrap();

    let host = PluginHost::new(workspace.path()).expect("PluginHost::new must succeed");

    let info = host
        .get_plugin("python-sdk-example-plugin")
        .expect("python-sdk-example-plugin must be discovered by PluginHost");

    assert_eq!(info.name, "python-sdk-example-plugin");
    assert_eq!(info.version, "0.1.0");
    assert!(info.wasm_path.exists(), "WASM binary path must exist");
    assert!(info.loaded, "plugin loaded flag must be true");

    let loaded = host
        .load_plugin("python-sdk-example-plugin")
        .expect("load_plugin must succeed for the Python SDK example WASM binary");

    assert_eq!(loaded.name, "python-sdk-example-plugin");
    assert!(
        loaded.wasm_sha256.is_some(),
        "WASM SHA-256 hash must be recorded after load"
    );

    assert_eq!(loaded.tools.len(), 1);
    assert_eq!(loaded.tools[0].name, "tool_greet");
}

/// PluginHost must report the correct metadata from the SDK example plugin manifest.
#[test]
fn python_sdk_example_plugin_metadata_matches_manifest() {
    let artifact = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/artifacts/python_sdk_example_plugin.wasm");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/python-sdk-example-plugin/plugin.toml");

    let workspace = tempdir().unwrap();
    let plugin_dir = workspace
        .path()
        .join("plugins")
        .join("python-sdk-example-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();

    std::fs::copy(&manifest, plugin_dir.join("plugin.toml")).unwrap();
    std::fs::copy(&artifact, plugin_dir.join("python_sdk_example_plugin.wasm")).unwrap();

    let host = PluginHost::new(workspace.path()).expect("PluginHost::new must succeed");
    let info = host
        .get_plugin("python-sdk-example-plugin")
        .expect("plugin must be discovered");

    assert_eq!(
        info.description.as_deref(),
        Some("Example plugin demonstrating all four SDK modules (context, memory, tools, messaging) — Python version of the Smart Greeter pattern.")
    );
    assert!(!info.capabilities.is_empty(), "capabilities must be parsed");
    assert_eq!(info.tools.len(), 1, "manifest declares one tool");
    assert_eq!(info.tools[0].export, "tool_greet");
}
