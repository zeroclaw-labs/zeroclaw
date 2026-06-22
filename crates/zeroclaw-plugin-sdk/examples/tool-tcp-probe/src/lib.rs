//! Test fixture for the host-socket-level permission matrix: attempts a raw
//! `std::net::TcpStream` connect to whatever `host:port` it's given and
//! reports success or failure as the tool result, rather than treating a
//! denied connection as a guest-side error. `wasm32-wasip2`'s `std::net` is
//! backed directly by `wasi:sockets`, so this exercises the real enforcement
//! path (`socket_addr_check` in `PluginStore::with_permissions`) even though
//! the `tool-plugin` WIT world this crate exports never declares a
//! `wasi:sockets` import.

use std::net::TcpStream;
use std::time::Duration;

use zeroclaw_plugin_sdk::tool::{ToolMetadata, ToolPlugin, ToolResult, ToolResultExt};

struct TcpProbe;

impl ToolPlugin for TcpProbe {
    fn metadata() -> ToolMetadata {
        ToolMetadata::new(
            "tcp-probe",
            "Attempts a raw TCP connect to the given `host:port` and reports success or failure.",
        )
        .parameters_schema(r#"{"type":"string"}"#)
    }

    fn plugin_info() -> (&'static str, &'static str) {
        ("tool-tcp-probe", "0.1.0")
    }

    fn execute(args: String) -> Result<ToolResult, String> {
        let addr = args.trim_matches('"');
        let socket_addr = addr
            .parse()
            .map_err(|e| format!("invalid socket address {addr:?}: {e}"))?;
        match TcpStream::connect_timeout(&socket_addr, Duration::from_secs(2)) {
            Ok(_) => Ok(ToolResult::ok("connected")),
            Err(e) => Ok(ToolResult::err(format!("connect failed: {e}"))),
        }
    }
}

zeroclaw_plugin_sdk::export_tool!(TcpProbe);
