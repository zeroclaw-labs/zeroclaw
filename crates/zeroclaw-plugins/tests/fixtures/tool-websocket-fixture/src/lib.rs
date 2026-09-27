//! Tool component that exercises the host's WebSocket resource.
//!
//! Its typed config names a `url` (`ws://` or `wss://`) and an optional TLS
//! `profile`. It sends one text `ping` and returns the first text message the
//! peer sends back. Any failure is returned in its `Debug` form, which carries
//! the WIT case name, so a host test can assert the exact category.

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0", "plugins-wit-v0-websocket"],
    });

    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};
    use zeroclaw::plugin::websocket::{ConnectOptions, Event, Message, connect};

    /// Receive polls before giving up. Each empty poll yields to the host.
    const RECEIVE_POLLS: usize = 5_000;

    struct WebSocketFixtureTool;

    impl PluginInfo for WebSocketFixtureTool {
        fn plugin_name() -> String {
            "tool-websocket-fixture".to_string()
        }

        fn plugin_version() -> String {
            "0.0.0".to_string()
        }
    }

    impl Tool for WebSocketFixtureTool {
        fn name() -> String {
            "websocket-round-trip".to_string()
        }

        fn description() -> String {
            "Exercises the WebSocket resource".to_string()
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
            let url = text("url").ok_or_else(|| "url-missing".to_string())?;
            let connection = connect(&ConnectOptions {
                url: url.to_string(),
                headers: Vec::new(),
                subprotocols: Vec::new(),
                tls_profile: text("profile").map(str::to_string),
            })
            .map_err(|error| format!("{error:?}"))?;
            connection
                .send(&Message::Text("ping".to_string()))
                .map_err(|error| format!("{error:?}"))?;
            for _ in 0..RECEIVE_POLLS {
                match connection.receive().map_err(|error| format!("{error:?}"))? {
                    Some(Event::Message(Message::Text(reply))) => {
                        return Ok(ToolResult {
                            success: true,
                            output: reply,
                            error: None,
                        });
                    }
                    Some(Event::Message(Message::Binary(_))) | None => {}
                    Some(Event::Closed(_)) => return Err("closed".to_string()),
                    Some(Event::Failed(error)) => return Err(format!("{error:?}")),
                }
            }
            Err("receive-timeout".to_string())
        }
    }

    export!(WebSocketFixtureTool);
}
