//! Spike: proves the SDK's raw `tool-plugin` guest bindings (sync exports,
//! generated via `wit_bindgen::generate!`) actually round-trip through the
//! real, unmodified host (`zeroclaw_plugins::PluginHost`, which wires the
//! same world via `wasmtime::component::bindgen!` with
//! `exports: { default: async }`). This is the strongest proof that the
//! two independently-pinned wit-parser toolchains (guest `wit-bindgen`,
//! host `wasmtime`) agree on the wire format for `wit/v0/tool.wit`.
//!
//! Skips (rather than fails) if the `wasm32-wasip2` target isn't installed,
//! since that's a local toolchain dependency, not a code correctness issue.

use std::path::Path;
use std::process::Command;

fn wasm32_wasip2_installed() -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line.trim() == "wasm32-wasip2")
        })
        .unwrap_or(false)
}

#[tokio::test]
async fn tool_echo_round_trips_through_plugin_host() {
    if !wasm32_wasip2_installed() {
        eprintln!("skipping: wasm32-wasip2 target not installed");
        return;
    }

    let example_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/tool-echo");

    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-wasip2"])
        .current_dir(&example_dir)
        .status()
        .expect("failed to invoke cargo build for tool-echo example");
    assert!(
        status.success(),
        "tool-echo failed to build for wasm32-wasip2"
    );

    let wasm_path = example_dir
        .join("target/wasm32-wasip2/debug/tool_echo.wasm")
        .canonicalize()
        .expect("tool-echo.wasm artifact not found after build");

    let workdir = tempfile::tempdir().expect("tempdir");
    let plugin_dir = workdir.path().join("plugins/echo");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::copy(&wasm_path, plugin_dir.join("echo.wasm")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        r#"
name = "echo"
version = "0.1.0"
description = "spike: tool-echo round trip"
wasm_path = "echo.wasm"
capabilities = ["tool"]
"#,
    )
    .unwrap();

    let host = zeroclaw_plugins::host::PluginHost::new(workdir.path()).expect("PluginHost::new");
    let tool = host
        .instantiate_tool_plugin("echo")
        .await
        .expect("instantiate_tool_plugin");

    assert_eq!(zeroclaw_api::tool::Tool::name(&*tool), "echo");

    let result = zeroclaw_api::tool::Tool::execute(&*tool, serde_json::json!("hello from host"))
        .await
        .expect("execute");
    assert!(result.success);
    assert_eq!(result.output, "\"hello from host\"");
}
