//! Minimal tool component that performs real `wasi:http` egress, used by the
//! host's plugin egress-policy integration tests (ADR-013 gate G2).
//!
//! `execute` takes `{"url": "...", "follow": <bool>}` and issues **one**
//! outbound request. When `follow` is set and the first response carries a
//! `location` header, it issues a **second, independent** request to that
//! location. The host never follows redirects on a guest's behalf, so this is
//! the only way a redirect gets chased — and it is exactly the shape the
//! second-hop denial test needs.
//!
//! Output is a flat text line the host test asserts on:
//! `hop1:status=200` / `hop1:error=<text>` / `hop1:status=302 hop2:error=<text>`.

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "tool-plugin",
        features: ["plugins-wit-v0"],
    });

    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use exports::zeroclaw::plugin::tool::{Guest as Tool, ToolResult};

    use wasi::http::outgoing_handler;
    use wasi::http::types::{Fields, Method, OutgoingBody, OutgoingRequest, Scheme};

    struct EgressFixture;

    impl PluginInfo for EgressFixture {
        fn plugin_name() -> String {
            "egress-fixture".to_string()
        }
        fn plugin_version() -> String {
            "0.0.0".to_string()
        }
    }

    impl Tool for EgressFixture {
        fn name() -> String {
            "egress_probe".to_string()
        }

        fn description() -> String {
            "Issues one outbound HTTP request and reports the status or the host's error"
                .to_string()
        }

        fn parameters_schema() -> String {
            r#"{"type":"object","properties":{"url":{"type":"string"},"follow":{"type":"boolean"}},"required":["url"]}"#
                .to_string()
        }

        fn execute(args: String) -> Result<ToolResult, String> {
            let url = match extract_string(&args, "url") {
                Some(url) => url,
                None => return Err("missing \"url\" argument".to_string()),
            };
            let follow = args.contains("\"follow\":true") || args.contains("\"follow\": true");

            let (first, location) = request(&url);
            let mut report = format!("hop1:{first}");

            if follow {
                match location {
                    Some(next) => {
                        let (second, _) = request(&next);
                        report.push_str(&format!(" hop2:{second}"));
                    }
                    None => report.push_str(" hop2:skipped-no-location"),
                }
            }

            Ok(ToolResult {
                success: true,
                output: report,
                error: None,
            })
        }
    }

    /// Issue exactly one `wasi:http` request. Returns the outcome text plus the
    /// `location` header when the response carried one.
    fn request(url: &str) -> (String, Option<String>) {
        let Some((scheme, rest)) = split_scheme(url) else {
            return ("error=unparsable-url".to_string(), None);
        };
        let (authority, path) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, "/"),
        };

        let outgoing = OutgoingRequest::new(Fields::new());
        if outgoing.set_method(&Method::Get).is_err()
            || outgoing.set_scheme(Some(&scheme)).is_err()
            || outgoing.set_authority(Some(authority)).is_err()
            || outgoing.set_path_with_query(Some(path)).is_err()
        {
            return ("error=request-setup-failed".to_string(), None);
        }

        // The body handle has to be taken before `handle` consumes the request.
        let Ok(body) = outgoing.body() else {
            return ("error=body-unavailable".to_string(), None);
        };

        let future = match outgoing_handler::handle(outgoing, None) {
            Ok(future) => future,
            // The host's policy denial surfaces here, synchronously.
            Err(code) => return (format!("error={code:?}"), None),
        };
        let _ = OutgoingBody::finish(body, None);

        future.subscribe().block();
        let outcome = match future.get() {
            Some(Ok(Ok(response))) => {
                let status = response.status();
                let location = response
                    .headers()
                    .get(&"location".to_string())
                    .into_iter()
                    .next()
                    .and_then(|v| String::from_utf8(v).ok());
                return (format!("status={status}"), location);
            }
            // A denial raised after the handler accepted the request lands here.
            Some(Ok(Err(code))) => format!("error={code:?}"),
            Some(Err(())) => "error=future-already-taken".to_string(),
            None => "error=future-not-ready".to_string(),
        };
        (outcome, None)
    }

    fn split_scheme(url: &str) -> Option<(Scheme, &str)> {
        if let Some(rest) = url.strip_prefix("http://") {
            Some((Scheme::Http, rest))
        } else {
            url.strip_prefix("https://")
                .map(|rest| (Scheme::Https, rest))
        }
    }

    /// Pull `"key":"value"` out of a flat JSON object. The host test controls
    /// the exact argument text, so a full JSON parser would only add a
    /// dependency without adding coverage.
    fn extract_string(json: &str, key: &str) -> Option<String> {
        let needle = format!("\"{key}\"");
        let after_key = &json[json.find(&needle)? + needle.len()..];
        let after_colon = &after_key[after_key.find(':')? + 1..];
        let open = after_colon.find('"')?;
        let value = &after_colon[open + 1..];
        let close = value.find('"')?;
        Some(value[..close].to_string())
    }

    export!(EgressFixture);
}
