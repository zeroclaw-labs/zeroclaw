//! Host-socket-level regression test for the `Http`/`Tcp` permission split
//! in `PluginStore::with_permissions`
//! (`crates/zeroclaw-plugins/src/component/v0/plugin_store.rs`):
//!
//! - An `Http`-only grant must NOT unlock raw `wasi:sockets` TCP connect to
//!   that same host — `wasi:http` outbound requests are fully intercepted
//!   by `PluginHttpHooks::send_request` and never reach the socket layer,
//!   so granting HTTP access must not also grant a raw-socket capability.
//! - A `Tcp` grant for that host must allow the raw connect.
//!
//! Uses `examples/tool-tcp-probe`, which calls `std::net::TcpStream::connect`
//! directly — `wasm32-wasip2`'s `std::net` is backed by `wasi:sockets`, so
//! this exercises the real `socket_addr_check` enforcement path rather than
//! a logic-level stand-in.

mod common;

use std::path::Path;

#[tokio::test]
async fn http_only_permission_denies_raw_tcp_connect() {
    if !common::wasm32_wasip2_installed() {
        eprintln!("skipping: wasm32-wasip2 target not installed");
        return;
    }

    let (addr, _listener) = spawn_loopback_listener().await;
    let result = run_tcp_probe("http", addr).await;

    assert!(
        !result.success,
        "an Http-only permission must not allow a raw TCP connect, but it succeeded: {:?}",
        result.output
    );
}

#[tokio::test]
async fn tcp_permission_allows_raw_tcp_connect() {
    if !common::wasm32_wasip2_installed() {
        eprintln!("skipping: wasm32-wasip2 target not installed");
        return;
    }

    let (addr, _listener) = spawn_loopback_listener().await;
    let result = run_tcp_probe("tcp", addr).await;

    assert!(
        result.success,
        "a Tcp permission for the target host must allow a raw TCP connect, but it was denied: {:?}",
        result.error
    );
}

/// Binds a loopback listener and spawns a background task that accepts (and
/// immediately drops) connections, so a successful raw connect from the
/// guest completes its handshake without hanging.
async fn spawn_loopback_listener() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback listener");
    let addr = listener.local_addr().expect("local_addr");
    let handle = zeroclaw_spawn::spawn!(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => drop(stream),
                Err(_) => break,
            }
        }
    });
    (addr, handle)
}

/// Instantiates `tool-tcp-probe` with a manifest granting only
/// `fine_grained_permission_type` (`"http"` or `"tcp"`) for `127.0.0.1`, then
/// asks it to connect to `addr`.
async fn run_tcp_probe(
    fine_grained_permission_type: &str,
    addr: std::net::SocketAddr,
) -> zeroclaw_api::tool::ToolResult {
    let example_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/tool-tcp-probe");
    let wasm_path = common::build_example(&example_dir, "tool_tcp_probe");

    let workdir = tempfile::tempdir().expect("tempdir");
    let plugin_dir = workdir.path().join("plugins/tcp-probe");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::copy(&wasm_path, plugin_dir.join("probe.wasm")).unwrap();
    std::fs::write(
        plugin_dir.join("manifest.toml"),
        format!(
            r#"
name = "tcp-probe"
version = "0.1.0"
description = "spike: Http-vs-Tcp permission matrix"
wasm_path = "probe.wasm"
capabilities = ["tool"]

[[fine_grained_permissions]]
type = "{fine_grained_permission_type}"
value = "127.0.0.1"
"#
        ),
    )
    .unwrap();

    let host = zeroclaw_plugins::host::PluginHost::new(workdir.path()).expect("PluginHost::new");
    let tool = host
        .instantiate_tool_plugin("tcp-probe")
        .await
        .expect("instantiate_tool_plugin");

    zeroclaw_api::tool::Tool::execute(&*tool, serde_json::json!(addr.to_string()))
        .await
        .expect("execute")
}
