//! Tool component that exercises the host's typed socket resource.
//!
//! Its typed config names one destination and a connect mode:
//!
//! * `plaintext` and `tls` send `ping` and return the first echoed chunk.
//! * `starttls` sends `STARTTLS\r\n` as negotiation traffic, waits for the
//!   peer's `OK`, upgrades in place, then sends `ping` as application traffic.
//!
//! An optional `profile` selects a host-configured TLS profile. Any socket
//! failure is returned in its `Debug` form, which carries the WIT case name, so
//! a host test can assert the exact category (`access-denied`, ...).

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0", "plugins-wit-v0-sockets"],
    });

    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};
    use zeroclaw::plugin::sockets::{
        ConnectMode, ConnectRequest, Connection, ReceiveEvent, SocketError, connect,
    };

    /// Receive polls before giving up. Each idle poll yields to the host.
    const RECEIVE_POLLS: usize = 5_000;

    struct SocketFixtureTool;

    impl PluginInfo for SocketFixtureTool {
        fn plugin_name() -> String {
            "tool-socket-fixture".to_string()
        }

        fn plugin_version() -> String {
            "0.0.0".to_string()
        }
    }

    fn failure(error: SocketError) -> String {
        format!("{error:?}")
    }

    fn first_chunk(
        receive: impl Fn() -> Result<ReceiveEvent, SocketError>,
    ) -> Result<Vec<u8>, String> {
        for _ in 0..RECEIVE_POLLS {
            match receive().map_err(failure)? {
                ReceiveEvent::Data(bytes) => return Ok(bytes),
                ReceiveEvent::Idle => {}
                ReceiveEvent::Closed(reason) => return Err(format!("closed:{reason:?}")),
            }
        }
        Err("receive-timeout".to_string())
    }

    fn round_trip(connection: &Connection) -> Result<String, String> {
        connection.send(b"ping").map_err(failure)?;
        let bytes = first_chunk(|| connection.receive())?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    impl Tool for SocketFixtureTool {
        fn name() -> String {
            "socket-round-trip".to_string()
        }

        fn description() -> String {
            "Exercises the typed socket resource".to_string()
        }

        fn parameters_schema() -> String {
            r#"{"type":"object"}"#.to_string()
        }

        fn execute(args: String) -> Result<ToolResult, String> {
            let args: serde_json::Value =
                serde_json::from_str(&args).map_err(|_| "args-invalid".to_string())?;
            let config = args
                .get("__config")
                .ok_or_else(|| "config-missing".to_string())?;
            let text = |key: &str| config.get(key).and_then(serde_json::Value::as_str);
            let host = text("host").ok_or_else(|| "host-missing".to_string())?;
            let port = config
                .get("port")
                .and_then(serde_json::Value::as_u64)
                .and_then(|port| u16::try_from(port).ok())
                .ok_or_else(|| "port-invalid".to_string())?;
            let mode = match text("mode").unwrap_or("plaintext") {
                "plaintext" => ConnectMode::Plaintext,
                "tls" => ConnectMode::DirectTls,
                "starttls" => ConnectMode::StartTls,
                _ => return Err("mode-invalid".to_string()),
            };
            let connection = connect(&ConnectRequest {
                host: host.to_string(),
                port,
                mode,
                tls_profile: text("profile").map(str::to_string),
            })
            .map_err(failure)?;

            if mode == ConnectMode::StartTls {
                connection
                    .send_negotiation(b"STARTTLS\r\n")
                    .map_err(failure)?;
                let reply = first_chunk(|| connection.receive_negotiation())?;
                if reply != b"OK\r\n" {
                    return Err("starttls-refused".to_string());
                }
                // Application traffic is refused until the upgrade completes.
                if connection.send(b"too-early") != Err(SocketError::InvalidState) {
                    return Err("application-before-upgrade".to_string());
                }
                connection.upgrade_tls().map_err(failure)?;
            }
            let output = round_trip(&connection)?;
            Ok(ToolResult {
                success: true,
                output,
                error: None,
            })
        }
    }

    export!(SocketFixtureTool);
}
