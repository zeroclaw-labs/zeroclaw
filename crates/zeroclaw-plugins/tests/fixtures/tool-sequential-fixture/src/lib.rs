//! Tool component that issues several **sequential** outbound requests from a
//! single `execute` — the call shape a bug report described as intermittently
//! failing on its later calls.
//!
//! `execute` takes `{"url": "...", "count": N, "hold": <bool>, "discard":
//! <bool>}` and POSTs to `url` N times in a row through `waki`'s blocking
//! client — the same client the report used.
//!
//! The two flags select what the guest does with each response before the next
//! request starts, and they are three different demands on the host, not one:
//!
//! * neither — each response is read to the end and dropped, so at most one is
//!   alive at a time and every connection finishes cleanly.
//! * `discard` — each response is dropped unread, so the connection is torn
//!   down rather than drained. This is the report's retry shape.
//! * `hold` — every response is kept alive until the last request has been
//!   issued, and only then are the bodies read. A `waki::Response` owns the
//!   `wasi:http` incoming-body resource, so holding it holds whatever the host
//!   attached to that body.
//!
//! Output is a flat line the host test asserts on:
//! `calls=<n> ok=<k>`, plus `first_error=<index>:<text>` when one failed.

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0"],
    });

    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};

    struct SequentialTool;

    impl PluginInfo for SequentialTool {
        fn plugin_name() -> String {
            "tool-sequential-fixture".to_string()
        }

        fn plugin_version() -> String {
            "0.0.0".to_string()
        }
    }

    impl Tool for SequentialTool {
        fn name() -> String {
            "sequential_probe".to_string()
        }

        fn description() -> String {
            "Issues N sequential outbound HTTP requests and reports how many succeeded".to_string()
        }

        fn parameters_schema() -> String {
            r#"{"type":"object","properties":{"url":{"type":"string"},"count":{"type":"integer"},"hold":{"type":"boolean"},"discard":{"type":"boolean"}},"required":["url","count"]}"#
                .to_string()
        }

        fn execute(args: String) -> Result<ToolResult, String> {
            let args: serde_json::Value =
                serde_json::from_str(&args).map_err(|error| error.to_string())?;
            let url = args["url"].as_str().ok_or("missing url")?;
            let count = args["count"].as_u64().ok_or("missing count")?;
            let hold = args["hold"].as_bool().unwrap_or(false);
            let discard = args["discard"].as_bool().unwrap_or(false);

            let mut ok = 0_u64;
            let mut first_error: Option<String> = None;
            let mut held: Vec<waki::Response> = Vec::new();

            for index in 1..=count {
                // A body on every call: the report's plugin was POSTing JSON-RPC,
                // and a request body is written through a second guest resource
                // after the handler has already been handed the request.
                let payload = format!(r#"{{"jsonrpc":"2.0","id":{index},"method":"probe"}}"#);
                let sent = waki::Client::new()
                    .post(url)
                    .headers([("content-type", "application/json")])
                    .body(payload.into_bytes())
                    .send();

                let response = match sent {
                    Ok(response) => response,
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(format!("{index}:{error}"));
                        }
                        continue;
                    }
                };

                if response.status_code() != 200 {
                    if first_error.is_none() {
                        first_error = Some(format!("{index}:status={}", response.status_code()));
                    }
                    continue;
                }

                if hold {
                    // Keep the response — and with it the incoming-body resource
                    // — alive past the next request.
                    held.push(response);
                    ok += 1;
                    continue;
                }

                if discard {
                    // The retry shape from the report: the caller looked at the
                    // response and threw it away without reading the body, so
                    // the connection is torn down rather than drained.
                    drop(response);
                    ok += 1;
                    continue;
                }

                match response.body() {
                    Ok(_) => ok += 1,
                    Err(error) => {
                        if first_error.is_none() {
                            first_error = Some(format!("{index}:body:{error}"));
                        }
                    }
                }
            }

            // Drained only now, so that in `hold` mode every request above ran
            // with all of its predecessors' responses still open.
            for (offset, response) in held.into_iter().enumerate() {
                if let Err(error) = response.body() {
                    if first_error.is_none() {
                        first_error = Some(format!("{}:held-body:{error}", offset + 1));
                    }
                    ok = ok.saturating_sub(1);
                }
            }

            let mut report = format!("calls={count} ok={ok}");
            if let Some(error) = first_error {
                report.push_str(&format!(" first_error={error}"));
            }

            Ok(ToolResult {
                success: true,
                output: report,
                error: None,
            })
        }
    }

    export!(SequentialTool);
}
