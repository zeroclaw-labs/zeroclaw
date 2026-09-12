//! Tool component that exercises real WASI HTTP waits at the host boundary.

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0"],
    });

    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};
    use waki::bindings::wasi::http::{
        outgoing_handler,
        types::{ErrorCode, Fields, OutgoingBody, OutgoingRequest, RequestOptions, Scheme},
    };
    use zeroclaw::plugin::logging::{LogLevel, PluginAction, PluginEvent, PluginOutcome};

    /// Stable classification the host test asserts on when the guest's own
    /// first-byte deadline fires, so the assertion cannot be satisfied by an
    /// unrelated pre-request or transport error.
    const GUEST_FIRST_BYTE_TIMEOUT: &str = "guest-first-byte-timeout";

    struct TimeoutTool;

    impl PluginInfo for TimeoutTool {
        fn plugin_name() -> String {
            "tool-timeout-fixture".to_string()
        }

        fn plugin_version() -> String {
            "0.0.0".to_string()
        }
    }

    impl Tool for TimeoutTool {
        fn name() -> String {
            "timeout-fixture".to_string()
        }

        fn description() -> String {
            "exercise host plugin deadlines".to_string()
        }

        fn parameters_schema() -> String {
            r#"{"type":"object","properties":{"mode":{"type":"string"},"url":{"type":"string"},"guest_timeout_ms":{"type":"integer"}},"required":["mode"]}"#.to_string()
        }

        fn execute(args: String) -> Result<ToolResult, String> {
            let args: serde_json::Value =
                serde_json::from_str(&args).map_err(|error| error.to_string())?;
            let mode = args["mode"].as_str().unwrap_or_default();
            let output = match mode {
                "http" => {
                    let url = args["url"].as_str().ok_or("missing url")?;
                    let body = waki::Client::new()
                        .get(url)
                        .send()
                        .map_err(|error| error.to_string())?
                        .body()
                        .map_err(|error| error.to_string())?;
                    format!("{} bytes", body.len())
                }
                "raw-first-byte" => {
                    let url = args["url"].as_str().ok_or("missing url")?;
                    let timeout_ms = args["guest_timeout_ms"].as_u64().unwrap_or(100);
                    raw_first_byte_request(url, timeout_ms)?;
                    "response arrived".to_string()
                }
                "spin" => {
                    let mut value = 0_u64;
                    loop {
                        value = std::hint::black_box(value.wrapping_add(1));
                    }
                }
                "log" => {
                    let message = args["message"].as_str().unwrap_or("plugin log");
                    let count = args["count"].as_u64().unwrap_or(1);
                    for _ in 0..count {
                        log_record_note(message);
                    }
                    "logged".to_string()
                }
                _ => return Err("unknown mode".to_string()),
            };
            Ok(ToolResult {
                success: true,
                output,
                error: None,
            })
        }
    }

    /// Emit one INFO record through the imported host `logging` interface so
    /// the host tests can exercise the `log-record` boundary end to end.
    fn log_record_note(message: &str) {
        zeroclaw::plugin::logging::log_record(
            LogLevel::Info,
            &PluginEvent {
                function_name: "zeroclaw_tool_timeout_fixture::execute".to_string(),
                action: PluginAction::Note,
                outcome: Some(PluginOutcome::Success),
                duration_ms: None,
                attrs: None,
                message: message.to_string(),
            },
        );
    }

    fn raw_first_byte_request(url: &str, timeout_ms: u64) -> Result<(), String> {
        let target = url
            .strip_prefix("http://")
            .ok_or("fixture only supports http:// URLs")?;
        let (authority, path) = match target.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (target, "/".to_string()),
        };

        let request = OutgoingRequest::new(Fields::new());
        request
            .set_scheme(Some(&Scheme::Http))
            .map_err(|()| "set scheme failed")?;
        request
            .set_authority(Some(authority))
            .map_err(|()| "set authority failed")?;
        request
            .set_path_with_query(Some(&path))
            .map_err(|()| "set path failed")?;
        let body = request.body().map_err(|()| "request body failed")?;

        let options = RequestOptions::new();
        options
            .set_first_byte_timeout(Some(timeout_ms.saturating_mul(1_000_000)))
            .map_err(|()| "first-byte timeout unsupported")?;
        let response = outgoing_handler::handle(request, Some(options))
            .map_err(|error| format!("request rejected: {error:?}"))?;
        OutgoingBody::finish(body, None)
            .map_err(|error| format!("finish body failed: {error:?}"))?;

        let pollable = response.subscribe();
        pollable.block();
        let response = response
            .get()
            .ok_or("response was not ready")?
            .map_err(|()| "response already taken")?
            .map_err(|error| match error {
                // The wasi:http mapping of an elapsed first-byte deadline.
                // Surfaced under a fixture-stable name so host assertions do
                // not depend on the bindings' Debug formatting.
                ErrorCode::ConnectionReadTimeout => GUEST_FIRST_BYTE_TIMEOUT.to_string(),
                other => format!("request failed: {other:?}"),
            })?;
        drop(response);
        Ok(())
    }

    export!(TimeoutTool);
}
